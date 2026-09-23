//! journal.rs — 索引变更日志（Index Journal）查询
//!
//! Everything 的「索引日志」记录索引里每个文件/文件夹的创建、修改、删除、
//! 重命名、移动事件。本模块把它暴露成可查询的数据源，供 MCP 工具
//! `index_changes` 使用。
//!
//! # 为什么读文本日志而不是 db_journal_* 接口
//!
//! Everything 1.5 的插件 SDK 确实导出了 `db_journal_file_open/read/close` 等
//! 六个 journal 函数（官方论坛 t=16535 p=75489，void 2025-05-27 发布，签名
//! 已核实并抄录在 docs/PLUGIN_SDK_API_CN.md 第 10 节），但它们有两个无法
//! 从任何公开资料补齐的缺口：
//!   - **记录格式完全无文档**：`db_journal_file_read` 只是往 buf 里灌字节流，
//!     官方既没有 struct 定义，也没有字段顺序、大小或解析循环；
//!   - **`journal_id` 的来源无文档**：通知回调只透传 `user_data`，不给出
//!     journal id；而二进制里的 `db_journal_get_info` / `db_journal_enum_changes`
//!     只是内部函数名，并未导出给插件（Everything.exe 里查证：导出的 journal
//!     相关函数只有那六个）。
//!
//! 因此走方案里预先约定的降级路径：解析 Everything 自己写的 journal_log 文本
//! 日志。格式在 1.5.0.1422b 上实证过（见下面常量与单元测试的 fixture），
//! 零格式风险。代价是依赖用户在 Everything 里开启日志记录 —— 工具检测到未
//! 开启时给出带确切改法的报错，而不是静默返回空结果。
//!
//! # 日志格式（实证）
//!
//! `journal_log=1` 时 Everything 每天写一个文件：
//!   `<日志目录>\index-journal-YYYY-MM-DD.txt`
//! 默认日志目录是 `%LOCALAPPDATA%\Everything\Logs`（可被 INI 的
//! `journal_log_directory` 改写）。每行一条变更，UTF-8、LF 行尾、制表符分列，
//! 共 6 列：
//!   `<journal_id>\t<change_id>\t<YYYY-MM-DD HH:MM:SS>\t"<动作>"\t"<路径>"\t"<新路径>"`
//! 例如：
//!   `134345291781008867\t352520\t2026-09-23 11:18:44\t"文件创建"\t"D:\src\a.txt"\t""`
//! 文件夹条目的路径列以 `\` 结尾；重命名/移动条目两个路径列都有值。
//! 时间列是本机**本地时间**（Everything 自己按本地时区格式化），不带时区。
//!
//! 动作列是本地化字符串（随 Everything 的界面语言变化），因此按多语言关键词
//! 归类成稳定的枚举值；归不上的记为 `other` 并保留原始串，LLM 仍能读懂。

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 单条索引变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// journal 世代 id —— 同一段日志期间保持不变，可用来分辨重启/重索引。
    pub journal_id: u64,
    /// 单调递增的变更序号，天然按时间排序。
    pub change_id: u64,
    /// 本机本地时间，形如 `2026-09-23 11:18:44`。
    pub date: String,
    /// 归类后的动作。
    pub action: Action,
    /// Everything 原始输出的本地化动作串（如「文件创建」）。
    pub action_text: String,
    pub is_folder: bool,
    /// 变更的完整路径（文件夹已去掉尾部分隔符）。
    pub path: String,
    /// 重命名/移动的目标路径。
    pub new_path: Option<String>,
}

impl Change {
    /// 文件名部分（不含目录）。
    pub fn name(&self) -> &str {
        basename(&self.path)
    }

    /// 把时间列换算成 Unix 秒。失败（越界）时返回 `None`。
    pub fn unix_seconds(&self) -> Option<u64> {
        local_civil_to_unix(&self.date)
    }
}

/// 归类后的动作。字符串值会直接进 MCP 响应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Modified,
    Deleted,
    Renamed,
    Moved,
    Other,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::Modified => "modified",
            Action::Deleted => "deleted",
            Action::Renamed => "renamed",
            Action::Moved => "moved",
            Action::Other => "other",
        }
    }
}

/// 查询过滤器。`None` 表示不限制。
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub action: Option<Action>,
    /// 完整路径前缀，不区分大小写（`D:\src` 命中 `D:\src\...` 下的一切）。
    pub path: Option<String>,
    /// 文件名子串，不区分大小写；重命名条目同时匹配新名字。
    pub name: Option<String>,
    /// 下限（含），已规范化为 `YYYY-MM-DD HH:MM:SS`。
    pub since: Option<String>,
    /// 上限（含），已规范化同上。
    pub until: Option<String>,
    /// 最多返回多少条（最新优先）。
    pub max_results: usize,
}

/// 查询结果。
#[derive(Debug, Clone)]
pub struct Outcome {
    /// 实际返回的条数。
    pub count: usize,
    pub changes: Vec<Change>,
    /// 实际打开过的日志文件数（让调用方知道回溯深度）。
    pub days_searched: usize,
    /// 实际读入的字节数（分块倒序扫描，通常远小于日志总体积）。
    pub bytes_scanned: u64,
    /// true = 已取满 `max_results` 提前收尾，日志里可能还有更早的匹配项。
    pub truncated: bool,
    /// 解析失败的行数（正在写入的半行、格式不认的行）。
    pub skipped_lines: u64,
    /// 实际使用的日志目录。
    pub log_directory: PathBuf,
}

