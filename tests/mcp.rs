//! MCP 协议层集成测试。
//!
//! 覆盖不依赖 Everything 主程序环境的纯逻辑：JSON-RPC 分发、HTTP 头部解析与
//! 响应构造、协议类型、工具参数校验。凡需要 db_query_search2 的路径（真正
//! 执行搜索）都必须在 Everything 进程内手动验证，不在自动化范围内。

use everything_mcp::mcp::protocol;
use everything_mcp::mcp::server;
use everything_mcp::mcp::tools;
use serde_json::{json, Value};

// ====================================================================
// 辅助
// ====================================================================

/// 调一次 dispatch_rpc 并把响应解析成 JSON。
fn dispatch(body: &str) -> Value {
    serde_json::from_str(&server::dispatch_rpc(body)).expect("response must be valid JSON")
}

/// 构造一个 modern 时代的请求头（声明 2026-07-28）。
fn modern_head() -> server::RequestHead {
    server::RequestHead {
        protocol_version: Some(protocol::PROTOCOL_VERSION_MODERN.into()),
        ..Default::default()
    }
}

/// 调一次 dispatch_rpc_http：空体（202 通知）解析成 Value::Null。
fn dispatch_http(body: &str, head: &server::RequestHead) -> (u16, Value) {
    let (status, out) = server::dispatch_rpc_http(body, head);
    let json = if out.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&out).expect("response must be valid JSON")
    };
    (status, json)
}

// ====================================================================
// protocol：initialize / tools/list / 响应构造
// ====================================================================

