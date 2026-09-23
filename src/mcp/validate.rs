//! validate.rs — 工具入参校验与路径规范化
//!
//! MCP 客户端是 LLM，传来的参数常带「人类书写」的痕迹：包裹引号、正斜杠、
//! 双反斜杠、尾部分隔符。Everything 的搜索语法按字面匹配路径，
//! 这些差异会直接导致 0 结果且无从诊断。这里在触达主程序之前统一收拾干净，
//! 收拾不了的给出带范例的错误消息 —— 让 LLM 一次改对，而不是盲目重试。

/// 缺 folder 参数时的错误消息（顺便告诉调用方期望的格式）。
pub const MISSING_FOLDER_MSG: &str = "missing 'folder': expected an absolute path like 'C:\\Users\\me\\project' or a UNC path like '\\\\server\\share'";

/// 规范化并校验 folder 参数。
///
/// 依次处理：去首尾空白与成对包裹引号 → 拒空/控制字符/内部引号/通配符 →
/// 正斜杠转反斜杠 → UNC 与普通路径分别折叠连续反斜杠 → 去尾斜杠
/// （保留 `C:\` 盘符根与 `\\server\share` 形态）→ 校验盘符绝对路径。
///
/// 返回值即可直接拼进 Everything 搜索语法的字面路径。
pub fn normalize_folder(input: &str) -> Result<String, String> {
    let mut s = input.trim();
    // LLM 常把路径带引号传来 —— 去掉成对的包裹引号。
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].trim();
    }
    if s.is_empty() {
        return Err(MISSING_FOLDER_MSG.to_string());
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(format!(
            "folder must not contain control characters; received {:?}",
            input
        ));
    }
    if s.contains('"') {
        return Err(format!(
            "folder must not contain double quotes (they would break the search syntax); received {:?}",
            input
        ));
    }
    if s.contains('*') || s.contains('?') {
        return Err(format!(
            "folder must be a literal path without wildcards (* or ?); put wildcards in 'pattern' instead; received {:?}",
            input
        ));
    }

    // 统一分隔符：Windows 接受正斜杠，Everything 的路径语法用反斜杠。
    let unified = s.replace('/', "\\");

    if unified.starts_with("\\\\") {
        // UNC：保留前导双反斜杠，折叠其余连续反斜杠。
        let mut out = String::from("\\\\");
        let mut prev_bs = true;
        for c in unified[2..].chars() {
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
        // 去尾斜杠，但至少保留 \\server\share 两段。
        while out.ends_with('\\') && out.len() > 2 {
            out.pop();
        }
        if out.matches('\\').count() < 3 {
            return Err(format!(
                "folder must be a full UNC path like '\\\\server\\share'; received {:?}",
                input
            ));
        }
        return Ok(out);
    }

    // 普通路径：折叠连续反斜杠（LLM 常见的转义失误）。
    let mut out = String::with_capacity(unified.len());
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
    // 盘符校验：X: 开头。
    let bytes = out.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(format!(
            "folder must be an absolute path like 'C:\\Users\\me\\project' or a UNC path like '\\\\server\\share'; received {:?}",
            input
        ));
    }
    if out.len() == 2 {
        out.push('\\'); // "C:" → "C:\"
    }
    // X: 后必须紧跟反斜杠 —— 拒绝 "C:foo" 这类相对盘符路径。
    if out.as_bytes().get(2) != Some(&b'\\') {
        return Err(format!(
            "folder must start with a drive letter and backslash like 'C:\\Users\\me\\project'; received {:?}",
            input
        ));
    }
    // 去尾斜杠，但保留盘符根 C:\。
    while out.ends_with('\\') && out.len() > 3 {
        out.pop();
    }
    Ok(out)
}

/// 校验 pattern（Everything 搜索语法）并去掉首尾空白。
///
/// 空 pattern 合法 —— 表示列出文件夹下全部条目。这里只挡控制字符。
pub fn validate_pattern(input: &str) -> Result<String, String> {
    let p = input.trim();
    if p.chars().any(|c| c.is_control()) {
        return Err(format!(
            "pattern must not contain control characters; received {:?}",
            input
        ));
    }
    Ok(p.to_string())
}

/// 把 shell/ripgrep 风格的 globstar 与 `./` 前缀翻译成 Everything 语义。
///
/// LLM 常直接写 `**/*.cs` —— Everything 没有 globstar 语法，原样传会得到
/// 0 条且无从诊断（评测里正是这么踩坑的）。本插件的搜索以 folder 为根递归
/// （`"<folder>\"` 路径前缀），`**/` 想表达的「整棵子树」恰是默认语义，
/// 所以剥掉开头的 globstar 即可，其余部分原样保留。
///
/// 只处理**开头**的 globstar：`src/**/*.cs` 这类中间 globstar 无法忠实
/// 翻译（Everything 无对应概念），原样返回 —— 客户端会看到 0 条和
/// `total: 0`，比悄悄改错语义好。
pub fn translate_globstar(pattern: &str) -> String {
    let mut p = pattern.trim();
    loop {
        if let Some(rest) = p.strip_prefix("**/").or_else(|| p.strip_prefix("**\\")) {
            p = rest.trim_start();
            continue;
        }
        if let Some(rest) = p.strip_prefix("./").or_else(|| p.strip_prefix(".\\")) {
            p = rest;
            continue;
        }
        break;
    }
    p.to_string()
}

/// 校验全局搜索（search_everywhere）的 pattern —— 文件夹范围之外的额外护栏。
///
/// 全局搜索的暴露面是整个索引，pattern 再放任下去就是「列出全盘」：
///   - 去掉空白后至少 2 个字符 —— 空 / 单字符 pattern 的命中量是灾难级的，
///     全局场景下没有「列出全部」的合法需求（那属于 list_folder）；
///   - 禁止 `content:` —— 正文检索必须收窄到文件夹（search_in_folder），
///     全局 `content:` 等于读遍所有已索引磁盘上的所有文件，慢且危险。
///     大小写、`case:content:` 之类的搜索函数链写法一并挡掉（统一小写查子串；
///     Windows 文件名里不允许冒号，`content:` 出现在 pattern 里只可能是搜索函数）。
///
/// 入参应当已经过 [`validate_pattern`] + [`translate_globstar`]。
pub fn validate_global_pattern(pattern: &str) -> Result<String, String> {
    let p = pattern.trim();
    if p.chars().count() < 2 {
        return Err(format!(
            "global 'pattern' must be at least 2 characters — a global search needs a name to look for (e.g. '*.vhd', '\"quarterly report\"'); received {:?}",
            pattern
        ));
    }
    if p.to_ascii_lowercase().contains("content:") {
        return Err(format!(
            "global 'pattern' must not use 'content:' — a full-disk content scan is too slow and too broad; use search_in_folder with a folder to narrow the scope; received {:?}",
            pattern
        ));
    }
    Ok(p.to_string())
}
