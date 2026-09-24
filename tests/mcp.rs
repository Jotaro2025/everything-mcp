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
    // 版本号唯一来源是 Cargo.toml —— 这里跟着它走，bump 时不必改测试。
    // （src/lib.rs 的 PLUGIN_VERSION 与 setup\version.h 都是同一来源。）
    assert_eq!(r["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn tools_list_contains_seven_tools_with_required_params() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let tools = list["tools"].as_array().expect("tools must be an array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "search_in_folder",
            "list_folder",
            "count",
            "index_changes",
            "read_file",
            "grep",
            "search_everywhere"
        ]
    );

    // search_in_folder 的 folder / pattern 必填，其余可带默认值。
    let search = &tools[0];
    let required = search["inputSchema"]["required"].as_array().unwrap();
    assert_eq!(required.len(), 2);
    assert_eq!(
        search["inputSchema"]["properties"]["max_results"]["default"],
        50
    );
    assert_eq!(
        search["inputSchema"]["properties"]["timeout_ms"]["default"],
        10_000
    );

    // 排序 / 分页 / 匹配开关：全部可选，且带默认值。
    let props = &search["inputSchema"]["properties"];
    assert_eq!(props["offset"]["default"], 0);
    assert_eq!(props["match_case"]["default"], false);
    assert_eq!(props["match_whole_word"]["default"], false);
    assert_eq!(props["match_regex"]["default"], false);
    assert_eq!(props["descending"]["default"], false);
    assert_eq!(props["sort"]["default"], "name");
    assert_eq!(
        props["sort"]["enum"],
        json!(["name", "path", "size", "modified", "created"])
    );
    // 这些新参数都不进 required —— 缺省即可用。
    assert!(!required
        .iter()
        .any(|v| v.as_str() == Some("sort") || v.as_str() == Some("offset")));

    // list_folder / count 只要求 folder；index_changes 一个参数都不必带。
    assert_eq!(tools[1]["inputSchema"]["required"], json!(["folder"]));
    assert_eq!(tools[2]["inputSchema"]["required"], json!(["folder"]));
    // index_changes 的入参全都不必填：不带参数就是「列出最近的变更」。
    assert!(
        tools[3]["inputSchema"].get("required").is_none(),
        "index_changes 应当全可选（列出最近的变更即可）"
    );
    assert_eq!(
        tools[3]["inputSchema"]["properties"]["max_results"]["default"],
        50
    );

    // read_file：只要求 path；start_line / max_lines 带默认值（数值与 PI-Desktop 对齐）。
    assert_eq!(tools[4]["inputSchema"]["required"], json!(["path"]));
    let read_props = &tools[4]["inputSchema"]["properties"];
    assert_eq!(read_props["start_line"]["default"], 1);
    assert_eq!(read_props["max_lines"]["default"], 2000);
    assert_eq!(read_props["max_lines"]["minimum"], 1);
    assert_eq!(read_props["max_lines"]["maximum"], 4000);

    // grep：pattern + folder 必填，其余带默认值。
    assert_eq!(tools[5]["inputSchema"]["required"], json!(["pattern", "folder"]));
    let grep_props = &tools[5]["inputSchema"]["properties"];
    assert_eq!(grep_props["output_mode"]["default"], "content");
    assert_eq!(
        grep_props["output_mode"]["enum"],
        json!(["content", "filesWithMatches", "count"])
    );
    assert_eq!(grep_props["head_limit"]["default"], 200);
    assert_eq!(grep_props["case_insensitive"]["default"], false);
    assert_eq!(grep_props["timeout_ms"]["default"], 10_000);

    // search_everywhere：只要求 pattern；max_results 是硬窗口 1..500。
    assert_eq!(tools[6]["inputSchema"]["required"], json!(["pattern"]));
    let global_props = &tools[6]["inputSchema"]["properties"];
    assert_eq!(global_props["max_results"]["default"], 50);
    assert_eq!(global_props["max_results"]["minimum"], 1);
    assert_eq!(global_props["max_results"]["maximum"], 500);
    assert_eq!(global_props["offset"]["default"], 0);
    assert_eq!(global_props["timeout_ms"]["default"], 10_000);
    assert_eq!(global_props["sort"]["default"], "name");
    assert_eq!(
        global_props["sort"]["enum"],
        json!(["name", "path", "size", "modified", "created"])
    );
}

