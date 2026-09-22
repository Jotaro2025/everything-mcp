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
fn pattern_arg(args: &Value) -> Result<String, (i32, String)> {
    let raw = args.get("pattern").and_then(Value::as_str).unwrap_or("");
    validate::validate_pattern(raw).map_err(|e| (INVALID_PARAMS, e))
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
            let max_results = u64_arg(args, "max_results", 50)? as usize;
            let timeout_ms = u64_arg(args, "timeout_ms", 10_000)? as u32;

            match plugin::search::search_in_folder(
                &folder,
                &pattern,
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
            // 用「直接子项」范围（SearchScope::Children → parent:"<folder>"）：
            // 空 pattern 等价于列出该目录全部直接子项，不递归。
            match plugin::search::search_in_folder(
                &folder,
                "",
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
            // 快速路径：只取 db_query_get_result_count 的计数值，
            // 不为每条结果拼 name/path 字符串。计数与 search_in_folder
            // 同范围（递归子树）。
            match plugin::search::count_in_folder(&folder, &pattern, 10_000) {
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