/// 回溯的日志天数上限。日志按天一个文件，超出这个跨度只取最近这些天。
const MAX_DAYS: usize = 92;

/// 倒序扫描的读取块大小。256 KiB 一次：既少发系统调用，又保证
/// 「取最近 N 条」这类常见查询只读文件尾部一小段。
const CHUNK: usize = 256 * 1024;

/// 单个日志文件最多读入的字节数。配上倒序扫描，即使过滤器什么都匹配不上
/// 也不会把一个巨大的日志文件读穿。
const MAX_BYTES_PER_FILE: u64 = 64 * 1024 * 1024;

/// 动作关键词表。**顺序即优先级**：必须先判重命名再判修改 ——
/// 日语「名前の変更」、韩语「이름 변경」都含有表示「修改」的「変更/변경」。
const ACTION_KEYWORDS: &[(Action, &[&str])] = &[
    (
        Action::Renamed,
        &[
            "重命名",
            "重新命名",
            "改名",
            "名前の変更",
            "이름 변경",
            "renamed",
            "renommé",
            "renombrado",
            "umbenannt",
            "rinominato",
            "renomeado",
            "hernoemd",
            "yeniden adlandırıldı",
            "переименован",
            "przeniesiono",
        ],
    ),
    (
        Action::Moved,
        &[
            "移动",
            "移動",
            "이동",
            "moved",
            "déplacé",
            "movido",
            "verschoben",
            "spostato",
            "verplaatst",
            "taşındı",
            "перемещён",
            "перемещен",
        ],
    ),
    (
        Action::Deleted,
        &[
            "删除",
            "刪除",
            "削除",
            "삭제",
            "deleted",
            "supprimé",
            "eliminado",
            "gelöscht",
            "eliminato",
            "excluído",
            "verwijderd",
            "silindi",
            "удалён",
            "удален",
            "usunięto",
        ],
    ),
    (
        Action::Modified,
        &[
            "修改",
            "変更",
            "수정",
            "변경",
            "modified",
            "modifié",
            "modificado",
            "geändert",
            "modificato",
            "gewijzigd",
            "değiştirildi",
            "изменён",
            "изменен",
            "zmodyfikowano",
        ],
    ),
    (
        Action::Created,
        &[
            "创建",
            "創建",
            "作成",
            "생성",
            "created",
            "créé",
            "creado",
            "erstellt",
            "creato",
            "criado",
            "aangemaakt",
            "oluşturuldu",
            "создан",
            "створено",
        ],
    ),
];

/// 「文件夹」关键词，动作串判不出类型时的兜底。
const FOLDER_KEYWORDS: &[&str] = &[
    "文件夹",
    "文件夾",
    "資料夾",
    "フォルダ",
    "디렉터리",
    "폴더",
    "folder",
    "dossier",
    "carpeta",
    "ordner",
    "cartella",
    "directorio",
    "mapa",
    "klasör",
    "папка",
    "directory",
];

/// 把 Everything 输出的本地化动作串归类成稳定枚举。
///
/// 认不出的返回 [`Action::Other`] —— 调用方仍能拿到 `action_text` 原文，
/// 不会因为多语言覆盖不全就把事件丢掉。
pub fn classify(raw: &str) -> Action {
    let lower = raw.to_lowercase();
    for (action, keys) in ACTION_KEYWORDS {
        if keys.iter().any(|k| lower.contains(&k.to_lowercase())) {
            return *action;
        }
    }
    Action::Other
}

/// 从动作串判断是不是文件夹条目（兜底；主判断依据是路径列的尾部分隔符）。
fn action_implies_folder(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    FOLDER_KEYWORDS
        .iter()
        .any(|k| lower.contains(&k.to_lowercase()))
}

// ====================================================================
// 解析
// ====================================================================

