//! tools.rs — 工具实现
//!
//! 把 JSON-RPC 调用分发到具体逻辑，调用插件搜索层。
//! 入参先过 `validate` 规范化/校验：错误消息带范例，LLM 客户端拿到
//! INVALID_PARAMS 就能一次改对，不会带着坏参数盲目重试。

use serde_json::{json, Value};

use crate::mcp::protocol::{INVALID_PARAMS, METHOD_NOT_FOUND};
use crate::mcp::validate;
use crate::plugin;

/// 工具调用返回：（content 数组, is_error）
/// MCP 协议要求工具调用结果以 `content` 数组返回，每项是 `{type, text}`。
pub type ToolOutput = (Value, bool);

/// 取 folder 参数：必须存在，且规范化后是合法绝对路径。
fn require_folder(args: &Value) -> Result<String, (i32, String)> {
    let raw = args
        .get("folder")
        .and_then(Value::as_str)
        .ok_or((INVALID_PARAMS, validate::MISSING_FOLDER_MSG.to_string()))?;
    validate::normalize_folder(raw).map_err(|e| (INVALID_PARAMS, e))
}

/// 取 pattern 参数：可缺省（空 = 列出全部），但不能含控制字符。
/// shell  globstar 前缀（`**/`、`**\`、`./`）在这里翻译成 Everything 语义。
fn pattern_arg(args: &Value) -> Result<String, (i32, String)> {
    let raw = args.get("pattern").and_then(Value::as_str).unwrap_or("");
    let p = validate::validate_pattern(raw).map_err(|e| (INVALID_PARAMS, e))?;
    Ok(validate::translate_globstar(&p))
}

/// 取 exclude 参数：字符串或字符串数组，每项作为一条 Everything NOT 项。
///
/// 仓库噪声（`.git` / `obj` / `node_modules` / `target`）不该进搜索结果 ——
/// 评测里 `*.cs` 前 50 条被 `obj\...` 占满、count 656 含生成代码，都是这个。
/// 调用方可以逐项写 `!term`（pattern 里），也可以用这个参数显式排除。
fn exclude_arg(args: &Value) -> Result<Vec<String>, (i32, String)> {
    let mut raw_terms: Vec<String> = Vec::new();
    match args.get("exclude") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => raw_terms.push(s.clone()),
        Some(Value::Array(arr)) => {
            for v in arr {
                match v.as_str() {
                    Some(s) => raw_terms.push(s.to_string()),
                    None => {
                        return Err((
                            INVALID_PARAMS,
                            format!("every item of 'exclude' must be a string; received {}", v),
                        ))
                    }
                }
            }
        }
        Some(v) => {
            return Err((
                INVALID_PARAMS,
                format!(
                    "'exclude' must be a string or an array of strings; received {}",
                    v
                ),
            ))
        }
    }

    let mut terms = Vec::with_capacity(raw_terms.len());
    for t in &raw_terms {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if t.chars().any(|c| c.is_control()) {
            return Err((
                INVALID_PARAMS,
                format!(
                    "'exclude' must not contain control characters; received {:?}",
                    t
                ),
            ));
        }
        terms.push(t.to_string());
    }
    Ok(terms)
}

/// 把 pattern 与 exclude 项拼成最终 Everything 搜索词。
///
/// exclude 每项变成 NOT 项（`!term`）—— 与评测建议的
/// `ext:cs !\obj\ !\.git\` 同形。pattern 为空时只留排除项。
fn combine_query(pattern: &str, excludes: &[String]) -> String {
    let mut out = String::new();
    if !pattern.is_empty() {
        out.push_str(pattern);
    }
    for e in excludes {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('!');
        out.push_str(e);
    }
    out
}

/// 取非负整数参数：缺省用默认值；类型不符（负数/小数/字符串）报 INVALID_PARAMS。
fn u64_arg(args: &Value, key: &str, default: u64) -> Result<u64, (i32, String)> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().ok_or((
            INVALID_PARAMS,
            format!("'{}' must be a non-negative integer; received {}", key, v),
        )),
    }
}