#[test]
fn initialize_result_announces_protocol_version_and_tools_capability() {
    let r = protocol::make_initialize_result(&json!({}));
    assert_eq!(r["protocolVersion"], "2024-11-05");
    assert_eq!(r["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(r["serverInfo"]["name"], "everything-mcp");
    assert_eq!(r["serverInfo"]["version"], "1.0.0");
}

#[test]
fn tools_list_contains_three_tools_with_required_params() {
    let list = protocol::make_tools_list();
    let tools = list["tools"].as_array().expect("tools must be an array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["search_in_folder", "list_folder", "count"]);

    // search_in_folder 的 folder / pattern 必填，其余可带默认值。
    let search = &tools[0];
    let required = search["inputSchema"]["required"].as_array().unwrap();
    assert_eq!(required.len(), 2);
    assert_eq!(search["inputSchema"]["properties"]["max_results"]["default"], 50);
    assert_eq!(
        search["inputSchema"]["properties"]["timeout_ms"]["default"],
        10_000
    );

    // list_folder / count 只要求 folder。
    assert_eq!(tools[1]["inputSchema"]["required"], json!(["folder"]));
    assert_eq!(tools[2]["inputSchema"]["required"], json!(["folder"]));
}

#[test]
fn ok_response_carries_id_and_result_only() {
    let resp = protocol::ok_response(&Some(json!(7)), json!({"ok": true}));
    assert_eq!(resp.id, Some(json!(7)));
    assert_eq!(resp.result, Some(json!({"ok": true})));
    assert!(resp.error.is_none());
    assert!(resp.method.is_none());
    assert!(resp.params.is_none());
}

#[test]
fn error_response_carries_code_and_message() {
    let resp = protocol::error_response(&Some(json!("abc")), protocol::METHOD_NOT_FOUND, "nope");
    assert_eq!(resp.id, Some(json!("abc")));
    let err = resp.error.expect("error must be present");
    assert_eq!(err.code, protocol::METHOD_NOT_FOUND);
    assert_eq!(err.message, "nope");
    assert!(resp.result.is_none());
}

#[test]
fn message_round_trips_through_json() {
    // 通知（无 id）序列化后不应出现 id 字段。
    let msg = protocol::JsonRpcMessage {
        jsonrpc: "2.0".into(),
        id: None,
        method: Some("notifications/initialized".into()),
        params: None,
        result: None,
        error: None,
    };
    let s = serde_json::to_string(&msg).unwrap();
    assert!(!s.contains("\"id\""));
    let back: protocol::JsonRpcMessage = serde_json::from_str(&s).unwrap();
    assert!(back.id.is_none());
    assert_eq!(back.method.as_deref(), Some("notifications/initialized"));
}

// ====================================================================
// server：HTTP 头部解析
// ====================================================================

#[test]
fn parse_header_extracts_method_path_and_length() {
    let head =
        server::parse_header("POST /mcp HTTP/1.1\r\nHost: x\r\ncontent-length: 42\r\n\r\n").unwrap();
    assert_eq!(head.method, "POST");
    assert_eq!(head.path, "/mcp");
    assert_eq!(head.content_length, 42);
    // legacy 请求不带 MCP 镜像头与 Origin。
    assert!(head.protocol_version.is_none());
    assert!(head.origin.is_none());
}

#[test]
fn parse_header_reads_mcp_and_origin_headers() {
    // modern 客户端的镜像头与安全头都要能解析出来（字段名不区分大小写）。
    let head = server::parse_header(
        "POST / HTTP/1.1\r\nMCP-Protocol-Version: 2026-07-28\r\nOrigin: http://localhost:3000\r\nmcp-method: ping\r\nMcp-Name: count\r\n\r\n",
    )
    .unwrap();
    assert_eq!(head.protocol_version.as_deref(), Some("2026-07-28"));
    assert_eq!(head.origin.as_deref(), Some("http://localhost:3000"));
    assert_eq!(head.mcp_method.as_deref(), Some("ping"));
    assert_eq!(head.mcp_name.as_deref(), Some("count"));
}

#[test]
fn parse_header_rejects_garbage_first_line() {
    // 只有方法没有路径 —— 不是合法请求行。
    assert!(server::parse_header("POST\r\n\r\n").is_none());
    assert!(server::parse_header("\r\n\r\n").is_none());
}

#[test]
fn parse_header_without_content_length_is_zero() {
    let head = server::parse_header("GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    assert_eq!(head.method, "GET");
    assert_eq!(head.path, "/");
    assert_eq!(head.content_length, 0);
}

// ====================================================================
// server：JSON-RPC 分发
// ====================================================================

#[test]
fn dispatch_initialize_returns_server_info() {
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(resp["result"]["serverInfo"]["name"], "everything-mcp");
    assert!(resp.get("error").is_none());
}

#[test]
fn dispatch_ping_returns_empty_result() {
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#);
    assert_eq!(resp["id"], "p");
    assert_eq!(resp["result"], json!({}));
}

#[test]
fn dispatch_tools_list_returns_three_tools() {
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(list.len(), 3);
}

#[test]
fn dispatch_notification_gets_null_id_ack() {
    // 通知没有 id —— 按约定返回 id 为 null 的空响应。
    let out = server::dispatch_rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!(out, r#"{"jsonrpc":"2.0","id":null}"#);
}

#[test]
fn dispatch_unknown_method_is_method_not_found() {
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#);
    assert_eq!(resp["error"]["code"], protocol::METHOD_NOT_FOUND);
    assert!(resp["error"]["message"].as_str().unwrap().contains("resources/list"));
}

#[test]
fn dispatch_malformed_json_is_parse_error_with_null_id() {
    let resp = dispatch("{not json");
    assert_eq!(resp["error"]["code"], protocol::PARSE_ERROR);
    assert!(resp["id"].is_null());
}

#[test]
fn dispatch_tools_call_unknown_tool_is_method_not_found() {
    let resp = dispatch(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
    );
    assert_eq!(resp["error"]["code"], protocol::METHOD_NOT_FOUND);
    assert!(resp["error"]["message"].as_str().unwrap().contains("nope"));
}

#[test]
fn dispatch_tools_call_missing_folder_is_invalid_params() {
    // 缺 folder 的参数校验在触达 Everything host 之前就返回，
    // 因此不依赖主程序环境（未安装 Everything 的机器上也能跑这个用例）。
    let resp = dispatch(
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_in_folder","arguments":{}}}"#,
    );
    assert_eq!(resp["error"]["code"], protocol::INVALID_PARAMS);
}

// ====================================================================
// server：HTTP 响应构造
// ====================================================================

#[test]
fn ok_http_response_content_length_matches_body_bytes() {
    // 多字节 UTF-8：Content-Length 必须是字节数而不是字符数。
    let body = r#"{"text":"中文"}"#;
    let resp = server::ok_http_response(body);
    // split 会吃掉空行前的 \r\n，所以 head 以 "Connection: close" 结尾。
    let (head, payload) = resp.split_once("\r\n\r\n").unwrap();
    assert_eq!(payload, body);
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(head.contains("Content-Type: application/json\r\n"));
    assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
    assert!(head.ends_with("Connection: close"));
}

#[test]
fn not_allowed_and_bad_request_shapes() {
    assert!(server::not_allowed().starts_with("HTTP/1.1 405"));
    let bad = server::bad_request("boom");
    assert!(bad.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(bad.ends_with("boom\"}"));
}

// ====================================================================
// tools：参数校验（不触达 host 的路径）
// ====================================================================

#[test]
fn missing_folder_is_invalid_params_for_every_tool() {
    // 这三个用例都必须在触达 Everything host 之前就报错返回 ——
    // 参数校验先行，没有主程序环境的 CI 上也能跑。
    for name in ["search_in_folder", "list_folder", "count"] {
        let err = tools::dispatch(name, &json!({})).unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "{}", name);
        assert!(err.1.contains("folder"));
    }
}

#[test]
fn folder_of_wrong_type_is_invalid_params() {
    let err = tools::dispatch("search_in_folder", &json!({"folder": 123})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
}

#[test]
fn unknown_tool_is_method_not_found() {
    let err = tools::dispatch("delete_everything", &json!({"folder": "C:\\"})).unwrap_err();
    assert_eq!(err.0, protocol::METHOD_NOT_FOUND);
    assert!(err.1.contains("delete_everything"));
}

// ====================================================================
// 双时代协议：版本协商（2024-11-05 legacy / 2026-07-28 modern）
// ====================================================================

#[test]
fn initialize_echoes_supported_client_version() {
    // legacy 握手：客户端请求我们支持的版本 → 原样回显。
    let r = protocol::make_initialize_result(&json!({"protocolVersion": "2026-07-28"}));
    assert_eq!(r["protocolVersion"], "2026-07-28");
    // 请求我们不认识的版本 → 退回 legacy，由客户端决定是否断开。
    let r = protocol::make_initialize_result(&json!({"protocolVersion": "2030-01-01"}));
    assert_eq!(r["protocolVersion"], "2024-11-05");
}

#[test]
fn discover_result_lists_both_supported_versions() {
    let r = protocol::make_discover_result();
    assert_eq!(r["resultType"], "complete");
    let versions: Vec<&str> = r["supportedVersions"]
        .as_array()
        .expect("supportedVersions must be an array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(versions.contains(&"2024-11-05"), "{:?}", versions);
    assert!(versions.contains(&"2026-07-28"), "{:?}", versions);
    // serverInfo 按规范放在 _meta 里。
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "everything-mcp"
    );
    assert_eq!(r["capabilities"]["tools"]["listChanged"], false);
}

#[test]
fn modern_discover_works_without_handshake() {
    // modern 客户端不握手，直接带 _meta 版本发 discover。
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &modern_head(),
    );
    assert_eq!(status, 200);
    assert_eq!(resp["result"]["supportedVersions"][0], "2024-11-05");
    assert_eq!(resp["result"]["supportedVersions"][1], "2026-07-28");
}

#[test]
fn modern_unsupported_version_is_400_with_supported_list() {
    let head = server::RequestHead {
        protocol_version: Some("2030-01-01".into()),
        ..Default::default()
    };
    let (status, resp) = dispatch_http(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, &head);
    assert_eq!(status, 400);
    assert_eq!(
        resp["error"]["code"],
        protocol::UNSUPPORTED_PROTOCOL_VERSION
    );
    assert_eq!(resp["error"]["data"]["requested"], "2030-01-01");
    // data.supported 让客户端知道该退到哪个版本重试。
    assert_eq!(
        resp["error"]["data"]["supported"],
        json!(["2024-11-05", "2026-07-28"])
    );
}

#[test]
fn modern_header_meta_mismatch_is_32020() {
    // 头说 modern、_meta 说 legacy —— 两者矛盾，拒绝。
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2024-11-05"}}}"#,
        &modern_head(),
    );
    assert_eq!(status, 400);
    assert_eq!(resp["error"]["code"], protocol::HEADER_MISMATCH);
}

#[test]
fn modern_unknown_method_is_404() {
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"resources/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &modern_head(),
    );
    assert_eq!(status, 404);
    assert_eq!(resp["error"]["code"], protocol::METHOD_NOT_FOUND);
    // legacy 客户端同一个请求仍走 200 + JSON-RPC 错误体的老路。
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#);
    assert_eq!(resp["error"]["code"], protocol::METHOD_NOT_FOUND);
}

#[test]
fn modern_notification_is_202_with_empty_body() {
    let out = server::dispatch_rpc_http(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &modern_head(),
    );
    // modern：接受通知不得返回响应体。
    assert_eq!(out, (202, String::new()));
}

#[test]
fn modern_mcp_method_header_mismatch_is_32020() {
    let head = server::RequestHead {
        protocol_version: Some("2026-07-28".into()),
        mcp_method: Some("tools/list".into()),
        ..Default::default()
    };
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &head,
    );
    assert_eq!(status, 400);
    assert_eq!(resp["error"]["code"], protocol::HEADER_MISMATCH);
}

#[test]
fn modern_mcp_name_header_accepts_base64_sentinel() {
    // "search_in_folder" 的 base64 形式（非 ASCII 头部值的 sentinel 编码）。
    let head = server::RequestHead {
        protocol_version: Some("2026-07-28".into()),
        mcp_name: Some("=?base64?c2VhcmNoX2luX2ZvbGRlcg==?=".into()),
        ..Default::default()
    };
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_in_folder","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &head,
    );
    // 头/体一致 → 放行进入工具分发；缺 folder 的 INVALID_PARAMS 证明走到了 tools 层。
    assert_eq!(status, 200);
    assert_eq!(resp["error"]["code"], protocol::INVALID_PARAMS);
}

#[test]
fn modern_mcp_name_header_mismatch_is_32020() {
    let head = server::RequestHead {
        protocol_version: Some("2026-07-28".into()),
        mcp_name: Some("count".into()),
        ..Default::default()
    };
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_in_folder","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &head,
    );
    assert_eq!(status, 400);
    assert_eq!(resp["error"]["code"], protocol::HEADER_MISMATCH);
}

