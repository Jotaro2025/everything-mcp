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
            "version": "1.1.2"
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
                "version": "1.1.2"
            }
        },
        "instructions": "Folder-scoped Everything file search. Search scope follows Everything's index: local drives are included automatically, but a network share is only searchable after being added in Tools > Options > Indexes > Folders — an un-indexed share yields 0 results, not an error. Use search_in_folder with an absolute folder path (a drive path like D:\\source\\repos\\myproject or a UNC path like \\\\server\\share\\project) plus Everything search syntax; list_folder for immediate children; count for totals only; index_changes to read the index journal (created/modified/deleted/renamed); it needs journal_log enabled in Everything and returns an actionable error when it is not. search_everywhere finds files/folders by NAME across the whole index without a folder — for when you know what it is called but not where it lives; it is gated by the server's global-search policy ('deny' rejects every call with GLOBAL_SEARCH_DISABLED, 'review' requires the user to confirm first, 'allow' serves calls directly) and its pattern must be at least 2 characters with no 'content:'. search_in_folder results carry size and modified/created timestamps, can be sorted (sort=name|path|size|modified|created, descending=true for newest/largest first), and support case/whole-word/regex matching. Results are capped by max_results and carry a separate 'total' count of all matches found — when count < total, page through with 'offset'.",
        "ttlMs": DISCOVER_TTL_MS,
        "cacheScope": CACHE_SCOPE_PUBLIC
    })
}

// ====================================================================
// 全局搜索模式（Everything 设置页「全局搜索」的三档）
// ====================================================================

/// search_everywhere 的服务端策略档位（`mcp_global_search` 设置项）。
///
///   - [`GlobalSearchMode::Deny`]（默认）：调用被服务端硬拒
///     （`GLOBAL_SEARCH_DISABLED`）—— 能力不存在，除非装机者显式打开；
///   - [`GlobalSearchMode::Review`]：调用放行，但工具注解标成「非只读 +
///     破坏性」、描述里写明需要用户确认 —— 把关方是**客户端的权限弹窗**
///     （ToolAnnotations 本来就是给客户端决定确认 UX 用的提示）；
///   - [`GlobalSearchMode::Allow`]：调用放行，注解标只读幂等，
///     严谨的客户端可自动放行、不再打扰用户。
///
/// Review 档的确认发生在客户端侧：注解只是 hints，认真的客户端会弹确认框，
/// 忽略注解的客户端会直接放行 —— 服务端真正强制的只有 Deny 这一档，
/// 这也是三档里唯一有硬闸门的。持久化取值：0/1/2。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlobalSearchMode {
    /// 0 —— 拒绝（默认）：调用即报 GLOBAL_SEARCH_DISABLED。
    #[default]
    Deny,
    /// 1 —— 审核：放行调用，靠注解 + 描述让客户端弹确认框。
    Review,
    /// 2 —— 允许：放行调用，注解标只读，可自动放行。
    Allow,
}

impl GlobalSearchMode {
    /// Plugins.ini 里的持久化取值（`mcp_global_search`）。
    pub fn as_int(self) -> i32 {
        match self {
            GlobalSearchMode::Deny => 0,
            GlobalSearchMode::Review => 1,
            GlobalSearchMode::Allow => 2,
        }
    }

    /// 从持久化取值解析。未知取值返回 None（调用方退回默认的 Deny）。
    pub fn from_int(v: i64) -> Option<Self> {
        match v {
            0 => Some(GlobalSearchMode::Deny),
            1 => Some(GlobalSearchMode::Review),
            2 => Some(GlobalSearchMode::Allow),
            _ => None,
        }
    }

    /// 规范名（回显给调用方 / 写进工具描述与诊断日志）。
    pub fn as_str(self) -> &'static str {
        match self {
            GlobalSearchMode::Deny => "deny",
            GlobalSearchMode::Review => "review",
            GlobalSearchMode::Allow => "allow",
        }
    }
}

