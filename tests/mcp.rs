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
    let (method, path, len) =
        server::parse_header("POST /mcp HTTP/1.1\r\nHost: x\r\ncontent-length: 42\r\n\r\n").unwrap();
    assert_eq!(method, "POST");
    assert_eq!(path, "/mcp");
    assert_eq!(len, 42);
}

#[test]
fn parse_header_without_content_length_is_zero() {
    let (method, path, len) = server::parse_header("GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    assert_eq!(method, "GET");
    assert_eq!(path, "/");
    assert_eq!(len, 0);
}

#[test]
fn parse_header_rejects_garbage_first_line() {
    // 只有方法没有路径 —— 不是合法请求行。
    assert!(server::parse_header("POST\r\n\r\n").is_none());
    assert!(server::parse_header("\r\n\r\n").is_none());
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
