//! read.rs
//!
//! `read_file` 工具：读取单个文本文件的内容。
//!
//! 读取用 `std::fs` 直读后自行解码：UTF-16 BOM → UTF-8 BOM → 合法 UTF-8 →
//! 本机 ANSI 代码页 → UTF-8 lossy。响应里的 `encoding` 字段回显判定结果。
//!
//! **为什么不用主程序的 `utf8_basic_string_get_text_plain_file`**（2026-09-23
//! 实机实测，别重复试）：那个接口确实存在、也能调用，但
//!   1. 只按 UTF-8 解码 —— GBK/ANSI 正文经它出来整篇是 U+FFFD 替换字符；
//!   2. 读不了被别的进程独占打开的文件（实测该场景返回 NULL，os error 32）；
//!   3. 也不比我们多覆盖长路径（303 字符的路径直读正常）。
//! 三条加起来它没有一条路比自解码更好，反而要占 Everything 主线程，所以
//! 最终没走它。详情见 docs/PLUGIN_SDK_API_CN.md 的 12.4 节。
//!
//! 三道闸门，顺序是「越早拒绝越省事」：
//!   1. 入参（绝对路径、非通配符）—— 纯形状问题，MCP 层报 INVALID_PARAMS；
//!   2. 敏感路径黑名单（见 [`super::sensitive`]）—— 在 stat 之前就拒，
//!      连文件是否存在都不去碰；
//!   3. 尺寸（8 MiB 文件上限）、二进制（NUL）、行窗口与字节预算。

use std::path::Path;

use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};

use super::sensitive;

/// 单次读取的字节上限。超过直接报错。
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// 单次返回的字节上限。窗口按行累加，碰到它就停下 —— 不会从行中间切开。
pub const MAX_WINDOW_BYTES: usize = 512 * 1024;

/// 单行字符上限。超长行（压缩过的 js、单行 JSON、base64 内联）会被裁到这里，
/// 裁了几行由 `clipped_lines` 上报。取 16384 与主流 agent 客户端一致。
pub const MAX_LINE_CHARS: usize = 16_384;

/// 默认返回的最大行数。
pub const DEFAULT_MAX_LINES: usize = 200;

// ====================================================================
// 错误码 —— 调用方据此决定下一步（换工具 / 换路径 / 缩窗口），
// 不必去解析文案。
// ====================================================================

/// 路径形状不对（相对路径、通配符、控制字符）。
pub const ERR_INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
/// 文件不存在。
pub const ERR_NOT_FOUND: &str = "NOT_FOUND";
/// 路径是目录。
pub const ERR_PATH_IS_DIRECTORY: &str = "PATH_IS_DIRECTORY";
/// 命中敏感路径黑名单。
pub const ERR_PATH_DENIED: &str = "PATH_DENIED";
/// 是二进制内容（含 NUL）。
pub const ERR_BINARY_CONTENT: &str = "BINARY_CONTENT";
/// 超过文件大小上限。
pub const ERR_TOO_LARGE: &str = "TOO_LARGE";
/// 其它读取失败（被独占锁定、权限不足等）。
pub const ERR_READ_FAILED: &str = "READ_FAILED";

/// 读取失败的结构化错误。
#[derive(Debug)]
pub struct ReadError {
    /// 上面那组常量之一。
    pub code: &'static str,
    pub message: String,
    /// 出错的路径（纯入参问题时为 None）。
    pub path: Option<String>,
}

impl ReadError {
    fn new(code: &'static str, message: String) -> Self {
        Self {
            code,
            message,
            path: None,
        }
    }

    fn at(code: &'static str, message: String, path: &str) -> Self {
        Self {
            code,
            message,
            path: Some(path.to_string()),
        }
    }
}

/// 一次文件读取的结果（文本已按行窗口与字节上限截断）。
#[derive(Debug)]
pub struct FileContent {
    /// 规范化后的绝对路径。
    pub path: String,
    /// 磁盘上的文件大小（字节）。
    pub size: u64,
    /// 文件总行数（不受窗口影响）。
    pub total_lines: usize,
    /// 本次窗口的起始行号（1 起）。
    pub start_line: usize,
    /// 窗口内实际返回的行数。
    pub lines_returned: usize,
    /// 还有更多行没返回（行数上限或字节预算拦下的）。
    pub truncated: bool,
    /// 被 [`MAX_LINE_CHARS`] 裁短的行数。
    pub clipped_lines: usize,
    /// 接着读时的 `start_line`（没有更多内容时为 None）。
    pub next_start_line: Option<usize>,
    /// 判定出的编码：`utf-8` / `utf-16le` / `utf-16be` / `ansi` / `utf-8-lossy`。
    /// 回显给调用方，便于判断正文可信度（`ansi` 与 `utf-8-lossy` 属于猜测）。
    pub encoding: &'static str,
    /// 窗口内的正文（不含行号）。
    pub text: String,
}