/// 去掉 Everything 给路径/动作列包的成对双引号。
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// 取路径的文件名部分（最后一个 `\` 之后）。
fn basename(path: &str) -> &str {
    match path.rfind('\\') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// 去掉路径的尾部分隔符；盘符根（`D:\`）与 UNC 头（`\\`）必须留住斜杠，
/// 否则剥光就成了 `D:` 这种会被误认成相对路径的形态。
fn strip_trailing_separators(p: &str) -> String {
    let t = p.trim_end_matches('\\');
    if t.len() >= 3 {
        t.to_string()
    } else {
        p.to_string()
    }
}

/// 校验 `YYYY-MM-DD HH:MM:SS` 形状（不做日历合法性校验 —— 日志不会写出
/// 2 月 30 日，而严格日历表只会给未来日期添麻烦）。
fn is_civil_datetime(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b' '
        || b[13] != b':'
        || b[16] != b':'
    {
        return false;
    }
    [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
        .iter()
        .all(|&i| b[i].is_ascii_digit())
}

/// 解析一行日志。结构不符（列数不对、id 不是数字、日期形状不对）返回 `None`。
pub fn parse_line(line: &str) -> Option<Change> {
    // Everything 写的是 LF；这里同时容忍 CRLF 与行尾空白。
    let line = line.trim_end_matches(['\r', '\n', ' ', '\t']);
    if line.is_empty() {
        return None;
    }

    let mut it = line.split('\t');
    let journal_id = it.next()?.trim().parse::<u64>().ok()?;
    let change_id = it.next()?.trim().parse::<u64>().ok()?;
    let date = it.next()?.trim();
    if !is_civil_datetime(date) {
        return None;
    }
    let action_text = unquote(it.next()?);
    let path = unquote(it.next()?);
    // 少了新路径列 —— 日志正写到一半，或第三方工具改过格式。
    let new_path = {
        let s = unquote(it.next()?);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    // 路径里含制表符会让列数超过 6，这种行无法可靠切分，直接跳过。
    if it.next().is_some() || path.is_empty() {
        return None;
    }

    // 文件夹条目的路径列以 `\` 结尾（实证），据此把尾斜杠从回报路径里去掉。
    let is_folder = path.ends_with('\\') || action_implies_folder(&action_text);
    let path = if is_folder {
        strip_trailing_separators(&path)
    } else {
        path
    };

    Some(Change {
        journal_id,
        change_id,
        date: date.to_string(),
        action: classify(&action_text),
        action_text,
        is_folder,
        path,
        new_path,
    })
}

// ====================================================================
// 日志定位
// ====================================================================

/// Everything 主配置文件的候选路径。
///
/// `std::env::current_exe()` 在 DLL 里返回宿主 exe 路径，因此最后一项覆盖
/// Everything 的便携安装（ini 挨着 `Everything.exe`）；前两项覆盖常规安装。
fn ini_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for base in ["APPDATA", "LOCALAPPDATA"] {
        if let Ok(dir) = std::env::var(base) {
            v.push(Path::new(&dir).join("Everything").join("Everything.ini"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("Everything.ini"));
        }
    }
    v
}

/// 读出 INI 文本（UTF-8；带 BOM 时按 UTF-16 解）。
fn read_ini(path: &Path) -> Option<String> {
    let raw = fs::read(path).ok()?;
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        let (pairs, _odd_tail) = raw[2..].as_chunks::<2>();
        let u16s: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    String::from_utf8(raw).ok()
}

/// 在 `[everything]` 段里读一个键值，找不到再退回全文扫描。
///
/// Everything 的 INI 是标准 `key=value`；空值一律视为未设置（返回 `None`），
/// 这样 `journal_log_directory=` 就等价于用默认目录。
fn ini_value(text: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    let mut global: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[everything]");
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(v) = rest.strip_prefix('=') {
                let v = v.trim();
                if v.is_empty() {
                    continue;
                }
                if global.is_none() {
                    global = Some(v.to_string());
                }
                if in_section {
                    return Some(v.to_string());
                }
            }
        }
    }
    global
}

/// 定位日志目录。
///
/// 优先用 INI 里显式设置的 `journal_log_directory`；否则按「挨着
/// Everything.ini 的 Logs」→ `%LOCALAPPDATA%\Everything\Logs` →
/// 「挨着 Everything.exe 的 Logs」的顺序，取第一个真实存在的目录。
pub fn log_directory() -> PathBuf {
    let ini = ini_candidates().into_iter().find(|p| p.is_file());

    if let Some(ini_path) = &ini {
        if let Some(text) = read_ini(ini_path) {
            if let Some(dir) = ini_value(&text, "journal_log_directory") {
                let p = PathBuf::from(&dir);
                if p.is_absolute() {
                    return p;
                }
                // 相对路径按 INI 所在目录解析。
                if let Some(parent) = ini_path.parent() {
                    let joined = parent.join(&p);
                    if joined.is_dir() {
                        return joined;
                    }
                }
                if p.is_dir() {
                    return p;
                }
            }
        }
    }

    let mut fallbacks: Vec<PathBuf> = Vec::new();
    if let Some(ini_path) = &ini {
        if let Some(parent) = ini_path.parent() {
            fallbacks.push(parent.join("Logs"));
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        fallbacks.push(Path::new(&local).join("Everything").join("Logs"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            fallbacks.push(dir.join("Logs"));
        }
    }
    // 一个都不存在时返回默认值，让调用方据此给出「去哪开启」的提示。
    fallbacks
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or_else(|| {
            let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
            Path::new(&local).join("Everything").join("Logs")
        })
}

/// 列出日志目录里的日志文件，**新的在前**。
///
/// 文件名里的日期既是回溯跨度的依据，也天然可按字典序比较。
fn day_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(day) = day_from_filename(name) {
                out.push((day, path));
            }
        }
    }
    // 日期字符串字典序即时间序；路径二次排序保证同一天多文件时结果稳定。
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    out
}

/// 从 `index-journal-YYYY-MM-DD.txt` 取出 `YYYY-MM-DD`。
fn day_from_filename(name: &str) -> Option<String> {
    let day = name.strip_prefix("index-journal-")?.strip_suffix(".txt")?;
    let b = day.as_bytes();
    let shape_ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|&i| b[i].is_ascii_digit());
    shape_ok.then(|| day.to_string())
}