// ====================================================================
// 双时代协议：resultType（2026-07-28 起 result 必须带，缺失即无效结果）
// ====================================================================

#[test]
fn modern_tools_list_result_carries_result_type() {
    // modern 客户端对 tools/list 的 result 强制要求 resultType —— 缺失会被
    // 判为无效结果并告警（absent-means-complete 桥接只适用于早期修订版）。
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &modern_head(),
    );
    assert_eq!(status, 200);
    assert_eq!(resp["result"]["resultType"], "complete");
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 3);
}

#[test]
fn legacy_tools_list_has_no_result_type() {
    // legacy 时代的规范没有 resultType 字段 —— 响应保持 2024-11-05 原形状，
    // 客户端按桥接规则把缺失当作 complete。
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    assert!(resp["result"].get("resultType").is_none());
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 3);
}

#[test]
fn modern_ping_result_carries_result_type() {
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":3,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &modern_head(),
    );
    assert_eq!(status, 200);
    assert_eq!(resp["result"]["resultType"], "complete");
}

#[test]
fn modern_initialize_result_carries_result_type() {
    // 客户端用 modern 版本走 initialize 握手时，回显 modern 版本 + resultType。
    let r = protocol::make_initialize_result(&json!({"protocolVersion": "2026-07-28"}));
    assert_eq!(r["protocolVersion"], "2026-07-28");
    assert_eq!(r["resultType"], "complete");
    // legacy 握手（未声明 / 老版本）保持老形状，不带 resultType。
    let r = protocol::make_initialize_result(&json!({}));
    assert_eq!(r["protocolVersion"], "2024-11-05");
    assert!(r.get("resultType").is_none());
}