/// 校验路径：必须是绝对的单个文件路径。
///
/// `pub` 是为了让 MCP 层在调用 [`read_file`] 之前先跑一遍，把「相对路径 /
/// 通配符 / 控制字符」这类纯入参问题报成 INVALID_PARAMS —— 与 folder 参数的
/// 处理一致。`read_file` 内部也会再校验一次（幂等）。
///
/// 不做「必须在索引里」的检查 —— 官方 http_server 发文件前会用
/// `db_file_exists` 过滤，但那是面向可远程访问的 HTTP 服务的保守策略；
/// 本服务默认只监听 127.0.0.1（options.rs 的 DEFAULT_BIND），且未索引卷
/// （网络盘 / 非 NTFS）上的文件也该能读，故不设该限制。
pub fn validate_path(path: &str) -> Result<String, String> {
    let p = path.trim();
    if p.is_empty() {
        return Err("'path' must not be empty".into());
    }
    if p.chars().any(|c| c.is_control()) {
        return Err("'path' must not contain control characters".into());
    }
    if p.contains('*') || p.contains('?') {
        return Err(format!(
            "'path' must be a single file, not a pattern (received {:?}); \
             use search_in_folder to locate files first",
            p
        ));
    }
    if !Path::new(p).is_absolute() {
        return Err(format!(
            "'path' must be an absolute path like D:\\\\source\\\\repos\\\\myproject\\\\README.md; \
             received {:?}",
            p
        ));
    }
    Ok(p.to_string())
}

/// 读文件正文，返回 (文本, 判定出的编码)。
fn read_text(path: &str) -> Result<(String, &'static str), std::io::Error> {
    let raw = std::fs::read(path)?;
    Ok(decode_bytes(&raw))
}

/// 把磁盘字节解码成文本，并回显判定出的编码。
///
/// 顺序：UTF-16 BOM → UTF-8 BOM → 合法 UTF-8 → 本机 ANSI 代码页 → UTF-8 lossy。
///
/// 第 4 步是中文 Windows 上读 GBK 老文档 / 日志的关键：没有它，GBK 正文会
/// 整篇变成替换字符（实测主程序自己的 `utf8_basic_string_get_text_plain_file`
/// 就是这个行为，所以没用它）。
fn decode_bytes(raw: &[u8]) -> (String, &'static str) {
    if raw.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return (String::from_utf16_lossy(&units), "utf-16le");
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return (String::from_utf16_lossy(&units), "utf-16be");
    }
    // UTF-8 BOM 交给下面的分支处理（会作为 \u{feff} 留在串首），这里显式剥掉。
    let body = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    if let Ok(s) = core::str::from_utf8(body) {
        return (s.to_string(), "utf-8");
    }
    // 不是合法 UTF-8 且无 BOM —— 按本机 ANSI 代码页解（中文 Windows 即 GBK）。
    if let Some(s) = decode_ansi(body, CP_ACP) {
        return (s, "ansi");
    }
    (String::from_utf8_lossy(body).into_owned(), "utf-8-lossy")
}

/// 用 Windows 的 `MultiByteToWideChar` 按指定代码页解码。
///
/// 失败返回 None（调用方退回 UTF-8 lossy）。代码页作为参数传入是为了可测 ——
/// 直接写死 CP_ACP 的话，单元测试的结果会随机器区域设置变化。
fn decode_ansi(raw: &[u8], code_page: u32) -> Option<String> {
    if raw.is_empty() {
        return Some(String::new());
    }
    if raw.len() > i32::MAX as usize {
        return None;
    }
    // 先问需要多少 UTF-16 单元。
    let units = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            raw.as_ptr(),
            raw.len() as i32,
            core::ptr::null_mut(),
            0,
        )
    };
    if units <= 0 {
        return None;
    }
    let mut buf: Vec<u16> = vec![0; units as usize];
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            raw.as_ptr(),
            raw.len() as i32,
            buf.as_mut_ptr(),
            units,
        )
    };
    if written <= 0 {
        return None;
    }
    buf.truncate(written as usize);
    Some(String::from_utf16_lossy(&buf))
}

