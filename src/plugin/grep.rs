//! grep.rs
//!
//! `grep` 工具：按正则逐行搜文件内容，返回**命中行与行号**。
//!
//! 与 `search_in_folder` 的分工：后者用 Everything 索引回答「哪些文件的正文里
//! 出现过这个词」，但只到**文件**粒度（返回路径）；本工具在候选文件里逐行
//! 匹配，给出 `行号 + 正文`。候选选择仍然交给 Everything 索引 —— 这是本项目的
//! 差异化优势（跨全盘按名字 / 时间 / 扩展名秒级锁定候选），逐行匹配才自己做。
//!
//! 两道闸门防止「一次 grep 读穿整个盘」：
//!   - 候选文件数上限 [`MAX_CANDIDATE_FILES`]（Everything 一次最多给这么多）；
//!   - 读取字节总量上限 [`MAX_TOTAL_BYTES`]（2000 × 8 MiB 最坏是 16 GB）。
//! 两道闸门都触发时都会把 `truncated` 置真，并如实报出 `candidates` /
//! `files_scanned` / `bytes_scanned`，让调用方知道是卡在哪一道上。

use regex::{Regex, RegexBuilder};

use super::read;
use super::search::{search_in_folder, SearchOptions, SearchScope, SortKey};

/// 默认最多返回多少条命中（content 模式）或多少个文件（其余模式）。
pub const DEFAULT_HEAD_LIMIT: usize = 200;
/// `head_limit` 上限。
pub const MAX_HEAD_LIMIT: usize = 2000;
/// 候选文件数上限 —— 超过就只处理前这么多。
pub const MAX_CANDIDATE_FILES: usize = 2000;
/// 单次 grep 读取的字节总量上限。
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// 等待 Everything 完成候选查询的默认超时。
pub const DEFAULT_TIMEOUT_MS: u32 = 10_000;

/// 输出模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// 命中行 + 行号（默认）。
    Content,
    /// 只列有命中的文件路径。
    FilesWithMatches,
    /// 每个文件的命中次数。
    Count,
}

impl OutputMode {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputMode::Content => "content",
            OutputMode::FilesWithMatches => "filesWithMatches",
            OutputMode::Count => "count",
        }
    }

    /// 解析输出模式，顺带收几个 LLM 常写的同义字。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['_', '-'], "").as_str() {
            "" | "content" | "lines" | "matches" => Some(OutputMode::Content),
            "fileswithmatches" | "files" | "filenames" => Some(OutputMode::FilesWithMatches),
            "count" | "counts" => Some(OutputMode::Count),
            _ => None,
        }
    }
}

/// 一条命中行。
#[derive(Debug, PartialEq)]
pub struct Hit {
    pub path: String,
    /// 1 起的行号。
    pub line: usize,
    /// 命中行正文（超长已按 [`read::MAX_LINE_CHARS`] 裁剪）。
    pub text: String,
}

/// 结果载荷，形状由 `mode` 决定。
#[derive(Debug)]
pub enum GrepPayload {
    Content(Vec<Hit>),
    Files(Vec<String>),
    Counts(Vec<(String, usize)>),
}

impl GrepPayload {
    pub fn len(&self) -> usize {
        match self {
            GrepPayload::Content(v) => v.len(),
            GrepPayload::Files(v) => v.len(),
            GrepPayload::Counts(v) => v.len(),
        }
    }
}

/// 一次 grep 的结果。
#[derive(Debug)]
pub struct GrepOutcome {
    pub mode: OutputMode,
    pub head_limit: usize,
    /// Everything 选出的候选文件总数（截断前）。
    pub candidates: usize,
    /// 候选超过 [`MAX_CANDIDATE_FILES`]，只处理了前一部分。
    pub candidates_truncated: bool,
    /// 实际读了内容的文件数（跳过二进制 / 超大 / 命中黑名单 / 读不了的）。
    pub files_scanned: usize,
    /// 实际读了的总字节数。
    pub bytes_scanned: u64,
    /// 有命中的文件数。
    pub files_with_matches: usize,
    /// 被裁短的行数。
    pub clipped_lines: usize,
    /// 还有命中没返回（`head_limit` 或字节预算拦下）。
    pub truncated: bool,
    pub payload: GrepPayload,
}