/// 分发工具调用。`name` 是工具名，`args` 是参数对象。
pub fn dispatch(name: &str, args: &Value) -> Result<ToolOutput, (i32, String)> {
    match name {
        "search_in_folder" => {
            let folder = require_folder(args)?;
            let pattern = pattern_arg(args)?;
            let excludes = exclude_arg(args)?;
            let query = combine_query(&pattern, &excludes);
            let max_results = u64_arg(args, "max_results", 50)? as usize;
            let timeout_ms = u64_arg(args, "timeout_ms", 10_000)? as u32;

            match plugin::search::search_in_folder(
                &folder,
                &query,
                max_results,
                timeout_ms,
                plugin::search::SearchScope::Recursive,
            ) {
                Ok(outcome) => {
                    let entries: Vec<Value> = outcome
                        .results
                        .iter()
                        .map(|r| {
                            let kind = if r.is_folder { "folder" } else { "file" };
                            json!({
                                "name": r.name,
                                "path": r.path,
                                "kind": kind,
                                "size": r.size,
                            })
                        })
                        .collect();
                    let text = serde_json::to_string_pretty(&json!({
                        "folder": folder,
                        "pattern": pattern,
                        "exclude": excludes,
                        "count": entries.len(),
                        "total": outcome.total,
                        "results": entries,
                    }))
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());

                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("search error: {}", e)), true)),
            }
        }

        "list_folder" => {
            let folder = require_folder(args)?;
            let excludes = exclude_arg(args)?;
            let query = combine_query("", &excludes);
            // 用「直接子项」范围（SearchScope::Children → parent:"<folder>"）：
            // 无排除项时 query 为空串，等价于列出该目录全部直接子项，不递归。
            match plugin::search::search_in_folder(
                &folder,
                &query,
                500,
                10_000,
                plugin::search::SearchScope::Children,
            ) {
                Ok(outcome) => {
                    let entries: Vec<Value> = outcome
                        .results
                        .iter()
                        .map(|r| {
                            json!({
                                "name": r.name,
                                "kind": if r.is_folder { "folder" } else { "file" },
                                "size": r.size,
                            })
                        })
                        .collect();
                    let text = serde_json::to_string_pretty(&json!({
                        "folder": folder,
                        "exclude": excludes,
                        "count": entries.len(),
                        "total": outcome.total,
                        "items": entries,
                    }))
                    .unwrap_or_else(|_| "{}".into());
                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("list error: {}", e)), true)),
            }
        }

        "count" => {
            let folder = require_folder(args)?;
            let pattern = pattern_arg(args)?;
            let excludes = exclude_arg(args)?;
            let query = combine_query(&pattern, &excludes);
            // 快速路径：只取 db_query_get_result_count 的计数值，
            // 不为每条结果拼 name/path 字符串。计数与 search_in_folder
            // 同范围（递归子树）。
            match plugin::search::count_in_folder(&folder, &query, 10_000) {
                Ok(n) => {
                    let text = format!(
                        "{{\"folder\":{:?},\"pattern\":{:?},\"count\":{}}}",
                        folder, pattern, n
                    );
                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("count error: {}", e)), true)),
            }
        }

        other => Err((METHOD_NOT_FOUND, format!("unknown tool: {}", other))),
    }
}

/// 构造 MCP 协议要求的 `content` 数组（单条文本项）。
fn content_text(text: &str) -> Value {
    json!({
        "content": [
            { "type": "text", "text": text }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn combine_without_excludes_is_pattern_verbatim() {
        assert_eq!(combine_query("ext:rs", &[]), "ext:rs");
        assert_eq!(combine_query("", &[]), "");
    }

    #[test]
    fn combine_appends_not_terms() {
        assert_eq!(
            combine_query("ext:cs", &v(&[r"\obj\", r"\.git\"])),
            r#"ext:cs !\obj\ !\.git\"#
        );
    }

    #[test]
    fn combine_with_empty_pattern_keeps_only_excludes() {
        assert_eq!(combine_query("", &v(&[r"\obj\"])), r#"!\obj\"#);
    }

    #[test]
    fn exclude_arg_accepts_string_and_array() {
        let one = json!({ "exclude": r"\obj\" });
        assert_eq!(exclude_arg(&one).unwrap(), v(&[r"\obj\"]));
        let many = json!({ "exclude": [r"\obj\", "  ", r"\target\"] });
        assert_eq!(
            exclude_arg(&many).unwrap(),
            v(&[r"\obj\", r"\target\"]),
            "空串项应被忽略"
        );
        assert!(exclude_arg(&json!({})).unwrap().is_empty());
        assert!(exclude_arg(&json!({ "exclude": null })).unwrap().is_empty());
    }

    #[test]
    fn exclude_arg_rejects_bad_shapes_and_control_chars() {
        let e = exclude_arg(&json!({ "exclude": 42 })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        let e = exclude_arg(&json!({ "exclude": [1, "ok"] })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        assert!(e.1.contains("every item"));
        let e = exclude_arg(&json!({ "exclude": "a\tb" })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
    }

    #[test]
    fn pattern_arg_translates_globstar_prefix() {
        assert_eq!(
            pattern_arg(&json!({ "pattern": "**/*.cs" })).unwrap(),
            "*.cs"
        );
        assert_eq!(
            pattern_arg(&json!({ "pattern": "  **\\*.cs  " })).unwrap(),
            "*.cs"
        );
        // 中间的 globstar 无法忠实翻译，原样保留。
        assert_eq!(
            pattern_arg(&json!({ "pattern": "src/**/*.cs" })).unwrap(),
            "src/**/*.cs"
        );
    }
}