/// search_everywhere 的工具条目 —— 描述前缀与 ToolAnnotations 随模式变。
///
/// 注解就是本插件的「审核」机制：Allow 档标只读幂等，客户端可自动放行；
/// Review 档标成非只读 + 破坏性 —— 规范里这两个提示的用途正是让客户端把
/// 调用当危险操作弹确认框。它并不真的改磁盘（纯查询），destructiveHint 只是
/// 驱动确认弹窗的手段，描述里写明这一点。Deny 档同 Review 的注解
///（不诱导自动放行），描述里直接说明服务端会拒绝。
fn search_everywhere_entry(mode: GlobalSearchMode) -> Value {
    let policy = match mode {
        GlobalSearchMode::Allow => "POLICY (server setting 'global search' = allow): calls are served directly; the annotations below mark this tool read-only, so clients that trust read-only tools may auto-approve it.",
        GlobalSearchMode::Review => "POLICY (server setting 'global search' = review): before this tool runs, the USER must confirm — the annotations below deliberately mark it as NOT read-only / destructive so the client shows its permission prompt, and you (the model) must not call it unless the user explicitly asked for a global search. Nothing on disk is ever modified; the flag only drives the confirmation prompt.",
        GlobalSearchMode::Deny => "POLICY (server setting 'global search' = deny): this tool is DISABLED — every call returns a GLOBAL_SEARCH_DISABLED error without searching anything. If a global search is needed, ask the user to set Everything > Options > Plugins > MCP > global search to 'review' or 'allow'.",
    };
    let (read_only, destructive) = match mode {
        GlobalSearchMode::Allow => (true, false),
        GlobalSearchMode::Review | GlobalSearchMode::Deny => (false, true),
    };
    serde_json::json!({
        "name": "search_everywhere",
        "description": format!(
            "{} Search file/folder NAMES across the ENTIRE Everything index — every local drive and indexed network share — without naming a folder. Use it when you know what a file is called but not where it lives (e.g. a folder somewhere on a NAS share), then hand the returned full paths to search_in_folder / list_folder for anything scoped. It exposes the whole machine's file names to the caller — hence the per-server policy above. Same Everything search syntax as search_in_folder's pattern ('*.vhd', '\"quarterly report\"', 'ext:pdf;docx dm:thisyear', 'backup !\\\\old\\\\') with two guard rails: 'pattern' must be at least 2 characters, and 'content:' is rejected (content search stays folder-scoped in search_in_folder). Each result carries the full path plus size and modified/created (ISO 8601 UTC); 'max_results' (1..500, default 50) caps the window and 'total' reports all matches — page with 'offset' like search_in_folder.",
            policy
        ),
        "annotations": {
            "title": "Search Everywhere",
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": true,
            "openWorldHint": true,
        },
        "inputSchema": {
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Everything search pattern matched against file/folder names across the whole index, e.g. '*.vhd' (extension), '\"quarterly report\"' (name phrase), 'ext:pdf;docx dm:thisyear' (functions), 'backup !\\\\old\\\\' ('!' excludes). At least 2 characters. 'content:' is rejected — use search_in_folder for content search."
                },
                "exclude": {
                    "type": ["string", "array"],
                    "items": { "type": "string" },
                    "description": "Terms to exclude, appended as Everything NOT operators. Accepts a string or an array of strings. Quoted path fragments match well: ['\\\\old\\\\', '\\\\.git\\\\', '\\\\node_modules\\\\']. Default: none."
                },
                "sort": {
                    "type": "string",
                    "enum": ["name", "path", "size", "modified", "created"],
                    "description": "Sort key for the results. 'modified'/'created' sort by file time, 'size' by byte size. Default: 'name'.",
                    "default": "name"
                },
                "descending": {
                    "type": "boolean",
                    "description": "Sort descending (e.g. 'sort':'modified' + 'descending':true = newest first). Default: false (ascending).",
                    "default": false
                },
                "match_case": {
                    "type": "boolean",
                    "description": "Case-sensitive matching. Default: false.",
                    "default": false
                },
                "match_whole_word": {
                    "type": "boolean",
                    "description": "Match whole words only. Default: false.",
                    "default": false
                },
                "match_regex": {
                    "type": "boolean",
                    "description": "Treat the pattern as a regular expression (Everything's 'regex:' syntax, applied to the pattern term). Default: false.",
                    "default": false
                },
                "offset": {
                    "type": "integer",
                    "description": "Index of the first result to return, for paging: when 'count' < 'total', re-query with 'offset' advanced by 'count'. Default 0.",
                    "default": 0
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (1..500). Default 50.",
                    "default": 50,
                    "minimum": 1,
                    "maximum": 500
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Maximum time in milliseconds to wait for results (default 10000).",
                    "default": 10000
                }
            },
            "required": ["pattern"]
        }
    })
}

