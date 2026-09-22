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
//! resultType（2026-07-28）：modern 时代的每个 result 都必须带
//! `resultType: "complete"`（规范 MUST，缺失会被客户端判为无效结果）；
//! legacy 响应保持 2024-11-05 原形状，客户端按 absent-means-complete
//! 桥接规则把缺失当作 complete。
//!
//! 缓存提示（2026-07-28）：`ListToolsResult` / `DiscoverResult` 继承
//! `CacheableResult`，`ttlMs` 与 `cacheScope` 同为必填字段（语义类比 HTTP
//! `Cache-Control: max-age` 与 public/private），modern 时代的这两个结果
//! 必须带上，缺失同样会被客户端判为无效结果。
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

/// modern 时代 result 的类型声明值。本服务端的所有结果都是完整的最终内容
/// （不支持分页，也不发 MRTR 的 input_required），统一用 "complete"。
pub const RESULT_TYPE_COMPLETE: &str = "complete";

/// tools/list 结果的缓存提示（`CacheableResult.ttlMs`，单位毫秒，语义类比
/// HTTP `Cache-Control: max-age`）。工具清单基本不变（`listChanged: false`），
/// 但插件升级会增减工具，取 5 分钟的保守值 —— 与规范示例一致。
pub const TOOLS_LIST_TTL_MS: u64 = 300_000;

/// server/discover 结果的缓存提示。支持的协议版本与能力很少变，取 1 小时。
pub const DISCOVER_TTL_MS: u64 = 3_600_000;

/// 缓存范围（`CacheableResult.cacheScope`）。本服务端无鉴权，结果里没有
/// 用户特定数据，任何客户端或中间代理都可以缓存并跨授权上下文复用，
/// 因此一律 "public"。
pub const CACHE_SCOPE_PUBLIC: &str = "public";

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

/// 按时代给 result 补上 `resultType` 字段。
///
/// 2026-07-28 起规范要求每个 result 都带 `resultType`（MUST），modern
/// 客户端缺失即判为无效结果并告警。本服务端只返回完整结果，统一标
/// `"complete"`。
///
/// legacy 时代的规范没有这个字段，客户端对缺失按 "complete" 处理
/// （absent-means-complete 桥接只适用于早期修订版的服务端），因此
/// legacy 响应原样返回 —— 老客户端的 wire 形状保持不变。
/// 非对象结果（理论上是非法的 MCP result）原样透传。
pub fn with_result_type(result: Value, modern: bool) -> Value {
    if !modern {
        return result;
    }
    match result {
        Value::Object(mut map) => {
            map.insert(
                "resultType".to_string(),
                Value::String(RESULT_TYPE_COMPLETE.to_string()),
            );
            Value::Object(map)
        }
        other => other,
    }
}

/// 按时代给 result 补上 `CacheableResult` 的缓存提示字段。
///
/// 2026-07-28 起 `ListToolsResult` / `DiscoverResult` 继承 `CacheableResult`，
/// `ttlMs`（number）与 `cacheScope`（"public" | "private"）是必填字段，
/// modern 客户端缺失即判为无效结果。legacy 规范没有这两个字段，响应保持
/// 原形状。非对象结果（理论上是非法的 MCP result）原样透传。
pub fn with_cache_control(result: Value, modern: bool, ttl_ms: u64) -> Value {
    if !modern {
        return result;
    }
    match result {
        Value::Object(mut map) => {
            map.insert("ttlMs".to_string(), Value::from(ttl_ms));
            map.insert(
                "cacheScope".to_string(),
                Value::String(CACHE_SCOPE_PUBLIC.to_string()),
            );
            Value::Object(map)
        }
        other => other,
    }
}

// ====================================================================
// MCP 协议层
// ====================================================================

/// initialize 响应中的 capabilities（我们宣告：只支持 tools）。
///
/// legacy 语义：回显客户端请求且我们支持的版本；客户端请求了我们不支持的
/// 版本（或没带版本）时退回 2024-11-05，由客户端决定是否断开。
/// 回显 modern 版本时响应同样按 modern 规则带 `resultType`。
pub fn make_initialize_result(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSION_LEGACY);
    let result = serde_json::json!({
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
    });
    with_result_type(result, is_modern_version(version))
}