#[test]
fn search_everywhere_annotations_and_policy_follow_the_global_mode() {
    // 方案 3：三档模式改变的是 ToolAnnotations + 描述里的 POLICY 段。
    // Review 档用 destructiveHint 驱动客户端的确认弹窗（并不真的改磁盘），
    // Allow 档标只读以便客户端自动放行，Deny 档沿用 Review 的注解（不诱导
    // 自动放行）并在描述里写明服务端会拒绝。
    let entry = |m: protocol::GlobalSearchMode| protocol::make_tools_list(m)["tools"][6].clone();
    let deny = entry(protocol::GlobalSearchMode::Deny);
    let review = entry(protocol::GlobalSearchMode::Review);
    let allow = entry(protocol::GlobalSearchMode::Allow);

    // 形状不随模式变：名字与 inputSchema 恒定，切档不必宣告 listChanged。
    for t in [&deny, &review, &allow] {
        assert_eq!(t["name"], "search_everywhere");
        assert_eq!(t["inputSchema"]["required"], json!(["pattern"]));
    }

    // Allow：只读注解 + 直接放行的说明。
    assert_eq!(allow["annotations"]["readOnlyHint"], true);
    assert_eq!(allow["annotations"]["destructiveHint"], false);
    assert!(
        allow["description"]
            .as_str()
            .unwrap()
            .contains("global search' = allow")
    );

    // Review：非只读 + 破坏性（驱动确认弹窗），描述点名 USER 必须确认、磁盘不改。
    assert_eq!(review["annotations"]["readOnlyHint"], false);
    assert_eq!(review["annotations"]["destructiveHint"], true);
    let desc = review["description"].as_str().unwrap();
    assert!(desc.contains("the USER must confirm"), "{desc}");
    assert!(desc.contains("Nothing on disk is ever modified"), "{desc}");

    // Deny：同 Review 注解 + 说明服务端会硬拒。
    assert_eq!(deny["annotations"]["readOnlyHint"], false);
    assert_eq!(deny["annotations"]["destructiveHint"], true);
    assert!(deny["description"]
        .as_str()
        .unwrap()
        .contains("GLOBAL_SEARCH_DISABLED"));

    // 幂等 / 开放世界提示三档一致。
    for t in [&deny, &review, &allow] {
        assert_eq!(t["annotations"]["idempotentHint"], true);
        assert_eq!(t["annotations"]["openWorldHint"], true);
    }
}

#[test]
fn index_changes_description_and_schema_document_the_gotchas() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let tool = &list["tools"][3];
    let desc = tool["description"].as_str().unwrap();

    // 四个必须说清的点：它答的是「什么变了」而非「现在有什么」；要开 journal_log；
    // 结果可能被 max_results 截断；写完到落盘有数十秒延迟。
    assert!(desc.contains("journal_log"), "desc should name the INI key");
    assert!(desc.contains("truncated"), "desc should expose truncation");
    assert!(
        desc.contains("search_in_folder"),
        "desc should point at the state-query tool"
    );
    assert!(
        desc.contains("Log changes"),
        "desc should name the Options menu path"
    );
    // 落盘延迟不说清，LLM 会把空结果读成「什么都没发生」。延迟是实测区间
    // （约 8～70 秒），不是固定值，描述里必须写成区间。
    assert!(
        desc.contains("8 s up to 70 s") && desc.contains("never read an empty result as proof"),
        "desc should document the measured log-write latency range"
    );

    // action 用封闭枚举，避免 LLM 乱自创动作名。
    let action = &tool["inputSchema"]["properties"]["action"];
    assert_eq!(
        action["enum"],
        json!(["created", "modified", "deleted", "renamed", "moved", "any"])
    );
    // max_results 的边界要声明出来（不能只写在描述文字里）：客户端据此提前
    // 拦下越界值，否则坏值要打到服务端才变成 -32602。上限与
    // tools.rs 的 INDEX_CHANGES_LIMIT 一致。
    let max_results = &tool["inputSchema"]["properties"]["max_results"];
    assert_eq!(max_results["minimum"], 1);
    assert_eq!(max_results["maximum"], 2000);
    // 描述里必须给出时间参数的形状范例，否则 LLM 会乱写。
    let since = tool["inputSchema"]["properties"]["since"]["description"]
        .as_str()
        .unwrap();
    assert!(since.contains("2026-09-23 11:18"), "since 应带范例");
}

#[test]
fn search_description_warns_against_shell_glob_and_documents_truncation() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let search = &list["tools"][0];

    // 描述里必须点明三件评测中被踩过的坑：不是 glob、排除语法、截断可见。
    let desc = search["description"].as_str().unwrap();
    assert!(desc.contains("**/*.rs"), "desc should reject shell glob");
    assert!(desc.contains('!'), "desc should document the '!' exclusion prefix");
    assert!(desc.contains("total"), "desc should document the total/count pair");
    assert!(desc.contains("content:"), "desc should mention content search");
    // 新增能力也要在描述里点明：时间戳、翻页、排序。
    assert!(desc.contains("modified"), "desc should mention timestamps");
    assert!(desc.contains("offset"), "desc should document paging");

    // pattern 的参数说明同样带 glob 警告与排除范例。
    let pattern_desc = search["inputSchema"]["properties"]["pattern"]["description"]
        .as_str()
        .unwrap();
    assert!(pattern_desc.contains("**/*.rs"));
    assert!(pattern_desc.contains("ext:rs !test"));
}