// ====================================================================
// 查询
// ====================================================================

/// 查询索引变更日志，返回最新优先的结果。
///
/// 不触达任何 Everything 宿主接口，纯文件读取 —— 可在任意线程调用。
pub fn query(filter: &Filter) -> Result<Outcome, String> {
    let dir = log_directory();
    if !dir.is_dir() {
        return Err(format!(
            "index journal log directory not found: {}\n\
             Everything only records index changes when 'journal_log' is enabled. \
             Turn it on in Everything: Tools > Options > Index > Journal > Log changes \
             (or set `journal_log=1` under [Everything] in %APPDATA%\\Everything\\Everything.ini \
             and restart Everything).",
            dir.display()
        ));
    }

    let files = day_files(&dir);
    if files.is_empty() {
        return Err(format!(
            "no index-journal-*.txt log files in {}\n\
             Everything writes one file per day when 'journal_log' is enabled. \
             Turn it on in Everything: Tools > Options > Index > Journal > Log changes \
             (or set `journal_log=1` under [Everything] in %APPDATA%\\Everything\\Everything.ini \
             and restart Everything).",
            dir.display()
        ));
    }

    // 按 since/until 收窄候选文件。日期字符串字典序即时间序。
    let since_day = filter.since.as_deref().map(|s| s[..10].to_string());
    let until_day = filter.until.as_deref().map(|s| s[..10].to_string());
    let selected: Vec<&(String, PathBuf)> = files
        .iter()
        .filter(|(day, _)| {
            since_day.as_deref().is_none_or(|s| day.as_str() >= s)
                && until_day.as_deref().is_none_or(|u| day.as_str() <= u)
        })
        // day_files 已是新的在前，倒序扫描要的就是这个顺序。
        .take(MAX_DAYS)
        .collect();

    let wanted = filter.max_results.max(1);
    let mut changes: Vec<Change> = Vec::new();
    let mut bytes_scanned: u64 = 0;
    let mut skipped_lines: u64 = 0;
    let mut days_searched = 0usize;
    let mut stopped_early = false;

    let path_prefix = filter
        .path
        .as_deref()
        .map(|p| normalize_prefix(p).trim_end_matches('\\').to_lowercase());
    let name_sub = filter.name.as_deref().map(|n| n.to_lowercase());

    // 单行处理：解析 + 过滤 + 收编。返回 false 表示已取满，调用方立即停手。
    // 把它做成闭包而不是内联三处，是为了让「取满即停」只有一个判断点。
    let accept = |line: &str, changes: &mut Vec<Change>, skipped: &mut u64| -> bool {
        if changes.len() >= wanted {
            return false;
        }
        match parse_line(line) {
            Some(c) => {
                if matches_filter(&c, filter, path_prefix.as_deref(), name_sub.as_deref()) {
                    changes.push(c);
                }
            }
            None => {
                if !line.trim().is_empty() {
                    *skipped += 1;
                }
            }
        }
        true
    };

    'files: for (_, file_path) in selected {
        let limit = match fs::metadata(file_path) {
            Ok(m) => m.len().min(MAX_BYTES_PER_FILE),
            Err(_) => continue,
        };
        let mut handle = match fs::File::open(file_path) {
            Ok(h) => h,
            Err(_) => continue,
        };
        days_searched += 1;

        // 倒序分块读：从文件尾往前啃，取满 wanted 立刻收工，
        // 「最近发生了什么」这类查询因此不必读整个日志。
        let mut end = limit;
        // carry 装上一块开头那一行的前半段，本块用块尾把它接完整。
        let mut carry = String::new();
        let mut at_tail = true;
        while end > 0 {
            let start = end.saturating_sub(CHUNK as u64);
            let take = (end - start) as usize;
            let mut buf = vec![0u8; take];
            if handle.seek(SeekFrom::Start(start)).is_err() || handle.read_exact(&mut buf).is_err()
            {
                break;
            }
            bytes_scanned += take as u64;
            let text = String::from_utf8_lossy(&buf).into_owned();

            // split 出的第一段是更早一块的半行尾巴，最后一段则是通向
            // 更早一块的半行开头 —— 只有中间各段是完整行。
            let mut segments: Vec<&str> = text.split('\n').collect();
            let tail = segments.pop().unwrap_or("");
            // 整块一个换行符都没有（单行超过一个块）时，首段就是尾段：
            // 它既是一条跨块行的前半段，也是另一条跨块行的后半段。
            let no_newline = segments.is_empty();
            let head = if no_newline { tail } else { segments.remove(0) };

            if at_tail && !no_newline {
                // 第一块到尾即文件尾：这段就是最后一行（文件未必以换行符收尾），
                // 它是全文件最新的一条，必须最先处理。
                if !tail.trim().is_empty() && !accept(tail, &mut changes, &mut skipped_lines) {
                    stopped_early = true;
                    break 'files;
                }
            } else if !at_tail {
                // tail 是与上一块边界处那一行的前半段，接上 carry 才是完整一行；
                // 它比本块其余行都新，所以也要先处理。
                let joined = format!("{}{}", tail, carry);
                if !accept(&joined, &mut changes, &mut skipped_lines) {
                    stopped_early = true;
                    break 'files;
                }
            }
            at_tail = false;
            for seg in segments.iter().rev() {
                if !accept(seg, &mut changes, &mut skipped_lines) {
                    stopped_early = true;
                    break 'files;
                }
            }
            carry = head.to_string();
            end = start;
        }
        // 走到这里 carry 是最老一块的开头片段 —— 它前面没有更多字节了，
        // 因此就是文件的第一行，同样要过一遍过滤。
        if !accept(&carry, &mut changes, &mut skipped_lines) {
            stopped_early = true;
            break 'files;
        }
    }

    Ok(Outcome {
        count: changes.len(),
        changes,
        days_searched,
        bytes_scanned,
        truncated: stopped_early,
        skipped_lines,
        log_directory: dir,
    })
}

