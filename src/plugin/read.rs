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

use std::path::Path;

use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};

/// 单次读取的字节上限。超过直接报错。
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// 单次返回的字节上限 —— 行数没超但行很长时（压缩过的 js / 单行日志）兜底。
pub const MAX_WINDOW_BYTES: usize = 512 * 1024;

/// 默认返回的最大行数。
pub const DEFAULT_MAX_LINES: usize = 200;

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
    /// 是否还有更多内容没返回（行窗口截断或字节上限截断）。
    pub truncated: bool,
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
fn read_text(path: &str) -> Result<(String, &'static str), String> {
    let raw = std::fs::read(path).map_err(|e| format!("cannot read {:?}: {}", path, e))?;
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

/// 按行窗口切出要返回的文本。
///
/// 返回 (窗口文本, 总行数, 窗口行数, 是否被行窗口截断)。
fn window_lines(text: &str, start_line: usize, max_lines: usize) -> (String, usize, usize, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    // start_line 是 1 起的行号；超出末尾时给出空窗口而不是报错。
    let start = start_line.saturating_sub(1).min(total);
    let end = if max_lines == 0 {
        total
    } else {
        start.saturating_add(max_lines).min(total)
    };
    let window = lines[start..end].join("\n");
    (window, total, end - start, end < total)
}

/// 按字节上限截断窗口（压缩过的单行文件可能很长）。
fn clamp_bytes(mut window: String) -> (String, bool) {
    if window.len() <= MAX_WINDOW_BYTES {
        return (window, false);
    }
    // 截到字符边界，避免把多字节 UTF-8 序列切一半。
    let mut cut = MAX_WINDOW_BYTES;
    while cut > 0 && !window.is_char_boundary(cut) {
        cut -= 1;
    }
    window.truncate(cut);
    (window, true)
}

/// 读取文件内容（`read_file` 工具的入口）。
///
/// `start_line` 是 1 起的起始行号；`max_lines` 为 0 表示不限行数
/// （仍受 [`MAX_WINDOW_BYTES`] 约束）。
pub fn read_file(path: &str, start_line: usize, max_lines: usize) -> Result<FileContent, String> {
    let path = validate_path(path)?;

    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read {:?}: {}", path, e))?;
    if meta.is_dir() {
        return Err(format!(
            "{:?} is a folder; use list_folder to list its contents",
            path
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{:?} is {} bytes, over read_file's {} byte limit; \
             use search_in_folder to narrow down to a smaller file, or read it outside this tool",
            path,
            meta.len(),
            MAX_FILE_BYTES
        ));
    }

    let (text, encoding) = read_text(&path)?;
    // 二进制文件会带 NUL 字节 —— 报错而不是把乱码塞给调用方。
    if text.as_bytes().contains(&0) {
        return Err(format!(
            "{:?} looks like a binary file (contains NUL bytes); read_file only returns text",
            path
        ));
    }

    let (window, total_lines, lines_returned, truncated_by_lines) =
        window_lines(&text, start_line, max_lines);
    let (text, truncated_by_bytes) = clamp_bytes(window);

    Ok(FileContent {
        path,
        size: meta.len(),
        total_lines,
        start_line,
        lines_returned,
        truncated: truncated_by_lines || truncated_by_bytes,
        encoding,
        text,
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
    fn window_lines_slices_by_one_based_line_number() {
        let text = "a\nb\nc\nd\ne";
        // 全部
        let (w, total, n, cut) = window_lines(text, 1, 0);
        assert_eq!((w.as_str(), total, n, cut), ("a\nb\nc\nd\ne", 5, 5, false));
        // 中间一段
        let (w, total, n, cut) = window_lines(text, 2, 2);
        assert_eq!((w.as_str(), total, n, cut), ("b\nc", 5, 2, true));
        // 尾部
        let (w, _, n, cut) = window_lines(text, 5, 10);
        assert_eq!((w.as_str(), n, cut), ("e", 1, false));
        // 起点超出末尾 —— 空窗口而不是报错
        let (w, total, n, cut) = window_lines(text, 99, 10);
        assert_eq!((w.as_str(), total, n, cut), ("", 5, 0, false));
    }

    #[test]
    fn window_lines_tolerates_crlf_and_missing_trailing_newline() {
        let (w, total, n, _) = window_lines("a\r\nb\r\n", 1, 0);
        assert_eq!((w.as_str(), total, n), ("a\nb", 2, 2));
    }

    #[test]
    fn clamp_bytes_cuts_on_char_boundary() {
        let small = "x".repeat(10);
        assert_eq!(clamp_bytes(small), ("x".repeat(10), false));
        // 每个字符 3 字节，上限必然落在字符中间 —— 必须回退到边界而不是 panic
        let big = "中".repeat(MAX_WINDOW_BYTES / 3 + 10);
        let (out, cut) = clamp_bytes(big);
        assert!(cut);
        assert!(out.len() <= MAX_WINDOW_BYTES);
        assert!(out.chars().all(|c| c == '中'));
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
}