#[test]
fn folder_params_document_unc_share_paths() {
    // 下午有 LLM 不会搜共享文件夹 —— 五个 folder / path 参数的说明都必须
    // 带 UNC 范例，模型才会知道 `\\server\share\project` 是合法入参。
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    for (tool, param) in [
        (0, "folder"),
        (1, "folder"),
        (2, "folder"),
        (4, "path"),
        (5, "folder"),
    ] {
        let desc = list["tools"][tool]["inputSchema"]["properties"][param]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("indexed UNC"), "desc: {desc}");
        assert!(desc.contains(r"\\server\share"), "desc: {desc}");
    }
}

#[test]
fn search_scope_documents_the_index_premise() {
    // 范围 = Everything 的索引：共享盘要先加进「选项 → 索引 → 文件夹」，
    // 没加的表现是 0 结果而不是报错。这句要进 search_in_folder / list_folder
    // 的描述和 discover instructions —— 不写清，LLM 会像下午那次一样
    // net view 逐个共享试。
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let instructions = protocol::make_discover_result()["instructions"]
        .as_str()
        .unwrap()
        .to_string();
    let docs = [
        list["tools"][0]["description"].as_str().unwrap().to_string(),
        list["tools"][1]["description"].as_str().unwrap().to_string(),
        instructions,
    ];
    for d in &docs {
        assert!(
            d.contains("Search scope follows Everything's index"),
            "desc: {d}"
        );
        assert!(d.contains("Tools > Options > Indexes > Folders"), "desc: {d}");
        assert!(d.contains("0 results, not an error"), "desc: {d}");
    }
}

#[test]
fn read_file_description_documents_the_contract() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let tool = &list["tools"][4];
    assert_eq!(tool["name"], "read_file");
    let desc = tool["description"].as_str().unwrap();

    // 描述要点明边界，否则 LLM 会拿它当搜索用、或读目录、或读巨型文件。
    assert!(desc.contains("search_in_folder"), "desc: {desc}");
    assert!(desc.contains("list_folder"), "desc: {desc}");
    assert!(desc.contains("8 MiB"), "desc should state the size limit");
    assert!(desc.contains("start_line"), "desc should document paging");
    assert!(desc.contains("encoding"), "desc should document the encoding field");
    assert!(desc.contains("NUL"), "desc should say binary files are rejected");
    // 加固后的三项：黑名单、超长行裁剪、可分支的错误码。
    assert!(desc.contains("denylist"), "desc should document the denylist");
    assert!(desc.contains("id_rsa"), "desc should name the denied files");
    assert!(desc.contains("clipped_lines"), "desc should document line clipping");
    assert!(desc.contains("next_start_line"), "desc should hand back the next offset");
    assert!(desc.contains("PATH_DENIED"), "desc should list the error codes");
    // 与 content: 检索的分工必须写清楚 —— 这是最容易混的一处。
    assert!(desc.contains("content:"), "desc: {desc}");

    let props = &tool["inputSchema"]["properties"];
    assert!(props["path"]["description"]
        .as_str()
        .unwrap()
        .contains("Absolute path"));
}

