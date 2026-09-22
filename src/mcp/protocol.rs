//! protocol.rs — JSON-RPC 2.0 + MCP 类型
//!
//! MCP 规范版本：双时代（dual-era）实现，同一端点同时服务两代客户端：
//!   - 2024-11-05（legacy）：initialize 握手协商版本；
//!   - 2026-07-28（modern，当前最新修订版）：无握手，每个请求在 `_meta` 的
//!     `io.modelcontextprotocol/protocolVersion` 键里自带版本（Streamable
//!     HTTP 上还必须在 `MCP-Protocol-Version` 请求头里带同一个值）。
//! 我们实现的子集：
//!   - initialize / initialized 握手（legacy 客户端）
//!   - server/discover（modern 客户端探测支持版本与能力，2026-07-28 起 MUST）
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

// MCP 协议自定义错误码（2026-07-28 起由规范定义）
/// HTTP 镜像请求头与请求体不一致（如 MCP-Protocol-Version 头与 _meta 版本不符）。
pub const HEADER_MISMATCH: i32 = -32020;
/// 客户端请求的协议版本本服务端不支持；data.supported 列出可用版本。
pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

// ====================================================================
// MCP 协议版本（dual-era）
// ====================================================================

/// legacy 时代：initialize 握手，2024-11-05 修订版。
pub const PROTOCOL_VERSION_LEGACY: &str = "2024-11-05";
/// modern 时代：逐请求 _meta 版本声明，2026-07-28 修订版（当前最新）。
pub const PROTOCOL_VERSION_MODERN: &str = "2026-07-28";
/// 本服务端支持的协议版本。同时用于 discover 结果与 -32022 错误的
/// `data.supported` 列表 —— 客户端从中选一个重试即可。
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = [PROTOCOL_VERSION_LEGACY, PROTOCOL_VERSION_MODERN];

/// `_meta` 中承载协议版本的键（2026-07-28）。
pub const META_PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";

/// 是否是 modern 时代的版本声明（决定逐请求语义）。
pub fn is_modern_version(version: &str) -> bool {
    version == PROTOCOL_VERSION_MODERN
}

/// 从请求 params 的 `_meta` 里取出客户端声明的协议版本。
pub fn requested_version_from_meta(params: &Value) -> Option<String> {
    params
        .get("_meta")
        .and_then(|m| m.get(META_PROTOCOL_VERSION_KEY))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

// ====================================================================
// MCP 协议层
// ====================================================================

/// initialize 响应中的 capabilities（我们宣告：只支持 tools）。
///
/// legacy 语义：回显客户端请求且我们支持的版本；客户端请求了我们不支持的
/// 版本（或没带版本）时退回 2024-11-05，由客户端决定是否断开。
pub fn make_initialize_result(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSION_LEGACY);
    serde_json::json!({
        "protocolVersion": version,
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

/// server/discover 响应（2026-07-28，modern 客户端探测用）。
///
/// 一次性给出支持的协议版本、能力与服务器身份，让客户端在发任何业务请求
/// 之前完成时代判定与版本选择。注意 serverInfo 放在 `_meta` 里（规范如此）。
pub fn make_discover_result() -> Value {
    serde_json::json!({
        "resultType": "complete",
        "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "_meta": {
            "io.modelcontextprotocol/serverInfo": {
                "name": "everything-mcp",
                "version": "1.0.0"
            }
        },
        "instructions": "Folder-scoped Everything file search. Use search_in_folder with an absolute folder path plus Everything search syntax; list_folder for immediate children; count for totals only."
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
    error_response_with_data(id, code, message, Value::Null)
}

/// 构造带 `data` 字段的 JSON-RPC 错误响应（版本协商等场景需要结构化数据）。
pub fn error_response_with_data(
    id: &Option<Value>,
    code: i32,
    message: &str,
    data: Value,
) -> JsonRpcMessage {
    JsonRpcMessage {
        jsonrpc: "2.0".into(),
        id: id.clone(),
        method: None,
        params: None,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
            data: if data.is_null() { None } else { Some(data) },
        }),
    }
}

/// -32022 UnsupportedProtocolVersionError：客户端请求的协议版本不支持。
/// `data.supported` 列出本服务端支持的版本，客户端应从中选一个重试。
pub fn unsupported_version_error(id: &Option<Value>, requested: &str) -> JsonRpcMessage {
    error_response_with_data(
        id,
        UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version",
        serde_json::json!({
            "supported": SUPPORTED_PROTOCOL_VERSIONS,
            "requested": requested,
        }),
    )
}

/// -32020 HeaderMismatch：HTTP 镜像请求头与请求体不一致。
pub fn header_mismatch_error(id: &Option<Value>, message: &str) -> JsonRpcMessage {
    error_response(id, HEADER_MISMATCH, message)
}
