//! tools.rs — 工具实现
//!
//! 把 JSON-RPC 调用分发到具体逻辑，调用插件搜索层。

use serde_json::{json, Value};

use crate::mcp::protocol::INVALID_PARAMS;
use crate::plugin;

/// 工具调用返回：（content 数组, is_error）
/// MCP 协议要求工具调用结果以 `content` 数组返回，每项是 `{type, text}`。
pub type ToolOutput = (Value, bool);

/// 分发工具调用。`name` 是工具名，`args` 是参数对象。
pub fn dispatch(name: &str, args: &Value) -> Result<ToolOutput, (i32, String)> {
    match name {
        "search_in_folder" => {
            let folder = args
                .get("folder")
                .and_then(Value::as_str)
                .ok_or((INVALID_PARAMS, "missing 'folder'".into()))?;
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or("");
            let max_results = args
                .get("max_results")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(50);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(|n| n as u32)
                .unwrap_or(10_000);

            match plugin::search::search_in_folder(folder, pattern, max_results, timeout_ms) {
                Ok(results) => {
                    let total = results.len();
                    let mut entries = Vec::with_capacity(total);
                    for r in &results {
                        let kind = if r.is_folder { "folder" } else { "file" };
                        entries.push(json!({
                            "name": r.name,
                            "path": r.path,
                            "kind": kind,
                            "size": r.size,
                        }));
                    }
                    let text = serde_json::to_string_pretty(&json!({
                        "folder": folder,
                        "pattern": pattern,
                        "count": total,
                        "results": entries,
                    }))
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());

                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("search error: {}", e)), true)),
            }
        }

        "list_folder" => {
            let folder = args
                .get("folder")
                .and_then(Value::as_str)
                .ok_or((INVALID_PARAMS, "missing 'folder'".into()))?;
            // 用 Everything 的「直接子项」语法：parent:"<folder>"
            // 给一个空 pattern 等价于列出该目录全部直接子项。
            match plugin::search::search_in_folder(folder, "", 500, 10_000) {
                Ok(results) => {
                    let entries: Vec<Value> = results
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
                        "items": entries,
                    }))
                    .unwrap_or_else(|_| "{}".into());
                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("list error: {}", e)), true)),
            }
        }

        "count" => {
            let folder = args
                .get("folder")
                .and_then(Value::as_str)
                .ok_or((INVALID_PARAMS, "missing 'folder'".into()))?;
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or("");
            // 我们用 search_in_folder 但只取总数（max_results=0 + 读 count）。
            // 优化：当前 search_in_folder 仍然会读出全部结果填到 Vec，
            // 对于纯计数来说有额外开销，但单次查询本身的耗时主要在搜索而不是读取，
            // 所以暂不专门优化。
            match plugin::search::search_in_folder(folder, pattern, 0, 10_000) {
                Ok(results) => {
                    let text = format!(
                        "{{\"folder\":{:?},\"pattern\":{:?},\"count\":{}}}",
                        folder, pattern, results.len()
                    );
                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("count error: {}", e)), true)),
            }
        }

        other => Err((
            crate::mcp::protocol::METHOD_NOT_FOUND,
            format!("unknown tool: {}", other),
        )),
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