/// 一行按字符裁到 [`MAX_LINE_CHARS`]（走字符边界，不切多字节序列）。
fn clip_line(line: &str) -> (String, bool) {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((idx, _)) => (line[..idx].to_string(), true),
        None => (line.to_string(), false),
    }
}

/// 按行窗口取出的正文。
#[derive(Debug, PartialEq)]
struct Window {
    text: String,
    total_lines: usize,
    lines_returned: usize,
    /// 还有更多行没进窗口。
    truncated: bool,
    /// 被裁短的行数。
    clipped_lines: usize,
}

/// 从 `start_line`（1 起）开始取最多 `max_lines` 行（0 = 不限），
/// 同时受 [`MAX_WINDOW_BYTES`] 与 [`MAX_LINE_CHARS`] 约束。
///
/// 逐行累加字节数而不是先拼好再截断：这样字节预算只会拦下**整行**，
/// 不会从一行中间切开（旧实现用 `String::truncate` 会切在半行上，
/// 调用方拿到残行却没有任何标记）。
fn build_window(text: &str, start_line: usize, max_lines: usize) -> Window {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    // start_line 超出末尾时给出空窗口而不是报错。
    let start = start_line.saturating_sub(1).min(total);

    let mut out = String::new();
    let mut count = 0usize;
    let mut clipped = 0usize;
    let mut idx = start;
    while idx < total {
        if max_lines != 0 && count >= max_lines {
            break;
        }
        let (piece, was_clipped) = clip_line(lines[idx]);
        // 首行不加分隔符，其余行前面各有一个换行。
        let extra = piece.len() + usize::from(count > 0);
        if out.len() + extra > MAX_WINDOW_BYTES {
            break;
        }
        if count > 0 {
            out.push('\n');
        }
        out.push_str(&piece);
        if was_clipped {
            clipped += 1;
        }
        count += 1;
        idx += 1;
    }

    Window {
        text: out,
        total_lines: total,
        lines_returned: count,
        truncated: idx < total,
        clipped_lines: clipped,
    }
}