/// tools/list 响应：列出我们暴露的全部工具。
///
/// 结果按 era 中性构造（2024-11-05 原形状）；modern 时代的必填字段
/// （resultType、CacheableResult 的 ttlMs / cacheScope）由调用方过
/// [`with_result_type`] 与 [`with_cache_control`] 补齐。
///
/// `mode` 是全局搜索的当前档位 —— 只影响 search_everywhere 的描述前缀与
/// ToolAnnotations（工具清单的形状不变，`listChanged` 仍可宣告 false）。
/// 注意 tools/list 结果带 5 分钟 TTL 缓存：切档后注解最多滞后一个 TTL，
/// 但 Deny 档的硬闸门在调用时检查，立即生效。
pub fn make_tools_list(mode: GlobalSearchMode) -> Value {
    let mut result = serde_json::json!({
        "tools": [
            {
                "name": "search_in_folder",
                "description": "Search files/folders recursively under a specific folder using Everything search syntax. Prefer this over global search when working within a project directory. This is not shell glob: use '*.rs', not '**/*.rs' (a leading '**/' is stripped for you); exclude matches with a '!' prefix (e.g. 'ext:rs !test') or the 'exclude' parameter (e.g. ['\\obj\\', '\\.git\\'] to drop build/VCS noise); 'content:\"fn main\"' searches file contents (it works with no content index on the Everything side, but an unfiltered content search over a large tree is slow enough to time out — narrow it with 'ext:', a subfolder or 'exclude', or raise 'timeout_ms'). Each result carries 'size' plus 'modified'/'created' (ISO 8601 UTC). The response reports both 'count' (entries returned, capped by max_results) and 'total' (all matches found) — when they differ, page through with 'offset'. Results are sorted by 'sort' (default name ascending). Search scope follows Everything's index: local drives are included automatically, but a network share is only searchable after being added in Tools > Options > Indexes > Folders — an un-indexed share yields 0 results, not an error.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": {
                            "type": "string",
                            "description": "Absolute path to the folder to search in, e.g. D:\\\\source\\\\repos\\\\myproject, or an indexed UNC path like \\\\server\\share\\project"
                        },
                        "pattern": {
                            "type": "string",
                            "description": "Everything search pattern. Examples: '*.rs' (extension), 'readme' (substring), 'ext:md;txt' (multiple extensions), '\"exact phrase\"', 'content:\"fn main\"' (content search — slow unless the scope is narrow, see the tool description), 'ext:rs !test' (the '!' prefix excludes matches). Case-sensitive content search is 'case:content:\"...\"' with no space after 'case:'. Empty pattern lists all files. Shell globs like '**/*.rs' are not supported — the folder scope already restricts the tree."
                        },
                        "exclude": {
                            "type": ["string", "array"],
                            "items": { "type": "string" },
                            "description": "Terms to exclude, appended as Everything NOT operators. Accepts a string or an array of strings. Quoted path fragments match well: ['\\obj\\', '\\.git\\', '\\node_modules\\'] drops build output, VCS metadata and dependencies. Default: none."
                        },
                        "sort": {
                            "type": "string",
                            "enum": ["name", "path", "size", "modified", "created"],
                            "description": "Sort key for the results. 'modified'/'created' sort by file time, 'size' by byte size. Default: 'name'.",
                            "default": "name"
                        },
                        "descending": {
                            "type": "boolean",
                            "description": "Sort descending (e.g. 'sort':'modified' + 'descending':true = newest first; 'sort':'size' + 'descending':true = largest first). Default: false (ascending).",
                            "default": false
                        },
                        "match_case": {
                            "type": "boolean",
                            "description": "Case-sensitive matching. Default: false.",
                            "default": false
                        },
                        "match_whole_word": {
                            "type": "boolean",
                            "description": "Match whole words only. Default: false.",
                            "default": false
                        },
                        "match_regex": {
                            "type": "boolean",
                            "description": "Treat the pattern as a regular expression (Everything's 'regex:' syntax, applied to the pattern term). Default: false.",
                            "default": false
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Index of the first result to return, for paging through a large result set: when 'count' < 'total', re-query with 'offset' advanced by 'count'. Default 0.",
                            "default": 0
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
                "description": "List immediate children of a folder (non-recursive). Returns both files and sub-folders; folder entries always report size 0 (Everything does not compute directory sizes). Each entry also carries 'modified'/'created' (ISO 8601 UTC). Use search_in_folder when you need the whole tree. Pass 'exclude' to skip children such as '.git', 'obj', 'node_modules'. Search scope follows Everything's index: local drives are included automatically, but a network share is only searchable after being added in Tools > Options > Indexes > Folders — an un-indexed share yields 0 results, not an error.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string", "description": "Absolute folder path, e.g. D:\\source\\repos\\myproject, or an indexed UNC path like \\\\server\\share\\project" },
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
                        "folder": { "type": "string", "description": "Absolute path to the folder to count in, e.g. D:\\source\\repos\\myproject, or an indexed UNC path like \\\\server\\share\\project" },
                        "pattern": { "type": "string", "default": "" },
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
                "name": "index_changes",
                "description": "Query the Everything index journal: which files/folders were created, modified, deleted, renamed or moved, most recent first. This answers 'what changed recently' from Everything's change history — it does not describe the current index state (use search_in_folder for that). Requires 'journal_log' to be enabled in Everything (Tools > Options > Index > Journal > Log changes); when it is off the tool returns an error that says so. Results are capped by max_results and carry 'truncated' when more matching history exists further back. Each entry's 'action_text' holds Everything's original localized action label, so an unfamiliar locale still reads sensibly. NOTE: Everything appends journal rows to its log file with a delay — measured from about 8 s up to 70 s — so a change made moments ago may not be in the results yet. Re-query after a short wait, and never read an empty result as proof that nothing happened.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["created", "modified", "deleted", "renamed", "moved", "any"],
                            "description": "Filter by change type. 'any' (default) returns everything. Everything distinguishes 'renamed' (same folder) from 'moved' (different folder); 'action_text' in each result carries the original localized label."
                        },
                        "path": {
                            "type": "string",
                            "description": "Only report changes at or under this folder, matched as a case-insensitive path prefix, e.g. D:\\source\\repos\\myproject. Omit to cover the whole index."
                        },
                        "name": {
                            "type": "string",
                            "description": "Case-insensitive substring of the file/folder name; for renames the new name is matched too."
                        },
                        "since": {
                            "type": "string",
                            "description": "Lower bound (inclusive) as '2026-09-23', '2026-09-23 11:18' or '2026-09-23 11:18:31' (space or 'T' between date and time); a bare number is read as unix seconds. Omit for no lower bound."
                        },
                        "until": {
                            "type": "string",
                            "description": "Upper bound (inclusive), same formats as 'since'. Omit for no upper bound."
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum number of changes to return, newest first (default 50).",
                            "default": 50
                        }
                    }
                }
            },
            {
                "name": "read_file",
                "description": "Read the contents of one text file, optionally a window of lines. The companion to search_in_folder: search to locate files, then read_file to read one. 'path' must be an absolute path to a single existing file — no wildcards (use search_in_folder for patterns) and no folders (the error for a folder carries 'suggested_tool'/'suggested_args' pointing at list_folder); files above 8 MiB are refused. Lines are returned verbatim starting at 'start_line' (1-based), at most 'max_lines' of them (default 2000, max 4000) and always within a 128 KiB budget on the returned text — whichever limit bites first, and always on a whole line, never mid-line. The response reports 'total_lines', 'lines_returned' and 'next_start_line': pass 'next_start_line' back as 'start_line' to page, and stop when it is null. Lines longer than 16384 characters are cut (counted in 'clipped_lines'), so a minified single-line file cannot swallow the whole window. 'truncated' is true when content remains beyond the window — its companion is 'next_start_line'. It is not a signal that something was cut mid-line; that is 'clipped_lines'. 'encoding' reports how the bytes were decoded: 'utf-8' / 'utf-16le' / 'utf-16be' are certain (valid UTF-8 or an explicit BOM), while 'ansi' means no valid UTF-8 and no BOM, so the machine's ANSI code page was assumed (correct for GBK text on a Chinese Windows, a guess elsewhere) and 'utf-8-lossy' means undecodable bytes were replaced — treat 'ansi' and 'utf-8-lossy' bodies with suspicion. Private keys and credential bundles are never returned: id_rsa/id_dsa/id_ecdsa/id_ed25519, .env (but not .env.example/.sample/.template/.dist), .netrc, .git-credentials, credentials.json, *.pem/*.key/*.p12/*.pfx/*.jks/*.keystore/*.ppk, and anything under .git/objects — that denylist is deliberately not configurable. Binary files are rejected rather than returned as garbage, by two independent checks: extension (documents, archives, executables, media, fonts and databases — .pdf/.docx/.xlsx/.zip/.exe/.dll/.png/.mp4/.ttf/.sqlite and friends) and content (a NUL byte, or over 30% non-printable bytes in the first 4 KiB). Text formats (.json/.xml/.svg/.log/.csv/.md/.dat) are never blocked by extension, and a UTF-16 file with a BOM is exempt from the content sniff because its raw bytes are full of NULs. Failures come back as JSON carrying 'error' plus a machine-readable 'code' (INVALID_ARGUMENT, NOT_FOUND, PATH_IS_DIRECTORY, PATH_DENIED, BINARY_CONTENT, TOO_LARGE, READ_FAILED) — branch on the code, not the wording. This does not search file contents — use search_in_folder with 'content:\"...\"' for that.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the file to read, e.g. D:\\\\source\\\\repos\\\\myproject\\\\README.md, or a file on an indexed UNC share like \\\\server\\share\\project\\\\README.md. Wildcards are rejected — use search_in_folder to find files by pattern."
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "1-based line number to start from (default 1). A value past the end of the file returns an empty window rather than an error. For the next page, pass back the 'next_start_line' from the previous response.",
                            "default": 1
                        },
                        "max_lines": {
                            "type": "integer",
                            "description": "Maximum number of lines to return (default 2000, max 4000). The real limiter is the 128 KiB response budget: the window stops at whichever comes first, always on a whole line. Individual lines are cut at 16384 characters.",
                            "default": 2000,
                            "minimum": 1,
                            "maximum": 4000
                        }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "grep",
                "description": "Search file CONTENTS by regular expression and return the matching lines with their line numbers. This is the line-level companion to search_in_folder: that one uses Everything's index to answer WHICH files contain something (paths only), while grep reads the candidate files and reports the matching lines themselves. Candidates are still picked by the Everything index, so 'filter' decides how much actually gets read — an unfiltered grep over a large tree reads every file. Three internal caps bound that work: 2000 candidate files, 64 MiB read, and a 128 KiB budget on the matched text that is returned; 'truncated' plus 'candidates' / 'files_scanned' / 'bytes_scanned' tell you which one bit. 'pattern' is a Rust regex matched against each line individually, so '^' and '$' anchor to line boundaries and a pattern containing a literal newline can never match. 'filter' takes the same Everything syntax as search_in_folder's pattern ('ext:rs;toml', 'dm:lastweek', '!\\\\target\\\\') and is applied before any file is read — use it. 'output_mode' picks the shape: 'content' (default) returns {path, line, text} hits, 'filesWithMatches' returns just the paths, 'count' returns per-file hit counts. 'head_limit' caps matches (content) or files (other modes); default 200, max 2000 — the 128 KiB budget is the real limiter. Matched lines longer than 16384 characters are cut and counted in 'clipped_lines'. Binary files (by extension or by content sniff), files above 8 MiB and denylisted paths (private keys, .env, credentials, .git/objects) are skipped silently — grep never returns content from them. Results are ordered by file modification time, newest first. Prefer this over search_in_folder + read_file when you need to know WHERE inside the files something is.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Rust regular expression matched against each line separately, e.g. 'fn\\\\s+main' or 'TODO|FIXME'. Case-sensitive unless 'case_insensitive' is set."
                        },
                        "folder": {
                            "type": "string",
                            "description": "Absolute path to the folder to search in, e.g. D:\\\\source\\\\repos\\\\myproject, or an indexed UNC path like \\\\server\\share\\project. Always recurse into subfolders."
                        },
                        "filter": {
                            "type": "string",
                            "description": "Everything-syntax filter on file names/paths, applied BEFORE any file is read: 'ext:rs;toml', 'dm:lastweek', '!\\\\target\\\\', '\"src\"'. Strongly recommended — without it every file under 'folder' is read. This is not the regex; 'pattern' is."
                        },
                        "output_mode": {
                            "type": "string",
                            "enum": ["content", "filesWithMatches", "count"],
                            "description": "content (default): matching lines with line numbers; filesWithMatches: only the paths of files containing a match; count: per-file match counts.",
                            "default": "content"
                        },
                        "head_limit": {
                            "type": "integer",
                            "description": "Maximum matches (content) or files (other modes); default 200, max 2000.",
                            "default": 200
                        },
                        "case_insensitive": {
                            "type": "boolean",
                            "description": "Match case-insensitively (default false).",
                            "default": false
                        },
                        "exclude": {
                            "type": ["string", "array"],
                            "items": { "type": "string" },
                            "description": "Terms to exclude from the candidate files, appended as Everything NOT operators (same as search_in_folder's 'exclude'). Quoted path fragments match well: ['\\\\target\\\\', '\\\\.git\\\\']. Default: none."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "Maximum time in milliseconds to wait for the Everything candidate query (default 10000). Reading the candidates afterwards is not covered by it.",
                            "default": 10000
                        }
                    },
                    "required": ["pattern", "folder"]
                }
            }
        ]
    });
    // search_everywhere 的描述前缀与注解随档位变，单独构造后追加 ——
    // 上面六个条目保持纯静态形状，方便逐字段断言。
    if let Some(arr) = result["tools"].as_array_mut() {
        arr.push(search_everywhere_entry(mode));
    }
    result
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