#[test]
fn with_result_type_only_decorates_modern_era() {
    // modern：补 "complete"；legacy：原样返回；非对象结果原样透传。
    let v = protocol::with_result_type(json!({"tools": []}), true);
    assert_eq!(v["resultType"], "complete");
    assert_eq!(v["tools"], json!([]));
    let v = protocol::with_result_type(json!({"tools": []}), false);
    assert!(v.get("resultType").is_none());
    let v = protocol::with_result_type(json!("not-an-object"), true);
    assert_eq!(v, json!("not-an-object"));
}

// ====================================================================
// 双时代协议：CacheableResult 缓存提示（ttlMs / cacheScope）
// ====================================================================

#[test]
fn modern_tools_list_result_carries_cache_control() {
    // 2026-07-28 起 ListToolsResult 继承 CacheableResult：ttlMs（number）与
    // cacheScope（"public" | "private"）是必填字段，缺失即无效结果。
    let (status, resp) = dispatch_http(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &modern_head(),
    );
    assert_eq!(status, 200);
    assert_eq!(resp["result"]["ttlMs"], json!(protocol::TOOLS_LIST_TTL_MS));
    assert_eq!(resp["result"]["cacheScope"], "public");
    // resultType 照旧带上，两个字段互不替代。
    assert_eq!(resp["result"]["resultType"], "complete");
}

