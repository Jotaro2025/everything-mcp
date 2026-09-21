//! protocol.rs — JSON-RPC 2.0 + MCP 类型
//!
//! MCP 规范版本：2024-11-05 (Streamable HTTP transport)。
//! 我们实现的子集：
//!   - initialize / initialized 握手
//!   - tools/list 返回工具清单
//!   - tools/call 调用工具
//!   - 通知（无 id 的请求）：notifications/initialized
//!
//! 传输层：单 POST / 单 GET 长连接（我们只支持单次 POST → 响应模式，
//! 因为 LLM 客户端轮询发起工具调用足够使用）。

// 标准错误码全部列出以保持协议完整性；暂未使用的允许 dead_code。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ====================================================================
// JSON-RPC 2.0 信封
// ====================================================================

/// JSON-RPC 请求/响应的公共信封字段。
/// `id` 仅在请求/响应中存在；通知（notification）没有 id。
/// 我们用一个统一的 `Request` 类型承载所有方向的消息，
/// 通过 `id` 是否为 `None` 区分。
#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcMessage {
    #[serde(rename = "jsonrpc")]
    pub jsonrpc: String,
    /// 请求 id（数字或字符串）。`None` 表示这是一条通知。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 方法名（仅请求/通知）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 参数（任意 JSON 值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// 响应结果（仅响应）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 响应错误（仅响应失败时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// 标准错误码
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

// ====================================================================
// MCP 协议层
// ====================================================================

/// initialize 响应中的 capabilities（我们宣告：只支持 tools）。
pub fn make_initialize_result(client_caps: &Value) -> Value {
    let _ = client_caps;
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "everything-mcp",
            "version": "1.0.0"
        }
    })
}

/// tools/list 响应：列出我们暴露的全部工具。
pub fn make_tools_list() -> Value {
    serde_json::json!({
        "tools": [
            {
                "name": "search_in_folder",
                "description": "Search files/folders under a specific folder using Everything search syntax. Prefer this over global search when working within a project directory.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": {
                            "type": "string",
                            "description": "Absolute path to the folder to search in, e.g. D:\\\\source\\\\repos\\\\myproject"
                        },
                        "pattern": {
                            "type": "string",
                            "description": "Everything search pattern. Examples: '*.rs' (extension), 'readme' (substring), 'ext:md;txt' (multiple extensions), '\"exact phrase\"'. Empty pattern lists all files."
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum number of results to return. 0 = no limit (default 50).",
                            "default": 50
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "Maximum time in milliseconds to wait for results (default 10000).",
                            "default": 10000
                        }
                    },
                    "required": ["folder", "pattern"]
                }
            },
            {
                "name": "list_folder",
                "description": "List immediate children of a folder (non-recursive). Returns both files and sub-folders with sizes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string", "description": "Absolute folder path" }
                    },
                    "required": ["folder"]
                }
            },
            {
                "name": "count",
                "description": "Count files matching a pattern within a folder, without fetching names. Useful for quick metrics.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string" },
                        "pattern": { "type": "string", "default": "" }
                    },
                    "required": ["folder"]
                }
            }
        ]
    })
}

/// 构造一个标准的 JSON-RPC 成功响应。
pub fn ok_response(id: &Option<Value>, result: Value) -> JsonRpcMessage {
    JsonRpcMessage {
        jsonrpc: "2.0".into(),
        id: id.clone(),
        method: None,
        params: None,
        result: Some(result),
        error: None,
    }
}

/// 构造一个标准的 JSON-RPC 错误响应。
pub fn error_response(id: &Option<Value>, code: i32, message: &str) -> JsonRpcMessage {
    JsonRpcMessage {
        jsonrpc: "2.0".into(),
        id: id.clone(),
        method: None,
        params: None,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
            data: None,
        }),
    }
}