/// server/discover 响应（2026-07-28，modern 客户端探测用）。
///
/// 一次性给出支持的协议版本、能力与服务器身份，让客户端在发任何业务请求
/// 之前完成时代判定与版本选择。注意 serverInfo 放在 `_meta` 里（规范如此）。
/// DiscoverResult 继承 CacheableResult，ttlMs / cacheScope 必填。
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
        "instructions": "Folder-scoped Everything file search. Use search_in_folder with an absolute folder path plus Everything search syntax; list_folder for immediate children; count for totals only. Search results are truncated by max_results and carry a separate 'total' count of all matches found.",
        "ttlMs": DISCOVER_TTL_MS,
        "cacheScope": CACHE_SCOPE_PUBLIC
    })
}

/// tools/list 响应：列出我们暴露的全部工具。
///
/// 结果按 era 中性构造（2024-11-05 原形状）；modern 时代的必填字段
/// （resultType、CacheableResult 的 ttlMs / cacheScope）由调用方过
/// [`with_result_type`] 与 [`with_cache_control`] 补齐。
pub fn make_tools_list() -> Value {
    serde_json::json!({
        "tools": [
            {
                "name": "search_in_folder",
                "description": "Search files/folders recursively under a specific folder using Everything search syntax. Prefer this over global search when working within a project directory. This is not shell glob: use '*.rs', not '**/*.rs' (a leading '**/' is stripped for you); exclude matches with a '!' prefix (e.g. 'ext:rs !test') or the 'exclude' parameter (e.g. ['\\obj\\', '\\.git\\'] to drop build/VCS noise); 'content:\"fn main\"' searches file contents. The response reports both 'count' (entries returned, capped by max_results) and 'total' (all matches found) — when they differ, the results are truncated.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": {
                            "type": "string",
                            "description": "Absolute path to the folder to search in, e.g. D:\\\\source\\\\repos\\\\myproject"
                        },
                        "pattern": {
                            "type": "string",
                            "description": "Everything search pattern. Examples: '*.rs' (extension), 'readme' (substring), 'ext:md;txt' (multiple extensions), '\"exact phrase\"', 'content:\"fn main\"' (content search), 'ext:rs !test' (the '!' prefix excludes matches). Empty pattern lists all files. Shell globs like '**/*.rs' are not supported — the folder scope already restricts the tree."
                        },
                        "exclude": {
                            "type": ["string", "array"],
                            "items": { "type": "string" },
                            "description": "Terms to exclude, appended as Everything NOT operators. Accepts a string or an array of strings. Quoted path fragments match well: ['\\obj\\', '\\.git\\', '\\node_modules\\'] drops build output, VCS metadata and dependencies. Default: none."
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
                "description": "List immediate children of a folder (non-recursive). Returns both files and sub-folders; folder entries always report size 0 (Everything does not compute directory sizes). Use search_in_folder when you need the whole tree. Pass 'exclude' to skip children such as '.git', 'obj', 'node_modules'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string", "description": "Absolute folder path" },
                        "exclude": {
                            "type": ["string", "array"],
                            "items": { "type": "string" },
                            "description": "Terms to exclude, appended as Everything NOT operators. Accepts a string or an array of strings."
                        }
                    },
                    "required": ["folder"]
                }
            },
            {
                "name": "count",
                "description": "Count files/folders matching a pattern within a folder (recursive), without fetching names. Fast path over search_in_folder — no per-entry names or paths are materialized. Supports the same 'exclude' terms as search_in_folder.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string" },
                        "pattern": { "type": "string", "default": "" },
                        "exclude": {
                            "type": ["string", "array"],
                            "items": { "type": "string" },
                            "description": "Terms to exclude, appended as Everything NOT operators. Accepts a string or an array of strings."
                        }
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