#[test]
fn legacy_tools_list_has_no_cache_control() {
    // legacy 规范没有 ttlMs / cacheScope —— 响应保持 2024-11-05 原形状。
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    assert!(resp["result"].get("ttlMs").is_none());
    assert!(resp["result"].get("cacheScope").is_none());
}

#[test]
fn discover_result_carries_cache_control() {
    // DiscoverResult 同样继承 CacheableResult —— discover 是 modern 独有方法，
    // 结果整体按 modern 形状构造，缓存字段必填。
    let r = protocol::make_discover_result();
    assert_eq!(r["ttlMs"], json!(protocol::DISCOVER_TTL_MS));
    assert_eq!(r["cacheScope"], "public");
}

#[test]
fn with_cache_control_only_decorates_modern_era() {
    // modern：补 ttlMs + cacheScope；legacy：原样返回；非对象结果原样透传。
    let v = protocol::with_cache_control(json!({"tools": []}), true, 1000);
    assert_eq!(v["ttlMs"], json!(1000));
    assert_eq!(v["cacheScope"], "public");
    let v = protocol::with_cache_control(json!({"tools": []}), false, 1000);
    assert!(v.get("ttlMs").is_none());
    assert!(v.get("cacheScope").is_none());
    let v = protocol::with_cache_control(json!("not-an-object"), true, 1000);
    assert_eq!(v, json!("not-an-object"));
}

// ====================================================================
// server：Origin 校验（防 DNS rebinding）
// ====================================================================

#[test]
fn origin_error_allows_localhost_and_rejects_remote() {
    // 放行：缺失（非浏览器客户端）、空、null（不透明来源）、本机来源。
    assert!(server::origin_error(None).is_none());
    assert!(server::origin_error(Some("")).is_none());
    assert!(server::origin_error(Some("null")).is_none());
    assert!(server::origin_error(Some("http://localhost:8285")).is_none());
    assert!(server::origin_error(Some("http://127.0.0.1:8285")).is_none());
    assert!(server::origin_error(Some("http://[::1]:8285")).is_none());
    assert!(server::origin_error(Some("https://localhost")).is_none());
    // 拒绝：远程网站经 DNS rebinding 打到本机端点的场景。
    let err = server::origin_error(Some("https://evil.example.com")).expect("must reject");
    let v: Value = serde_json::from_str(&err).unwrap();
    assert_eq!(v["error"]["code"], protocol::INVALID_REQUEST);
}

// ====================================================================
// validate：folder 路径规范化
// ====================================================================