/// 规范化路径前缀：统一 `\` 分隔、折叠连续反斜杠、去尾斜杠（盘符根除外）。
fn normalize_prefix(p: &str) -> String {
    let unified = p.trim().replace('/', "\\");
    let mut out = String::with_capacity(unified.len() + 1);
    let mut prev_bs = false;
    for c in unified.chars() {
        if c == '\\' {
            if !prev_bs {
                out.push('\\');
            }
            prev_bs = true;
        } else {
            out.push(c);
            prev_bs = false;
        }
    }
    // `C:` 补成 `C:\`；盘符根的尾斜杠必须保留，否则前缀永远不命中。
    if out.len() == 2 {
        out.push('\\');
    }
    while out.len() > 3 && out.ends_with('\\') {
        out.pop();
    }
    out
}

/// 单条过滤判定。
fn matches_filter(
    c: &Change,
    f: &Filter,
    path_prefix: Option<&str>,
    name_sub: Option<&str>,
) -> bool {
    if let Some(a) = f.action {
        if c.action != a {
            return false;
        }
    }
    // 两边都是 `YYYY-MM-DD HH:MM:SS` 定宽串，字典序即时间序。
    if let Some(s) = &f.since {
        if c.date.as_str() < s.as_str() {
            return false;
        }
    }
    if let Some(u) = &f.until {
        if c.date.as_str() > u.as_str() {
            return false;
        }
    }
    if let Some(p) = path_prefix {
        if !c.path.to_lowercase().starts_with(p) {
            return false;
        }
    }
    if let Some(n) = name_sub {
        let hit = c.name().to_lowercase().contains(n)
            || c.new_path
                .as_deref()
                .map(|np| basename(np).to_lowercase().contains(n))
                .unwrap_or(false);
        if !hit {
            return false;
        }
    }
    true
}

// ====================================================================
// 时间输入规范化
// ====================================================================

/// 把调用方给的 `since` / `until` 规范化成 `YYYY-MM-DD HH:MM:SS`。
///
/// 接受 `YYYY-MM-DD`、`YYYY-MM-DD HH:MM`、`YYYY-MM-DD HH:MM:SS`（日期与时间
/// 之间用 `T` 或空格都行），以及 Unix 秒 —— 后者按本机时区折成本地时间再
/// 比较，因为日志里的时间列本来就是本机本地时间。
pub fn normalize_timestamp(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("timestamp must not be empty".to_string());
    }
    // 纯数字按 Unix 秒处理。
    if s.bytes().all(|b| b.is_ascii_digit()) {
        let secs: u64 = s
            .parse()
            .map_err(|_| format!("invalid unix timestamp: {:?}", input))?;
        return local_civil_from_unix(secs)
            .ok_or_else(|| format!("unix timestamp out of range: {}", secs));
    }

    let (date_part, time_part) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let [y, mo, d] = match parse_date(date_part) {
        Some(v) => v,
        None => {
            return Err(format!(
                "expected '2026-09-23', '2026-09-23 11:18' or '2026-09-23 11:18:31' \
                 (a bare number is read as unix seconds); received {:?}",
                input
            ))
        }
    };
    let [h, mi, se] = match time_part.trim() {
        // 只给日期：从当天零点算起。
        "" => [0, 0, 0],
        t => match parse_time(t) {
            Some(v) => v,
            None => {
                return Err(format!(
                    "expected a time as 'HH', 'HH:MM' or 'HH:MM:SS' with hour 0-23 \
                     and minute/second 0-59; received {:?}",
                    input
                ))
            }
        },
    };
    Ok(format_civil(y, mo, d, h, mi, se))
}

/// 解析 `YYYY-MM-DD` 并校验数值范围。调用方给的时间戳错了就是错，
/// 不能悄悄变成一个注定 0 结果的过滤器。
fn parse_date(s: &str) -> Option<[u16; 3]> {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || ![0, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|&i| b[i].is_ascii_digit())
    {
        return None;
    }
    let get2 = |i: usize| s[i..i + 2].parse::<u16>().ok();
    // 年占 4 位，月/日各 2 位。
    let v = [s[0..4].parse::<u16>().ok()?, get2(5)?, get2(8)?];
    // 年 0、月 >12、日 >31 都拦下来。
    if v[0] == 0 || !(1..=12).contains(&v[1]) || !(1..=31).contains(&v[2]) {
        return None;
    }
    Some(v)
}