#[test]
fn read_file_arg_validation_before_touching_the_disk() {
    // 缺 path / 类型错 / 相对路径 / 通配符 —— 全部必须在读盘之前报错。
    let err = tools::dispatch("read_file", &json!({})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("path"));

    let err = tools::dispatch("read_file", &json!({"path": 42})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);

    let err = tools::dispatch("read_file", &json!({"path": "src\\lib.rs"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("absolute"), "{}", err.1);

    let err = tools::dispatch("read_file", &json!({"path": "D:\\a\\*.rs"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("pattern"), "{}", err.1);

    // start_line 是 1 起的 —— 0 属于写错，不该静默当 1 处理。
    let err = tools::dispatch(
        "read_file",
        &json!({"path": "D:\\a\\b.rs", "start_line": 0}),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("1-based"), "{}", err.1);

    let err = tools::dispatch(
        "read_file",
        &json!({"path": "D:\\a\\b.rs", "max_lines": "all"}),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("max_lines"), "{}", err.1);

    // max_lines 是 1..=4000 的闭区间：0 与超上限都属于写错
    // （0 不再有「不限」的第二含义 —— 与 PI-Desktop 的 schema 对齐）。
    for bad in [0, 4001, 999_999] {
        let err = tools::dispatch(
            "read_file",
            &json!({"path": "D:\\a\\b.rs", "max_lines": bad}),
        )
        .unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "max_lines={bad}");
        assert!(err.1.contains("max_lines"), "{}", err.1);
    }
    // 边界值放行（文件不存在会在后面报 NOT_FOUND，而不是参数错误）
    for ok in [1, 4000] {
        let out = tools::dispatch(
            "read_file",
            &json!({"path": "D:\\a\\b.rs", "max_lines": ok}),
        )
        .unwrap();
        assert!(out.1, "文件不存在应当是工具错误");
        assert_eq!(out_error(&out)["code"], "NOT_FOUND", "max_lines={ok}");
    }
}

/// 取工具结果的文本（单条 content）。
fn out_text(out: &tools::ToolOutput) -> String {
    out.0["content"][0]["text"].as_str().unwrap().to_string()
}

/// 解析 read_file 的结构化错误载荷（`{error, code, ...}`）。
fn out_error(out: &tools::ToolOutput) -> Value {
    serde_json::from_str(&out_text(out)).expect("read_file 的错误必须是 JSON")
}

#[test]
fn read_file_reads_a_real_file_and_windows_lines() {
    // 测试进程里没有主程序 host → 走 std::fs 直读路径。
    // 覆盖真实读取、行窗口、元信息与两段式 content。
    let path = std::env::temp_dir().join("everything_mcp_read_file_test.txt");
    let body = "alpha\nbravo\ncharlie\ndelta\n";
    std::fs::write(&path, body).unwrap();

    let out = tools::dispatch(
        "read_file",
        &json!({"path": path.to_string_lossy(), "start_line": 2, "max_lines": 2}),
    )
    .unwrap();
    assert!(!out.1, "read must succeed: {}", out_text(&out));

    let items = out.0["content"].as_array().unwrap();
    assert_eq!(items.len(), 2, "元信息 + 正文两段");
    let meta: Value = serde_json::from_str(items[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["total_lines"], 4);
    assert_eq!(meta["start_line"], 2);
    assert_eq!(meta["lines_returned"], 2);
    assert_eq!(meta["truncated"], true, "还有 delta 没返回");
    assert_eq!(meta["clipped_lines"], 0);
    assert_eq!(meta["next_start_line"], 4, "接着从第 4 行读");
    assert_eq!(meta["encoding"], "utf-8");
    assert_eq!(meta["size"], body.len());
    // 正文单独成项、保持原样（不是 JSON 转义过的一行）。
    assert_eq!(items[1]["text"], "bravo\ncharlie");

    // 够大的 max_lines = 余下全部，此时不再截断、也没有下一页
    let out = tools::dispatch(
        "read_file",
        &json!({"path": path.to_string_lossy(), "start_line": 1, "max_lines": 4000}),
    )
    .unwrap();
    let meta: Value =
        serde_json::from_str(out.0["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["lines_returned"], 4);
    assert_eq!(meta["truncated"], false);
    assert_eq!(meta["next_start_line"], Value::Null);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn read_file_clips_overlong_lines_and_reports_the_count() {
    // 单行超长时不能吃掉整个窗口：裁到上限并如实上报裁了几行。
    let path = std::env::temp_dir().join("everything_mcp_longline_test.txt");
    let long = "x".repeat(20_000);
    std::fs::write(&path, format!("head\n{long}\ntail\n")).unwrap();

    let out = tools::dispatch(
        "read_file",
        &json!({"path": path.to_string_lossy(), "max_lines": 4000}),
    )
    .unwrap();
    assert!(!out.1, "{}", out_text(&out));

    let meta: Value = serde_json::from_str(out.0["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["clipped_lines"], 1);
    assert_eq!(meta["total_lines"], 3);
    assert_eq!(meta["lines_returned"], 3, "裁短的行仍算一行");
    let body = out.0["content"][1]["text"].as_str().unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines[0], "head");
    assert_eq!(lines[1].chars().count(), 16_384);
    assert_eq!(lines[2], "tail");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn read_file_reports_folders_and_missing_files_as_tool_errors() {
    // 目录 / 不存在的文件是运行时问题（isError），不是协议错误 ——
    // 与 folder 参数的 INVALID_PARAMS 分工不同。错误体是结构化 JSON：
    // 调用方按 code 分支，目录那条还带建议的替代调用。
    let dir = std::env::temp_dir();
    let out = tools::dispatch("read_file", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert!(out.1, "folder must be a tool error");
    let err = out_error(&out);
    assert_eq!(err["code"], "PATH_IS_DIRECTORY");
    assert_eq!(err["suggested_tool"], "list_folder");
    assert_eq!(err["suggested_args"]["folder"], dir.to_string_lossy().as_ref());

    let missing = dir.join("everything_mcp_definitely_missing_file.txt");
    let out = tools::dispatch("read_file", &json!({"path": missing.to_string_lossy()})).unwrap();
    assert!(out.1);
    let err = out_error(&out);
    assert_eq!(err["code"], "NOT_FOUND");
    assert!(err["error"].as_str().unwrap().contains("cannot read"));
    // 不存在这种错误没有替代工具可建议
    assert!(err.get("suggested_tool").is_none());
}

#[test]
fn read_file_rejects_binary_content() {
    let path = std::env::temp_dir().join("everything_mcp_binary_probe.bin");
    std::fs::write(&path, [0x00u8, 0x01, 0x02, 0xFF, 0x00]).unwrap();

    let out = tools::dispatch("read_file", &json!({"path": path.to_string_lossy()})).unwrap();
    assert!(out.1, "binary must be rejected");
    let err = out_error(&out);
    assert_eq!(err["code"], "BINARY_CONTENT");
    assert!(err["error"].as_str().unwrap().contains("binary"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn grep_description_documents_the_contract() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let tool = &list["tools"][5];
    assert_eq!(tool["name"], "grep");
    let desc = tool["description"].as_str().unwrap();

    // 与 search_in_folder 的分工（文件粒度 vs 行粒度）必须写清楚。
    assert!(desc.contains("search_in_folder"), "desc: {desc}");
    assert!(desc.contains("line"), "desc should mention line numbers");
    assert!(desc.contains("head_limit"), "desc: {desc}");
    assert!(desc.contains("truncated"), "desc should document truncation");
    assert!(desc.contains("newest first"), "desc should state the ordering");
    // 二进制 / 超大 / 黑名单都要写明是「跳过」而不是报错。
    assert!(desc.contains("skipped"), "desc: {desc}");
    assert!(desc.contains("denylisted"), "desc should name the denylist");

    // pattern 是正则、filter 是 Everything 语法 —— 这处最容易混，必须点破。
    let props = &tool["inputSchema"]["properties"];
    let filter_desc = props["filter"]["description"].as_str().unwrap();
    assert!(
        filter_desc.contains("not the regex"),
        "filter 说明要点明它不是正则: {filter_desc}"
    );
    assert!(props["pattern"]["description"]
        .as_str()
        .unwrap()
        .contains("regular expression"));

    // head_limit 的边界要声明出来（与 tools.rs 的 MAX_HEAD_LIMIT 一致）。
    assert_eq!(props["head_limit"]["minimum"], 1);
    assert_eq!(props["head_limit"]["maximum"], 2000);
}

#[test]
fn empty_results_can_carry_a_folder_warning() {
    // 空结果与「路径写错」原先无法区分：四个 folder 类工具现在都会在
    // 空结果时附一句 folder_warning，描述里必须写明这个字段，否则 LLM 看到
    // 一个不认识的键会直接忽略。
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    for idx in [0, 1, 2, 5] {
        let name = list["tools"][idx]["name"].as_str().unwrap();
        let desc = list["tools"][idx]["description"].as_str().unwrap();
        assert!(
            desc.contains("folder_warning"),
            "{name} 的描述要点明 folder_warning: {desc}"
        );
    }
}

#[test]
fn grep_arg_validation_before_touching_everything() {
    // 全部在触达 Everything host 之前返回 —— 无主程序环境的 CI 也能跑。
    let err = tools::dispatch("grep", &json!({"pattern": "x"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("folder"));

    let err = tools::dispatch("grep", &json!({"folder": "C:\\"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("pattern"));

    let err = tools::dispatch("grep", &json!({"folder": "C:\\", "pattern": "  "})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("must not be empty"), "{}", err.1);

    let err = tools::dispatch("grep", &json!({"folder": "C:\\", "pattern": 42})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);

    // 坏正则是纯入参问题，不该等到 Everything 才发现
    let err = tools::dispatch("grep", &json!({"folder": "C:\\", "pattern": "a("})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("invalid regex"), "{}", err.1);

    let err = tools::dispatch(
        "grep",
        &json!({"folder": "C:\\", "pattern": "x", "output_mode": "summary"}),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("filesWithMatches"), "{}", err.1);

    let err = tools::dispatch(
        "grep",
        &json!({"folder": "C:\\", "pattern": "x", "output_mode": 3}),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);

    for bad in [0, 99_999] {
        let err = tools::dispatch(
            "grep",
            &json!({"folder": "C:\\", "pattern": "x", "head_limit": bad}),
        )
        .unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "head_limit={bad}");
        assert!(err.1.contains("head_limit"), "{}", err.1);
    }

    let err = tools::dispatch(
        "grep",
        &json!({"folder": "C:\\", "pattern": "x", "case_insensitive": "yes"}),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
}

#[test]
fn read_file_denies_private_keys_and_env_files() {
    // 敏感路径在读盘之前就被拒 —— 这些文件甚至不需要真实存在。
    for path in [
        r"C:\Users\someone\.ssh\id_rsa",
        r"D:\proj\.env",
        r"D:\proj\.env.production",
        r"D:\proj\certs\server.pem",
        r"D:\repo\.git\objects\ab\cdef0123",
    ] {
        let out = tools::dispatch("read_file", &json!({"path": path})).unwrap();
        assert!(out.1, "{path} 必须被拒绝");
        let err = out_error(&out);
        assert_eq!(err["code"], "PATH_DENIED", "{path}");
        assert!(
            err["error"].as_str().unwrap().contains("denylist"),
            "{path}: {}",
            err["error"]
        );
    }

    // 放行：公钥、模板 env、以及只是名字里带敏感词的普通文件
    let dir = std::env::temp_dir();
    let ok = dir.join("everything_mcp_env_example_probe.env.example");
    std::fs::write(&ok, "KEY=placeholder\n").unwrap();
    let out = tools::dispatch("read_file", &json!({"path": ok.to_string_lossy()})).unwrap();
    assert!(!out.1, ".env.example 应可读: {}", out_text(&out));
    let _ = std::fs::remove_file(&ok);
}

#[test]
fn list_folder_description_documents_zero_folder_size() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let desc = list["tools"][1]["description"].as_str().unwrap();
    assert!(desc.contains("size 0"), "desc: {desc}");
    assert!(desc.contains("recursive"));
    // 大目录必须能翻页：说明里点名 offset/total/truncated，schema 里给出硬窗口。
    assert!(desc.contains("page through with 'offset'"), "desc: {desc}");
    assert!(desc.contains("'truncated'"), "desc: {desc}");
    let schema = &list["tools"][1]["inputSchema"];
    assert_eq!(schema["properties"]["max_results"]["maximum"], 500);
    assert_eq!(schema["properties"]["max_results"]["minimum"], 1);
    assert_eq!(schema["properties"]["offset"]["default"], 0);
    // 超时也要可调，与另外三个搜索类工具对齐（原先写死 10 秒）。
    assert_eq!(schema["properties"]["timeout_ms"]["default"], 10000);
    assert!(desc.contains("timeout_ms"), "desc: {desc}");
}

#[test]
fn list_folder_window_is_validated_before_touching_everything() {
    // 0 与 501 都是写错（0 不是「不限」）—— 在触达 Everything 之前报错，
    // 所以这条测试在没有主程序的环境里也能跑。
    // 注意合法窗口不能在这里断言「没被拒」：那会走到搜索层，而 Host::get()
    // 在测试进程里没有 PM_INIT，直接 panic（这是本仓库的既有约定）。
    for bad in [0, 501] {
        let err = tools::dispatch(
            "list_folder",
            &json!({"folder": "C:\\", "max_results": bad}),
        )
        .unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "max_results={bad}");
        assert!(err.1.contains("between 1 and 500"), "{}", err.1);
    }
}

#[test]
fn count_description_documents_fast_path() {
    let list = protocol::make_tools_list(protocol::GlobalSearchMode::Deny);
    let desc = list["tools"][2]["description"].as_str().unwrap();
    assert!(desc.contains("without fetching names"), "desc: {desc}");
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
fn dispatch_tools_list_returns_seven_tools() {
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(list.len(), 7);
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

#[test]
fn exclude_of_wrong_shape_is_invalid_params_before_search() {
    // 每个工具带合法 folder，只把 exclude 写错 —— 必须在触达 Everything
    // host 之前就报错返回（CI 上没有主程序环境）。
    for name in ["search_in_folder", "list_folder", "count"] {
        let err = tools::dispatch(name, &json!({"folder": "C:\\", "exclude": 42})).unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "{}", name);
        assert!(err.1.contains("exclude"));
        let err = tools::dispatch(name, &json!({"folder": "C:\\", "exclude": [1]})).unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "{}", name);
        assert!(err.1.contains("every item"));
    }
}

#[test]
fn search_everywhere_arg_validation_before_touching_anything() {
    // 全部在触达 Everything host 与模式闸门之前返回 —— 无主程序环境的 CI 也能跑。
    // 缺 pattern / 写成非字符串：全局搜索必须有名字可找，空串落进 ≥2 字符护栏。
    let err = tools::dispatch("search_everywhere", &json!({})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("pattern"), "{}", err.1);
    let err = tools::dispatch("search_everywhere", &json!({"pattern": 42})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);

    // 两道护栏：≥2 字符、禁 content:（正文检索必须收窄到文件夹）。
    let err = tools::dispatch("search_everywhere", &json!({"pattern": "a"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("at least 2 characters"), "{}", err.1);
    let err =
        tools::dispatch("search_everywhere", &json!({"pattern": "content:secret"})).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("content:"), "{}", err.1);

    // max_results 是硬窗口：0 与 501 都是写错，不是「不限」。
    for bad in [0, 501] {
        let err = tools::dispatch(
            "search_everywhere",
            &json!({"pattern": "ab", "max_results": bad}),
        )
        .unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "max_results={bad}");
        assert!(err.1.contains("between 1 and 500"), "{}", err.1);
    }

    // content: 闸门在 exclude 上也要生效：exclude 项会被拼成 `!term` 进同一条
    // 全局查询，只查 pattern 的话就能从旁边绕过护栏。
    for bad in [
        json!({"pattern": "ab", "exclude": ["content:\"secret\""]}),
        json!({"pattern": "ab", "exclude": "case:content:\"secret\""}),
    ] {
        let err = tools::dispatch("search_everywhere", &bad).unwrap_err();
        assert_eq!(err.0, protocol::INVALID_PARAMS, "{bad}");
        assert!(err.1.contains("exclude"), "{}", err.1);
        assert!(err.1.contains("content:"), "{}", err.1);
    }
}

#[test]
fn global_exclude_cannot_smuggle_content_search() {
    use everything_mcp::mcp::validate;
    // 正常的 exclude 项（路径片段、扩展名）一律放行。
    assert!(validate::validate_global_excludes(&[]).is_ok());
    for ok in [r"\old\", r"\.git\", "ext:tmp", "node_modules"] {
        assert!(
            validate::validate_global_excludes(&[ok.to_string()]).is_ok(),
            "{ok} 应放行"
        );
    }
    // content: 一律拦下 —— 大小写与函数链写法都要认出来。
    for bad in [
        r#"content:"secret""#,
        r#"CONTENT:"secret""#,
        r#"case:content:"secret""#,
    ] {
        let e = validate::validate_global_excludes(&[bad.to_string()]).unwrap_err();
        assert!(e.contains("exclude"), "{e}");
        assert!(e.contains("content:"), "{e}");
    }
    // 只报坏的那一项，前面的正常项不影响判断。
    let e = validate::validate_global_excludes(&[r"\old\".to_string(), "content:x".to_string()])
        .unwrap_err();
    assert!(e.contains("content:x"), "{e}");
    // 注意这条闸门只属于全局搜索：文件夹范围内范围已收窄，
    // search_in_folder 的 exclude 用 content: 是合法用法（该分支不调用本函数）。
}

#[test]
fn normalize_exclude_term_collapses_doubled_backslashes() {
    use everything_mcp::mcp::validate;
    // 实机实测（1.1.4）：`\\target\\` 静默不生效，折叠后才是调用方想要的 `\target\`。
    assert_eq!(validate::normalize_exclude_term(r"\\target\\"), r"\target\");
    assert_eq!(validate::normalize_exclude_term(r"\\\obj\\\\"), r"\obj\");
    // UNC 排除项也折叠 —— Everything 对完整路径做子串匹配，
    // `\NAS\old\` 与 `\\NAS\old\` 命中同一批文件，所以「排除整个共享」照样生效。
    assert_eq!(validate::normalize_exclude_term(r"\\NAS\old\"), r"\NAS\old\");
    // 本来就正常的项一个字符都不动。
    for ok in [r"\obj\", "ext:tmp", "node_modules", r"dm:lastweek", ""] {
        assert_eq!(validate::normalize_exclude_term(ok), ok, "{ok} 不该被改动");
    }
    // 首尾空白照旧去掉。
    assert_eq!(validate::normalize_exclude_term("  \\x\\  "), r"\x\");
}

#[test]
fn search_everywhere_deny_gate_returns_global_search_disabled() {
    // 默认档（Deny）：合法入参过了校验后被模式闸门硬拒 —— 返回工具级错误
    // （is_error=true）而非 JSON-RPC 错误，载荷是结构化 JSON，LLM 一眼看出
    // 原因、开启方法与退回 search_in_folder 的提示。测试进程里的设置状态
    // 就是默认档（Deny），无需装机环境。
    let out = tools::dispatch("search_everywhere", &json!({"pattern": "*.iso"})).unwrap();
    assert!(out.1, "deny 档必须是工具错误: {}", out_text(&out));
    let err = out_error(&out);
    assert_eq!(err["code"], "GLOBAL_SEARCH_DISABLED");
    assert_eq!(err["mode"], "deny");
    assert!(err["how_to_enable"]
        .as_str()
        .unwrap()
        .contains("search_in_folder"));
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
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 7);
}

#[test]
fn legacy_tools_list_has_no_result_type() {
    // legacy 时代的规范没有 resultType 字段 —— 响应保持 2024-11-05 原形状，
    // 客户端按桥接规则把缺失当作 complete。
    let resp = dispatch(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    assert!(resp["result"].get("resultType").is_none());
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 7);
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

#[test]
fn translate_globstar_strips_leading_prefix_only() {
    use everything_mcp::mcp::validate;
    // 开头的 globstar / ./ 前缀：LLM 的 shell/ripgrep 书写习惯，
    // Everything 无此语法，剥掉后正好等于我们的递归语义。
    assert_eq!(validate::translate_globstar("**/*.cs"), "*.cs");
    assert_eq!(validate::translate_globstar("**\\*.cs"), "*.cs");
    assert_eq!(validate::translate_globstar("./*.cs"), "*.cs");
    assert_eq!(validate::translate_globstar(r".\*.cs"), "*.cs");
    assert_eq!(validate::translate_globstar("  **/ *.cs "), "*.cs");
    assert_eq!(validate::translate_globstar("**/**/*.cs"), "*.cs");
    // 普通 pattern 原样保留（含首尾空白已由 validate_pattern 处理过一层）。
    assert_eq!(validate::translate_globstar("*.rs"), "*.rs");
    assert_eq!(validate::translate_globstar(""), "");
    // 中间的 globstar 无法忠实翻译 —— 原样返回，客户端会看到 0 条自行修正。
    assert_eq!(validate::translate_globstar("src/**/*.cs"), "src/**/*.cs");
    // 尾部的 globstar 同样不属于「开头前缀」，原样保留。
    assert_eq!(validate::translate_globstar("*.cs/**"), "*.cs/**");
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

#[test]
fn index_changes_rejects_bad_params_before_touching_the_log() {
    // 全都要在触达文件系统之前就返回 INVALID_PARAMS —— CI 上没有 Everything
    // 配置也能跑，且结果确定。
    let err = tools::dispatch("index_changes", &json!({ "action": "destoryed" })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("action"), "{}", err.1);
    assert!(err.1.contains("created"), "消息应列出合法值 {}", err.1);

    let err = tools::dispatch("index_changes", &json!({ "action": 1 })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);

    // 时间格式坏：消息里必须带范例，LLM 才能一次改对。
    for bad in [
        "昨天",
        "2026-13-45",
        "2026-09-23 25:00",
        "2026-09-23T11:18:31:00",
    ] {
        for key in ["since", "until"] {
            let err = tools::dispatch("index_changes", &json!({ key: bad })).unwrap_err();
            assert_eq!(err.0, protocol::INVALID_PARAMS, "{} = {:?}", key, bad);
            assert!(err.1.contains(key), "{} = {:?} -> {}", key, bad, err.1);
        }
    }

    // 非字符串的时间参数（数字会被当 unix 秒收下，所以这里用小数）。
    let err = tools::dispatch("index_changes", &json!({ "since": 3.5 })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    // 控制字符不进 path / name。
    let err = tools::dispatch("index_changes", &json!({ "name": "a\tb" })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    let err = tools::dispatch("index_changes", &json!({ "path": 7 })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);

    // max_results 为 0 与超上限都不行，且消息写清边界。
    let err = tools::dispatch("index_changes", &json!({ "max_results": 0 })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("at least 1"), "{}", err.1);
    let err = tools::dispatch("index_changes", &json!({ "max_results": 99_999 })).unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("2000"), "上限要写进消息 {}", err.1);

    // since 晚于 until 是最容易漏的一种错：放过它只会得到 0 条且无从诊断。
    let err = tools::dispatch(
        "index_changes",
        &json!({ "since": "2026-09-23 12:00:00", "until": "2026-09-23 11:00:00" }),
    )
    .unwrap_err();
    assert_eq!(err.0, protocol::INVALID_PARAMS);
    assert!(err.1.contains("until"), "{}", err.1);
}

#[test]
fn index_changes_accepts_valid_params() {
    // 合法入参必须通过校验。查不到日志（CI / 没开 journal_log）时返回
    // is_error 的友好提示；能查到时返回带 count 的 JSON —— 两种情况都不该是
    // JSON-RPC 层面的错误。
    for args in [
        json!({}),
        json!({ "action": "any" }),
        json!({ "action": "created", "max_results": 10 }),
        json!({ "path": "C:\\", "name": "readme", "since": "2026-09-01", "until": "2026-09-23 11:18" }),
        json!({ "since": "2026-09-23T11:18", "until": "" }),
        json!({ "max_results": 2000 }),
    ] {
        let (out, is_error) = tools::dispatch("index_changes", &args).expect("合法入参不该报错");
        let text = out["content"][0]["text"]
            .as_str()
            .expect("content 应是文本");
        if is_error {
            assert!(text.starts_with("index_changes error"), "{}", text);
            assert!(
                text.contains("journal_log"),
                "报错要给出开关相关的做法: {}",
                text
            );
        } else {
            let parsed: Value = serde_json::from_str(text).expect("成功响应应是 JSON");
            assert!(parsed["count"].is_number(), "{}", text);
            assert!(parsed["changes"].is_array(), "{}", text);
        }
    }
}

#[test]
fn index_changes_happy_path_shape_is_readable() {
    // 这台开发机上开着 journal_log，能真跑通一遍。万一没有日志，就以
    // 友好报错成立，不强求查到数据。
    let (out, is_error) =
        tools::dispatch("index_changes", &json!({ "max_results": 5 })).expect("合法入参不该报错");
    let text = out["content"][0]["text"].as_str().unwrap();
    if is_error {
        assert!(text.contains("journal_log"), "{}", text);
        return;
    }
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed["count"].as_u64().unwrap() <= 5, "{}", text);
    // 每条变更都要带 LLM 判断所需的字段，且动作值来自封闭集合。
    for c in parsed["changes"].as_array().unwrap() {
        assert!(c["date"].is_string() && c["path"].is_string());
        assert!(c["action"].is_string(), "{}", c);
        assert!(
            ["created", "modified", "deleted", "renamed", "moved", "other"]
                .contains(&c["action"].as_str().unwrap()),
            "{}",
            c
        );
        assert!(
            c["action_text"].is_string(),
            "原始的本地化动作串要保留 {}",
            c
        );
        assert!(
            ["file", "folder"].contains(&c["kind"].as_str().unwrap()),
            "{}",
            c
        );
    }
    // 倒序：最新的在最前面。
    let dates: Vec<&str> = parsed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["date"].as_str().unwrap())
        .collect();
    let mut sorted = dates.clone();
    sorted.sort_unstable();
    sorted.reverse();
    assert_eq!(dates, sorted, "应按时间倒序返回");
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
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 7);
    // 清单里应当能看到新工具，且按现有顺序排在最后。
    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names[3], "index_changes");
    assert_eq!(resp["result"]["tools"][3]["name"], "index_changes");
    assert_eq!(names[6], "search_everywhere");
    assert_eq!(resp["result"]["tools"][6]["name"], "search_everywhere");

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