/// 读取文件内容（`read_file` 工具的入口）。
///
/// `start_line` 是 1 起的起始行号；`max_lines` 为 0 表示不限行数
/// （仍受 [`MAX_WINDOW_BYTES`] 约束）。
pub fn read_file(
    path: &str,
    start_line: usize,
    max_lines: usize,
) -> Result<FileContent, ReadError> {
    let path = validate_path(path).map_err(|m| ReadError::new(ERR_INVALID_ARGUMENT, m))?;

    // 黑名单先于 stat：拒绝一个路径不该先去碰它，也不该靠「文件不存在」
    // 与否泄漏它是否存在。
    if sensitive::is_sensitive_path(&path) {
        return Err(ReadError::at(
            ERR_PATH_DENIED,
            sensitive::denied_message(&path),
            &path,
        ));
    }

    let meta = std::fs::metadata(&path).map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound {
            ERR_NOT_FOUND
        } else {
            ERR_READ_FAILED
        };
        ReadError::at(code, format!("cannot read {:?}: {}", path, e), &path)
    })?;
    if meta.is_dir() {
        return Err(ReadError::at(
            ERR_PATH_IS_DIRECTORY,
            format!(
                "{:?} is a folder; use list_folder to list its contents",
                path
            ),
            &path,
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ReadError::at(
            ERR_TOO_LARGE,
            format!(
                "{:?} is {} bytes, over read_file's {} byte limit; \
                 use search_in_folder to narrow down to a smaller file, or read it outside this tool",
                path,
                meta.len(),
                MAX_FILE_BYTES
            ),
            &path,
        ));
    }

    let (text, encoding) = read_text(&path).map_err(|e| {
        ReadError::at(
            ERR_READ_FAILED,
            format!("cannot read {:?}: {}", path, e),
            &path,
        )
    })?;
    // 二进制文件会带 NUL 字节 —— 报错而不是把乱码塞给调用方。
    if text.as_bytes().contains(&0) {
        return Err(ReadError::at(
            ERR_BINARY_CONTENT,
            format!(
                "{:?} looks like a binary file (contains NUL bytes); read_file only returns text",
                path
            ),
            &path,
        ));
    }

    let window = build_window(&text, start_line, max_lines);
    Ok(FileContent {
        path,
        size: meta.len(),
        total_lines: window.total_lines,
        start_line,
        lines_returned: window.lines_returned,
        truncated: window.truncated,
        clipped_lines: window.clipped_lines,
        next_start_line: window
            .truncated
            .then_some(start_line + window.lines_returned),
        encoding,
        text: window.text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_rejects_relative_and_patterns() {
        assert!(validate_path("").unwrap_err().contains("empty"));
        assert!(validate_path("   ").unwrap_err().contains("empty"));
        assert!(validate_path("src\\lib.rs").unwrap_err().contains("absolute"));
        assert!(validate_path("D:\\a\\*.rs").unwrap_err().contains("pattern"));
        assert!(validate_path("D:\\a\\b?c").unwrap_err().contains("pattern"));
        assert!(validate_path("D:\\a\\b\tc").unwrap_err().contains("control"));
        assert_eq!(validate_path("  D:\\a\\b.rs  ").unwrap(), "D:\\a\\b.rs");
    }

    #[test]
    fn build_window_slices_by_one_based_line_number() {
        let text = "a\nb\nc\nd\ne";
        // 全部
        let w = build_window(text, 1, 0);
        assert_eq!(w.text, "a\nb\nc\nd\ne");
        assert_eq!((w.total_lines, w.lines_returned, w.truncated), (5, 5, false));
        // 中间一段
        let w = build_window(text, 2, 2);
        assert_eq!(w.text, "b\nc");
        assert_eq!((w.total_lines, w.lines_returned, w.truncated), (5, 2, true));
        // 尾部
        let w = build_window(text, 5, 10);
        assert_eq!((w.text.as_str(), w.lines_returned, w.truncated), ("e", 1, false));
        // 起点超出末尾 —— 空窗口而不是报错
        let w = build_window(text, 99, 10);
        assert_eq!((w.text.as_str(), w.total_lines, w.lines_returned), ("", 5, 0));
        assert!(!w.truncated);
    }

    #[test]
    fn build_window_tolerates_crlf_and_missing_trailing_newline() {
        let w = build_window("a\r\nb\r\n", 1, 0);
        assert_eq!((w.text.as_str(), w.total_lines, w.lines_returned), ("a\nb", 2, 2));
    }

    #[test]
    fn build_window_clips_long_lines_and_counts_them() {
        let long = "x".repeat(MAX_LINE_CHARS + 500);
        let text = format!("short\n{long}\nshort2");
        let w = build_window(&text, 1, 0);
        assert_eq!(w.clipped_lines, 1);
        assert_eq!(w.lines_returned, 3, "裁短的行仍然算一行");
        let mid = w.text.lines().nth(1).unwrap();
        assert_eq!(mid.chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    fn build_window_clip_is_char_boundary_safe() {
        // 每个字符 3 字节：裁剪点必然落在字符中间，必须回退到边界而不是 panic
        let long = "中".repeat(MAX_LINE_CHARS + 10);
        let w = build_window(&long, 1, 0);
        assert_eq!(w.clipped_lines, 1);
        assert_eq!(w.text.chars().count(), MAX_LINE_CHARS);
        assert!(w.text.chars().all(|c| c == '中'));
    }

    #[test]
    fn build_window_stops_on_byte_budget_without_cutting_a_line() {
        // 每行 100 字节，预算 512 KiB → 只能放进 5242 行左右，且必须是整行
        let line = "y".repeat(99);
        let text = vec![line.clone(); 6000].join("\n");
        let w = build_window(&text, 1, 0);
        assert!(w.truncated, "预算拦下时 truncated 应为真");
        assert!(w.text.len() <= MAX_WINDOW_BYTES);
        assert_eq!(w.lines_returned, w.text.lines().count());
        // 没有半行：每行都完整
        assert!(w.text.lines().all(|l| l == line));
    }

    #[test]
    fn clip_line_leaves_short_lines_untouched() {
        assert_eq!(clip_line("hello"), ("hello".to_string(), false));
        assert_eq!(clip_line(""), (String::new(), false));
        let exact = "z".repeat(MAX_LINE_CHARS);
        assert_eq!(clip_line(&exact), (exact.clone(), false), "正好到上限不算裁");
    }

    #[test]
    fn decode_bytes_handles_boms() {
        assert_eq!(decode_bytes(b"hello"), ("hello".to_string(), "utf-8"));
        assert_eq!(
            decode_bytes(&[0xEF, 0xBB, 0xBF, b'h', b'i']),
            ("hi".to_string(), "utf-8")
        );
        // UTF-16 LE / BE BOM
        assert_eq!(
            decode_bytes(&[0xFF, 0xFE, b'h', 0x00, b'i', 0x00]),
            ("hi".to_string(), "utf-16le")
        );
        assert_eq!(
            decode_bytes(&[0xFE, 0xFF, 0x00, b'h', 0x00, b'i']),
            ("hi".to_string(), "utf-16be")
        );
    }

    #[test]
    fn decode_ansi_decodes_gbk_bytes() {
        // 代码页写死 936 才可测 —— decode_bytes 用的 CP_ACP 随机器区域设置变。
        // 「中文」的 GBK 字节是 D6 D0 CE C4。
        assert_eq!(decode_ansi(&[0xD6, 0xD0, 0xCE, 0xC4], 936).unwrap(), "中文");
        assert_eq!(decode_ansi(&[], 936).unwrap(), "");
        // 完整的 GBK 句子（含全角标点）
        let raw = [
            0xD6, 0xD0, 0xCE, 0xC4, 0xB1, 0xE0, 0xC2, 0xEB, 0xB2, 0xE2, 0xCA, 0xD4, 0xA3, 0xBA,
            0xD5, 0xE2, 0xCA, 0xC7, 0xD2, 0xBB, 0xB6, 0xCE, 0x20, 0x47, 0x42, 0x4B, 0x20, 0xD5,
            0xFD, 0xCE, 0xC4, 0xA1, 0xA3,
        ];
        assert_eq!(
            decode_ansi(&raw, 936).unwrap(),
            "中文编码测试：这是一段 GBK 正文。"
        );
    }

    #[test]
    fn decode_bytes_prefers_utf8_then_ansi() {
        // 合法 UTF-8 原样返回
        assert_eq!(decode_bytes("中文".as_bytes()), ("中文".to_string(), "utf-8"));
        assert_eq!(
            decode_bytes(b"plain ascii"),
            ("plain ascii".to_string(), "utf-8")
        );
        // 非法 UTF-8 交给 ANSI 兜底：不该留下替换字符（具体字符随本机代码页，
        // 所以只断言「没有乱码」，精确解码由 decode_ansi 的 936 用例覆盖）
        let (gbk, encoding) = decode_bytes(&[0xD6, 0xD0, 0xCE, 0xC4]);
        assert_eq!(encoding, "ansi");
        assert!(!gbk.contains('\u{FFFD}'), "{gbk}");
        // 对照：这正是主程序读取器会给的结果（UTF-8 lossy）
        assert!(String::from_utf8_lossy(&[0xD6, 0xD0, 0xCE, 0xC4]).contains('\u{FFFD}'));
    }

    #[test]
    fn read_file_denies_sensitive_paths_before_touching_them() {
        // 黑名单在 stat 之前生效：不存在的敏感路径也报 PATH_DENIED 而不是 NOT_FOUND
        let err = read_file(r"C:\definitely\missing\.ssh\id_rsa", 1, 10).unwrap_err();
        assert_eq!(err.code, ERR_PATH_DENIED);
        assert_eq!(err.path.as_deref(), Some(r"C:\definitely\missing\.ssh\id_rsa"));

        let err = read_file(r"D:\proj\.env", 1, 10).unwrap_err();
        assert_eq!(err.code, ERR_PATH_DENIED);

        let err = read_file(r"D:\repo\.git\objects\ab\cd", 1, 10).unwrap_err();
        assert_eq!(err.code, ERR_PATH_DENIED);
    }

    #[test]
    fn read_file_reports_missing_files_with_their_own_code() {
        let missing = r"C:\definitely\missing\plain-file-that-is-not-there.txt";
        let err = read_file(missing, 1, 10).unwrap_err();
        assert_eq!(err.code, ERR_NOT_FOUND);
        assert_eq!(err.path.as_deref(), Some(missing));
    }
}