/// 解析时间部分：`HH` / `HH:MM` / `HH:MM:SS`，缺省的段补 0。
///
/// `str::parse` 会放过 `+12` 这种带符号写法，所以再逐段查一遍是否纯数字。
fn parse_time(s: &str) -> Option<[u16; 3]> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut out = [0u16; 3];
    for (i, p) in parts.iter().enumerate() {
        let t = p.trim();
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u16 = t.parse().ok()?;
        // 第一段是小时（0–23），分秒各 0–59。
        if n > if i == 0 { 23 } else { 59 } {
            return None;
        }
        out[i] = n;
    }
    Some(out)
}

/// 从 `YYYY-MM-DD HH:MM:SS` 拆出数字字段。
fn civil_parts(s: &str) -> Option<[u16; 6]> {
    if !is_civil_datetime(s) {
        return None;
    }
    let get = |i: usize| s[i..i + 2].parse::<u16>().ok();
    let year = s[0..4].parse::<u16>().ok()?;
    Some([year, get(5)?, get(8)?, get(11)?, get(14)?, get(17)?])
}

/// Unix 秒 → 本机本地时间字符串。越界返回 `None`。
///
/// 走 `SystemTimeToTzSpecificLocalTime`（时区传 NULL = 用当前活动时区，含
/// 夏令时规则）而不是手写偏移量 —— 后者搞不定 DST，会把夏令时前后的时间
/// 整体挪错一小时。
#[cfg(windows)]
fn local_civil_from_unix(secs: u64) -> Option<String> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // Unix 纪元（1970）与 FILETIME 纪元（1601）相差 11644473600 秒。
    let hundreds = (secs + 11_644_473_600).checked_mul(10_000_000)?;
    let utc_ft = FILETIME {
        dwLowDateTime: (hundreds & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (hundreds >> 32) as u32,
    };
    let mut utc: SYSTEMTIME = unsafe { core::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&utc_ft, &mut utc) } == 0 {
        return None;
    }
    let mut local: SYSTEMTIME = unsafe { core::mem::zeroed() };
    if unsafe { SystemTimeToTzSpecificLocalTime(core::ptr::null(), &utc, &mut local) } == 0 {
        return None;
    }
    Some(format_civil(
        local.wYear,
        local.wMonth,
        local.wDay,
        local.wHour,
        local.wMinute,
        local.wSecond,
    ))
}

#[cfg(not(windows))]
fn local_civil_from_unix(_secs: u64) -> Option<String> {
    None
}

/// 本机本地时间字符串 → Unix 秒。越界或平台不支持返回 `None`。
#[cfg(windows)]
fn local_civil_to_unix(civil: &str) -> Option<u64> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};
    let p = civil_parts(civil)?;
    let local = SYSTEMTIME {
        wYear: p[0],
        wMonth: p[1],
        wDayOfWeek: 0,
        wDay: p[2],
        wHour: p[3],
        wMinute: p[4],
        wSecond: p[5],
        wMilliseconds: 0,
    };
    let mut utc: SYSTEMTIME = unsafe { core::mem::zeroed() };
    if unsafe { TzSpecificLocalTimeToSystemTime(core::ptr::null(), &local, &mut utc) } == 0 {
        return None;
    }
    let mut utc_ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    if unsafe { SystemTimeToFileTime(&utc, &mut utc_ft) } == 0 {
        return None;
    }
    let hundreds = ((utc_ft.dwHighDateTime as u64) << 32) | utc_ft.dwLowDateTime as u64;
    hundreds
        .checked_sub(11_644_473_600 * 10_000_000)
        .map(|h| h / 10_000_000)
}

#[cfg(not(windows))]
fn local_civil_to_unix(_civil: &str) -> Option<u64> {
    None
}