/// 在已解码的文本里逐行匹配。纯函数，便于单测。
///
/// content 模式下把命中追加进 `out`；返回 (本文件命中数, 裁短行数, 是否触顶)。
fn scan_text(
    re: &Regex,
    path: &str,
    text: &str,
    mode: OutputMode,
    head_limit: usize,
    out: &mut Vec<Hit>,
) -> (usize, usize, bool) {
    let mut file_hits = 0usize;
    let mut clipped = 0usize;
    for (idx, line) in text.lines().enumerate() {
        if !re.is_match(line) {
            continue;
        }
        file_hits += 1;
        if mode != OutputMode::Content {
            continue;
        }
        let (piece, was_clipped) = read::clip_line(line);
        if was_clipped {
            clipped += 1;
        }
        out.push(Hit {
            path: path.to_string(),
            line: idx + 1,
            text: piece,
        });
        if out.len() >= head_limit {
            return (file_hits, clipped, true);
        }
    }
    (file_hits, clipped, false)
}

/// 预校验正则。
///
/// `pub` 是为了让 MCP 层在触达 Everything 之前先跑一遍，把「正则写错」这种
/// 纯入参问题报成 INVALID_PARAMS —— 与 `read::validate_path` 的处理一致。
/// [`grep`] 内部也会再编译一次（幂等，代价可忽略）。
pub fn validate_regex(pattern: &str) -> Result<(), String> {
    RegexBuilder::new(pattern)
        .build()
        .map(|_| ())
        .map_err(|e| format!("invalid regex in 'pattern': {}", e))
}