#[test]
fn normalize_folder_fixes_llm_path_quirks() {
    use everything_mcp::mcp::validate;
    // 包裹引号、正斜杠、双反斜杠、尾斜杠一次收拾干净 —— parent:"…" 按字面匹配，
    // 这些「人类书写」痕迹不规范化就会静默返回 0 结果。
    assert_eq!(
        validate::normalize_folder(r#""D:\source\repos\everything-mcp""#).unwrap(),
        r"D:\source\repos\everything-mcp"
    );
    assert_eq!(
        validate::normalize_folder("D:/source/repos").unwrap(),
        r"D:\source\repos"
    );
    assert_eq!(
        validate::normalize_folder(r"D:\\source\\repos\\").unwrap(),
        r"D:\source\repos"
    );
    assert_eq!(
        validate::normalize_folder("  C:\\Users\\me\\project  ").unwrap(),
        r"C:\Users\me\project"
    );
    // 盘符根保留尾斜杠；裸盘号补全。
    assert_eq!(validate::normalize_folder(r"C:\").unwrap(), r"C:\");
    assert_eq!(validate::normalize_folder("C:").unwrap(), r"C:\");
    // UNC：折叠多余反斜杠、去尾斜杠但保留两段。
    assert_eq!(
        validate::normalize_folder(r"\\server\share\docs\").unwrap(),
        r"\\server\share\docs"
    );
    assert_eq!(
        validate::normalize_folder("//server/share").unwrap(),
        r"\\server\share"
    );
}

#[test]
fn normalize_folder_rejects_bad_paths_with_examples() {
    use everything_mcp::mcp::validate;
    for bad in [
        "",           // 空
        "   ",        // 只有空白
        "src\\mcp",   // 相对路径
        "C:src",      // 盘符后无反斜杠
        "C:src\\mcp", // 相对盘符路径
        r"C:\Users\*", // 通配符不属于路径
        r"\\server",  // UNC 只有一段
        r#"D:\a"b"#,  // 内部引号会破坏搜索语法
    ] {
        let err = validate::normalize_folder(bad)
            .unwrap_err();
        assert!(err.contains("folder"), "错误消息要点名参数: {}", err);
    }
    // 绝对路径类错误的消息里带范例，LLM 拿到就能一次改对。
    let err = validate::normalize_folder("src\\mcp").unwrap_err();
    assert!(err.contains(r"C:\Users\me\project"), "{}", err);
}

#[test]
fn validate_pattern_trims_and_rejects_control_chars() {
    use everything_mcp::mcp::validate;
    // 空 pattern 合法 —— 表示列出全部条目。
    assert_eq!(validate::validate_pattern("").unwrap(), "");
    assert_eq!(validate::validate_pattern("  *.rs  ").unwrap(), "*.rs");
    assert!(validate::validate_pattern("a\nb").is_err());
}

// ====================================================================
// tools：入参校验（不触达 host 的路径）
// ====================================================================

#[test]
fn non_numeric_max_results_is_invalid_params() {
    // 负数/小数/字符串都会在 as_u64 上败下阵来 —— 明确报错而不是静默取默认值。
    let err = tools::dispatch(
        "search_in_folder",
        &json!({"folder": "C:\\", "max_results": "many"}),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("max_results"));
}

#[test]
fn wildcard_in_folder_is_rejected_with_guidance() {
    let err =
        tools::dispatch("search_in_folder", &json!({"folder": "C:\\Users\\*"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("wildcards"), "{}", err.1);
}

// ====================================================================
// 端到端：真实 TCP 连接（只发不触达 Everything host 的请求，
// 因此没有主程序环境的 CI 上也能跑）
// ====================================================================

/// 向本地服务发一个 POST，返回 (状态行, 响应体)。
fn http_post(port: u16, body: &str, extra_headers: &[(&str, &str)]) -> (String, String) {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str(&format!("\r\n{}", body));

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to test server");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, payload) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    (head.to_string(), payload.to_string())
}

#[test]
fn end_to_end_over_real_tcp() {
    let port = 18285;
    server::start("127.0.0.1", port).expect("test server must start");

    // legacy initialize 握手照旧。
    let (head, body) = http_post(
        port,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        &[],
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{}", head);
    let resp: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");

    // modern discover：不握手，_meta 带版本。
    let (head, body) = http_post(
        port,
        r#"{"jsonrpc":"2.0","id":2,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &[],
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{}", head);
    let resp: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resp["result"]["supportedVersions"][1], "2026-07-28");
    assert_eq!(resp["result"]["cacheScope"], "public");

    // modern tools/list：镜像头 + _meta 同版本，result 必须带 resultType 与
    // CacheableResult 的 ttlMs / cacheScope。
    let (head, body) = http_post(
        port,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &[("MCP-Protocol-Version", "2026-07-28")],
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{}", head);
    let resp: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resp["result"]["resultType"], "complete");
    assert_eq!(resp["result"]["ttlMs"], json!(protocol::TOOLS_LIST_TTL_MS));
    assert_eq!(resp["result"]["cacheScope"], "public");
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 3);

    // modern 未知方法 → 404。
    let (head, _) = http_post(
        port,
        r#"{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        &[],
    );
    assert!(head.starts_with("HTTP/1.1 404"), "{}", head);

    // 非 localhost Origin → 403（DNS rebinding 防护在真实连接上生效）。
    let (head, _) = http_post(
        port,
        r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#,
        &[("Origin", "https://evil.example.com")],
    );
    assert!(head.starts_with("HTTP/1.1 403"), "{}", head);

    // 参数校验在触达 host 之前返回带范例的 INVALID_PARAMS。
    let (head, body) = http_post(
        port,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"count","arguments":{"folder":"D:src"}}}"#,
        &[],
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{}", head);
    let resp: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(resp["error"]["code"], protocol::INVALID_PARAMS);
    assert!(resp["error"]["message"].as_str().unwrap().contains("folder"));

    server::stop();
}