fn format_civil(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16) -> String {
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 实证样本：Everything 1.5.0.1422b，journal_log=1，中文界面。
    const SAMPLE_CREATED: &str = "134345291781008867\t352520\t2026-09-23 11:18:44\t\"文件创建\"\t\"D:\\source\\repos\\everything-mcp\\.sdk-cache\\zzjournal_probe\\ZZCREATE-7f3a1c.txt\"\t\"\"";
    const SAMPLE_RENAMED: &str = "134345291781008867\t352531\t2026-09-23 11:18:46\t\"文件重命名\"\t\"D:\\a\\ZZCREATE-7f3a1c.txt\"\t\"D:\\a\\ZZRENAME-7f3a1c.txt\"";
    const SAMPLE_FOLDER: &str = "134345291781008867\t352536\t2026-09-23 11:18:47\t\"文件夹创建\"\t\"D:\\a\\ZZFOLDER-7f3a1c\\\"\t\"\"";

    #[test]
    fn parses_created_line() {
        let c = parse_line(SAMPLE_CREATED).expect("应能解析创建行");
        assert_eq!(c.journal_id, 134345291781008867);
        assert_eq!(c.change_id, 352520);
        assert_eq!(c.date, "2026-09-23 11:18:44");
        assert_eq!(c.action, Action::Created);
        assert_eq!(c.action_text, "文件创建");
        assert!(!c.is_folder);
        assert!(c.path.ends_with("ZZCREATE-7f3a1c.txt"));
        assert_eq!(c.new_path, None);
        assert_eq!(c.name(), "ZZCREATE-7f3a1c.txt");
    }

    #[test]
    fn parses_renamed_line_and_keeps_target() {
        let c = parse_line(SAMPLE_RENAMED).expect("应能解析重命名行");
        assert_eq!(c.action, Action::Renamed);
        assert_eq!(c.new_path.as_deref(), Some("D:\\a\\ZZRENAME-7f3a1c.txt"));
    }

    #[test]
    fn folder_line_is_flagged_and_trailing_slash_stripped() {
        let c = parse_line(SAMPLE_FOLDER).expect("应能解析文件夹行");
        assert_eq!(c.action, Action::Created);
        assert!(c.is_folder);
        assert_eq!(c.path, "D:\\a\\ZZFOLDER-7f3a1c");
    }

    #[test]
    fn classifies_english_actions() {
        assert_eq!(classify("File created"), Action::Created);
        assert_eq!(classify("File modified"), Action::Modified);
        assert_eq!(classify("File deleted"), Action::Deleted);
        assert_eq!(classify("File renamed"), Action::Renamed);
        assert_eq!(classify("File moved"), Action::Moved);
        assert_eq!(classify("Folder created"), Action::Created);
        assert_eq!(classify("Folder deleted"), Action::Deleted);
    }

    #[test]
    fn rename_beats_modify_when_keywords_nest() {
        // 日语「名前の変更」含有表示「修改」的「変更」，必须先命中重命名。
        assert_eq!(classify("ファイル名前の変更"), Action::Renamed);
        assert_eq!(classify("이름 변경"), Action::Renamed);
        assert_eq!(classify("ファイル変更"), Action::Modified);
    }

    #[test]
    fn unknown_action_falls_back_to_other() {
        assert_eq!(classify("发生了某种事"), Action::Other);
        let line = SAMPLE_CREATED.replace("文件创建", "某种事");
        let c = parse_line(&line).expect("结构仍合法");
        assert_eq!(c.action, Action::Other);
        assert_eq!(c.action_text, "某种事");
    }

    #[test]
    fn rejects_malformed_lines() {
        // 列数不足（日志写入途中的半行）。
        assert!(parse_line("1\t2\t2026-09-23 11:18:44\t\"文件创建\"").is_none());
        // id 不是数字。
        assert!(parse_line("x\t2\t2026-09-23 11:18:44\t\"文件创建\"\t\"a\"\t\"\"").is_none());
        // 日期形状不对。
        assert!(parse_line("1\t2\t2026/09/23 11:18:44\t\"文件创建\"\t\"a\"\t\"\"").is_none());
        // 含制表符的路径导致列数溢出。
        assert!(parse_line("1\t2\t2026-09-23 11:18:44\t\"文件创建\"\t\"a\tb\"\t\"\"").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn tolerates_crlf_and_trailing_whitespace() {
        let with_crlf = format!("{}\r\n", SAMPLE_CREATED);
        let c = parse_line(&with_crlf).expect("CRLF 行尾应被容忍");
        assert_eq!(c.action, Action::Created);
    }

    #[test]
    fn normalize_timestamp_accepts_common_shapes() {
        assert_eq!(
            normalize_timestamp("2026-09-23").unwrap(),
            "2026-09-23 00:00:00"
        );
        assert_eq!(
            normalize_timestamp("2026-09-23T11:18").unwrap(),
            "2026-09-23 11:18:00"
        );
        assert_eq!(
            normalize_timestamp("2026-09-23 11:18:31").unwrap(),
            "2026-09-23 11:18:31"
        );
        assert_eq!(
            normalize_timestamp(" 2026-09-23 11 ").unwrap(),
            "2026-09-23 11:00:00"
        );
        assert!(normalize_timestamp("昨天").is_err());
        assert!(normalize_timestamp("").is_err());
    }

    #[test]
    fn normalize_timestamp_rejects_out_of_range_parts() {
        // 形状对但数值越界：LLM 的常见笔误，放过就等于给一个注定 0 结果的过滤器。
        assert!(normalize_timestamp("2026-13-45").is_err());
        assert!(normalize_timestamp("2026-00-10").is_err());
        assert!(normalize_timestamp("2026-09-00").is_err());
        assert!(normalize_timestamp("0000-01-01").is_err());
        assert!(normalize_timestamp("2026-09-23 24:00").is_err());
        assert!(normalize_timestamp("2026-09-23 11:60").is_err());
        assert!(normalize_timestamp("2026-09-23 11:18:99").is_err());
        assert!(normalize_timestamp("2026-09-23 11:18:31:00").is_err());
        // 带符号的数字段不该被 str::parse 放过。
        assert!(normalize_timestamp("2026-09-23 +11:18").is_err());
        assert!(normalize_timestamp("+2026-09-23").is_err());
    }

    #[test]
    fn unix_seconds_round_trip_through_local_civil() {
        // 与具体时区无关：本地时间串 → Unix 秒 → 本地时间串必须原样回来。
        let civil = "2026-09-23 11:18:44";
        let unix = local_civil_to_unix(civil).expect("应能换算");
        let back = normalize_timestamp(&unix.to_string()).expect("应能解析回来");
        assert_eq!(back, civil);
    }

    #[test]
    fn prefix_normalization_handles_separators_and_root() {
        assert_eq!(normalize_prefix("D:\\src"), "D:\\src");
        assert_eq!(normalize_prefix("D:/src/"), "D:\\src");
        assert_eq!(normalize_prefix("D:"), "D:\\");
        assert_eq!(normalize_prefix("D:\\src\\\\repo"), "D:\\src\\repo");
    }

    #[test]
    fn filter_matches_action_path_name_and_range() {
        let c = parse_line(SAMPLE_CREATED).unwrap();
        let mut f = Filter {
            max_results: 10,
            ..Default::default()
        };
        assert!(matches_filter(&c, &f, None, None));

        f.action = Some(Action::Deleted);
        assert!(!matches_filter(&c, &f, None, None));

        f.action = Some(Action::Created);
        assert!(matches_filter(
            &c,
            &f,
            Some("d:\\source\\repos\\everything-mcp\\.sdk-cache"),
            None
        ));
        assert!(!matches_filter(&c, &f, Some("c:\\windows"), None));
        assert!(matches_filter(&c, &f, None, Some("zzcreate")));
        assert!(!matches_filter(&c, &f, None, Some("nope")));

        f.since = Some("2026-09-23 11:18:44".to_string());
        assert!(matches_filter(&c, &f, None, None));
        f.since = Some("2026-09-23 11:18:45".to_string());
        assert!(!matches_filter(&c, &f, None, None));

        f.since = None;
        f.until = Some("2026-09-23 11:18:43".to_string());
        assert!(!matches_filter(&c, &f, None, None));
    }

    #[test]
    fn name_filter_also_matches_new_name() {
        let c = parse_line(SAMPLE_RENAMED).unwrap();
        assert!(matches_filter(
            &c,
            &Filter::default(),
            None,
            Some("zzrename")
        ));
        assert!(matches_filter(
            &c,
            &Filter::default(),
            None,
            Some("zzcreate")
        ));
    }

    #[test]
    fn day_from_filename_extracts_date_and_rejects_noise() {
        assert_eq!(
            day_from_filename("index-journal-2026-09-23.txt").as_deref(),
            Some("2026-09-23")
        );
        assert_eq!(day_from_filename("index-journal-2026-09-2.txt"), None);
        assert_eq!(day_from_filename("other-2026-09-23.txt"), None);
        assert_eq!(day_from_filename("index-journal-2026-09-23.log"), None);
    }

    #[test]
    fn ini_value_prefers_everything_section() {
        let text = "[Everything]\r\njournal=1\r\njournal_log=1\r\njournal_log_directory=\r\n";
        assert_eq!(ini_value(text, "journal_log").as_deref(), Some("1"));
        // 空值视为未设置，默认目录才生效。
        assert_eq!(ini_value(text, "journal_log_directory"), None);
        assert_eq!(ini_value(text, "nope"), None);
    }

    #[test]
    fn ini_value_ignores_other_sections_when_present() {
        let text = "[Plugins]\r\njournal_log=D:\\bogus\r\n[Everything]\r\njournal_log=1\r\n";
        // [Everything] 段命中，不会误取 [Plugins] 段的值。
        assert_eq!(ini_value(text, "journal_log").as_deref(), Some("1"));
    }

    /// 手工验收：对着本机真实的 Everything 日志跑一遍。需要本机开着
    /// `journal_log`，所以默认不跑 —— `cargo test -- --ignored` 手动触发。
    ///
    /// 它验证两件在单元测试里验证不了的事：解析器吃得下 Everything 真实写出的
    /// 十几万行；倒序分块真的只啃了文件尾部（`bytes_scanned` 远小于文件大小）。
    #[test]
    #[ignore = "需要本机开着 journal_log 的 Everything"]
    fn query_against_real_log_reports_tail_scan_budget() {
        let dir = log_directory();
        let (_, newest) = day_files(&dir)
            .into_iter()
            .next()
            .expect("应有最近的日志文件 —— 请先在 Everything 里开启 journal_log");
        let file_len = fs::metadata(&newest).unwrap().len();

        let filter = Filter {
            max_results: 50,
            ..Default::default()
        };
        let out = query(&filter).expect("查询不该失败");
        assert_eq!(out.count, 50, "未过滤时应直接取满 50 条");
        // 倒序扫描只该啃到尾部一小段：256 KiB 一块，50 条最多再翻几块。
        assert!(
            out.bytes_scanned < file_len / 4,
            "读入 {} 字节 vs 日志 {} 字节 —— 倒序扫描没起作用",
            out.bytes_scanned,
            file_len
        );
        // 最新的在最前面。
        let dates: Vec<&str> = out.changes.iter().map(|c| c.date.as_str()).collect();
        let mut sorted = dates.clone();
        sorted.sort_unstable();
        sorted.reverse();
        assert_eq!(dates, sorted, "应按时间倒序返回");
        // 真实日志里不该有大面积解析失败。
        assert!(
            out.skipped_lines < 10,
            "解析失败 {} 行 —— 格式假设可能不再成立",
            out.skipped_lines
        );
    }
}