/// 执行一次 grep。
///
/// `filter` 是交给 Everything 的候选筛选串（与 `search_in_folder` 的 `pattern`
/// 同一套语法，如 `ext:rs;toml`、`dm:lastweek`、`!\target\`），空串表示不筛 ——
/// **不筛会读很多文件**，两道闸门就是为这种情况准备的。
#[allow(clippy::too_many_arguments)]
pub fn grep(
    folder: &str,
    pattern: &str,
    filter: &str,
    mode: OutputMode,
    head_limit: usize,
    case_insensitive: bool,
    timeout_ms: u32,
) -> Result<GrepOutcome, String> {
    if pattern.trim().is_empty() {
        return Err("'pattern' must not be empty".into());
    }
    let re = RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|e| format!("invalid regex in 'pattern': {}", e))?;

    // 候选：递归子树 + filter，按修改时间倒序 —— 最近改动的文件先出结果，
    // 这与 agent 的直觉一致（刚改过的代码最可能是要找的）。
    let options = SearchOptions {
        sort: SortKey::DateModified,
        descending: true,
        ..SearchOptions::for_scope(SearchScope::Recursive)
    };
    let outcome = search_in_folder(
        folder,
        filter,
        0,
        MAX_CANDIDATE_FILES,
        timeout_ms,
        options,
    )?;
    let candidates = outcome.total;
    let candidates_truncated = outcome.total > outcome.results.len();

    let mut hits: Vec<Hit> = Vec::new();
    let mut per_file: Vec<(String, usize)> = Vec::new();
    let mut files_scanned = 0usize;
    let mut bytes_scanned = 0u64;
    let mut clipped_lines = 0usize;
    let mut truncated = false;

    'files: for r in &outcome.results {
        if r.is_folder {
            continue;
        }
        // 到量就停：content 模式按命中行数算，其余模式按文件数算。
        let listed = match mode {
            OutputMode::Content => hits.len(),
            _ => per_file.len(),
        };
        if listed >= head_limit {
            truncated = true;
            break;
        }
        if bytes_scanned >= MAX_TOTAL_BYTES {
            truncated = true;
            break;
        }
        // 二进制 / 超大 / 黑名单 / 读不了 —— 跳过这个文件，不中断整次搜索。
        let Some(text) = read::read_text_for_matching(&r.path) else {
            continue;
        };
        files_scanned += 1;
        bytes_scanned += text.len() as u64;

        let (file_hits, clipped, hit_limit) =
            scan_text(&re, &r.path, &text, mode, head_limit, &mut hits);
        clipped_lines += clipped;
        if file_hits > 0 {
            per_file.push((r.path.clone(), file_hits));
        }
        if hit_limit {
            truncated = true;
            break 'files;
        }
        if mode != OutputMode::Content && per_file.len() >= head_limit {
            truncated = true;
            break;
        }
    }

    let files_with_matches = per_file.len();
    let payload = match mode {
        OutputMode::Content => GrepPayload::Content(hits),
        OutputMode::FilesWithMatches => {
            GrepPayload::Files(per_file.iter().map(|(p, _)| p.clone()).collect())
        }
        OutputMode::Count => GrepPayload::Counts(per_file),
    };

    Ok(GrepOutcome {
        mode,
        head_limit,
        candidates,
        candidates_truncated,
        files_scanned,
        bytes_scanned,
        files_with_matches,
        clipped_lines,
        truncated,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re(p: &str) -> Regex {
        RegexBuilder::new(p).build().unwrap()
    }

    fn re_i(p: &str) -> Regex {
        RegexBuilder::new(p).case_insensitive(true).build().unwrap()
    }

    #[test]
    fn output_mode_parses_synonyms_and_rejects_junk() {
        assert_eq!(OutputMode::parse(""), Some(OutputMode::Content));
        assert_eq!(OutputMode::parse("content"), Some(OutputMode::Content));
        assert_eq!(OutputMode::parse("LINES"), Some(OutputMode::Content));
        assert_eq!(
            OutputMode::parse("filesWithMatches"),
            Some(OutputMode::FilesWithMatches)
        );
        assert_eq!(
            OutputMode::parse("files_with_matches"),
            Some(OutputMode::FilesWithMatches)
        );
        assert_eq!(OutputMode::parse("files"), Some(OutputMode::FilesWithMatches));
        assert_eq!(OutputMode::parse("count"), Some(OutputMode::Count));
        assert_eq!(OutputMode::parse("summary"), None);
    }

    #[test]
    fn scan_text_reports_line_numbers_and_all_hits() {
        let text = "alpha\nbeta TARGET\ngamma\ntarget again";
        let mut out = Vec::new();
        let (hits, clipped, limit) = scan_text(
            &re_i("target"),
            "D:\\a.txt",
            text,
            OutputMode::Content,
            10,
            &mut out,
        );
        assert_eq!((hits, clipped, limit), (2, 0, false));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].line, 2, "行号是 1 起的");
        assert_eq!(out[0].text, "beta TARGET");
        assert_eq!(out[1].line, 4);
        assert_eq!(out[1].text, "target again");

        // 默认区分大小写：同一条文本用大小写敏感版只命中一行
        let mut sensitive = Vec::new();
        let (hits_s, _, _) = scan_text(
            &re("target"),
            "D:\\a.txt",
            text,
            OutputMode::Content,
            10,
            &mut sensitive,
        );
        assert_eq!(hits_s, 1);
        assert_eq!(sensitive[0].line, 4);
    }

    #[test]
    fn scan_text_stops_at_head_limit() {
        let text = (1..=10).map(|i| format!("hit {i}")).collect::<Vec<_>>().join("\n");
        let mut out = Vec::new();
        let (hits, _, limit) = scan_text(&re("hit"), "p", &text, OutputMode::Content, 3, &mut out);
        assert!(limit, "触顶要报出来");
        assert_eq!(out.len(), 3);
        // 触顶时本文件的命中数是「数到触顶为止」，不是全文件的真实命中数 ——
        // 这是有意的：真实总数已经超出调用方要的量了。
        assert_eq!(hits, 3);
    }

    #[test]
    fn scan_text_counts_all_hits_in_non_content_modes() {
        let text = "hit\nnope\nhit\nhit";
        let mut out = Vec::new();
        let (hits, _, limit) = scan_text(&re("hit"), "p", &text, OutputMode::Count, 2, &mut out);
        // count 模式不产出 Hit，也不受 head_limit 影响（上限按文件数算）
        assert!(out.is_empty());
        assert_eq!((hits, limit), (3, false));
    }

    #[test]
    fn scan_text_clips_long_matched_lines() {
        let long = format!("prefix {}", "x".repeat(read::MAX_LINE_CHARS + 100));
        let mut out = Vec::new();
        let (_, clipped, _) = scan_text(&re("prefix"), "p", &long, OutputMode::Content, 10, &mut out);
        assert_eq!(clipped, 1);
        assert_eq!(out[0].text.chars().count(), read::MAX_LINE_CHARS);
    }

    #[test]
    fn scan_text_matches_per_line_so_caret_anchors_the_line() {
        let text = "start here\nhere start";
        let mut out = Vec::new();
        scan_text(&re("^start"), "p", text, OutputMode::Content, 10, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line, 1, "每行独立匹配，^ 锚的是行首");
    }

    #[test]
    fn grep_rejects_empty_pattern_and_bad_regex() {
        // 这两条在触达 Everything 之前就该失败
        let e = grep("C:\\", "   ", "", OutputMode::Content, 10, false, 1000).unwrap_err();
        assert!(e.contains("must not be empty"), "{e}");
        let e = grep("C:\\", "a(", "", OutputMode::Content, 10, false, 1000).unwrap_err();
        assert!(e.contains("invalid regex"), "{e}");
    }
}
