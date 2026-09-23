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

/// 把 pattern 包成 Everything 的 `regex:` 搜索函数项。
///
/// 正则不能用 `db_query_search2` 的 match_regex 开关表达：那会把整条搜索串
/// （含我们前置的文件夹路径前缀）当成一个正则编译，路径里的 `\s`、`\.` 之类
/// 让结果恒为 0（实测）。`regex:` 只作用于紧跟其后的这一项，与文件夹前缀天然
/// 共存。空 pattern 或未开正则时原样返回。
fn regex_term(pattern: &str, match_regex: bool) -> String {
    if match_regex && !pattern.is_empty() {
        format!("regex:{}", pattern)
    } else {
        pattern.to_string()
    }
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

/// 取布尔参数：缺省用默认值。
///
/// 只接受真正的 JSON 布尔 —— 字符串 `"false"` 会被 `Value::as_bool` 拒掉并报错。
/// 这样 LLM 把布尔写成字符串时能立刻收到带范例的错误，而不是被当成真值。
fn bool_arg(args: &Value, key: &str, default: bool) -> Result<bool, (i32, String)> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(v) => Err((
            INVALID_PARAMS,
            format!(
                "'{}' must be a boolean (true or false); received {}",
                key, v
            ),
        )),
    }
}

/// 取排序键参数：缺省按名字升序；未知取值报错并列出合法值。
fn sort_arg(args: &Value) -> Result<plugin::search::SortKey, (i32, String)> {
    let raw = match args.get("sort") {
        None | Some(Value::Null) => return Ok(plugin::search::SortKey::Name),
        Some(Value::String(s)) => s.as_str(),
        Some(v) => {
            return Err((
                INVALID_PARAMS,
                format!("'sort' must be a string; received {}", v),
            ))
        }
    };
    plugin::search::SortKey::parse(raw).ok_or((
        INVALID_PARAMS,
        format!(
            "'sort' must be one of name|path|size|modified|created; received {:?}",
            raw
        ),
    ))
}

/// `index_changes` 的默认返回条数。
const INDEX_CHANGES_DEFAULT: u64 = 50;

/// `index_changes` 的返回条数上限。日志一行一条，2000 条已是很长的排查
/// 历史；再加限是为了不让一次调用把整天的日志啃穿。
const INDEX_CHANGES_LIMIT: u64 = 2000;

/// 把 `action` 参数映射成 journal 的动作枚举。
///
/// 接收 `Action::as_str()` 的全部取值外加 `any`（不限）。空串与缺省都视为
/// 不限 —— LLM 常传 `""` 表示「不过滤」。顺手收几个同义字：LLM 偶尔把
/// 动作写成动词原形或 updated/removed。
fn action_arg(args: &Value) -> Result<Option<plugin::journal::Action>, (i32, String)> {
    let raw = match args.get("action") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.as_str(),
        Some(v) => {
            return Err((
                INVALID_PARAMS,
                format!(
                "'action' must be one of created|modified|deleted|renamed|moved|any; received {}",
                v
            ),
            ))
        }
    };
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("any") {
        return Ok(None);
    }
    use plugin::journal::Action::*;
    let action = match raw.to_ascii_lowercase().as_str() {
        "created" | "create" | "new" => Created,
        "modified" | "modify" | "updated" | "changed" => Modified,
        "deleted" | "delete" | "removed" => Deleted,
        "renamed" | "rename" => Renamed,
        "moved" | "move" => Moved,
        _ => {
            return Err((
                INVALID_PARAMS,
                format!(
                "'action' must be one of created|modified|deleted|renamed|moved|any; received {:?}",
                raw
            ),
            ))
        }
    };
    Ok(Some(action))
}

/// `index_changes` 的时间参数规范化。缺省/空串都视为不限。
fn timestamp_arg(args: &Value, key: &str) -> Result<Option<String>, (i32, String)> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => plugin::journal::normalize_timestamp(s)
            .map(Some)
            .map_err(|e| (INVALID_PARAMS, format!("'{}': {}", key, e))),
        Some(v) => Err((
            INVALID_PARAMS,
            format!(
                "'{}' must be a timestamp string like '2026-09-23 11:18'; received {}",
                key, v
            ),
        )),
    }
}

/// `index_changes` 的可选字符串子串参数（`path` / `name`）。
fn optional_text_arg(args: &Value, key: &str) -> Result<Option<String>, (i32, String)> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => {
            if s.chars().any(|c| c.is_control()) {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'{}' must not contain control characters; received {:?}",
                        key, s
                    ),
                ));
            }
            Ok(Some(s.trim().to_string()))
        }
        Some(v) => Err((
            INVALID_PARAMS,
            format!("'{}' must be a string; received {}", key, v),
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
            let match_regex = bool_arg(args, "match_regex", false)?;
            // 正则改用 `regex:` 搜索函数（见 regex_term 注释），只包 pattern，
            // 排除项 `!term` 保持独立。
            let query = combine_query(&regex_term(&pattern, match_regex), &excludes);
            let offset = u64_arg(args, "offset", 0)? as usize;
            let max_results = u64_arg(args, "max_results", 50)? as usize;
            let timeout_ms = u64_arg(args, "timeout_ms", 10_000)? as u32;
            let sort = sort_arg(args)?;
            let descending = bool_arg(args, "descending", false)?;
            let options = plugin::search::SearchOptions {
                scope: plugin::search::SearchScope::Recursive,
                match_case: bool_arg(args, "match_case", false)?,
                match_whole_word: bool_arg(args, "match_whole_word", false)?,
                sort,
                descending,
            };

            match plugin::search::search_in_folder(
                &folder,
                &query,
                offset,
                max_results,
                timeout_ms,
                options,
            ) {
                Ok(outcome) => {
                    let entries: Vec<Value> = outcome
                        .results
                        .iter()
                        .map(|r| {
                            json!({
                                "name": r.name,
                                "path": r.path,
                                "kind": if r.is_folder { "folder" } else { "file" },
                                "size": r.size,
                                "modified": r.modified,
                                "created": r.created,
                            })
                        })
                        .collect();
                    let text = serde_json::to_string_pretty(&json!({
                        "folder": folder,
                        "pattern": pattern,
                        "exclude": excludes,
                        "sort": sort.as_str(),
                        "descending": descending,
                        "offset": outcome.offset,
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
                0,
                500,
                10_000,
                plugin::search::SearchOptions::for_scope(plugin::search::SearchScope::Children),
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
                                "modified": r.modified,
                                "created": r.created,
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

        "index_changes" => {
            // 所有入参先规范化完再触达插件：坏 action / 坏时间格式在这里就
            // 报 INVALID_PARAMS，消息里带范例，LLM 一次就能改对。
            let action = action_arg(args)?;
            let path = optional_text_arg(args, "path")?;
            let name = optional_text_arg(args, "name")?;
            let since = timestamp_arg(args, "since")?;
            let until = timestamp_arg(args, "until")?;
            let max_results = u64_arg(args, "max_results", INDEX_CHANGES_DEFAULT)? as usize;

            if max_results == 0 {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'max_results' must be at least 1 (and at most {}); received 0",
                        INDEX_CHANGES_LIMIT
                    ),
                ));
            }
            if max_results as u64 > INDEX_CHANGES_LIMIT {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'max_results' must not exceed {}; received {}. \
                         Narrow the query with 'action', 'path', 'name' or a 'since'/'until' window instead.",
                        INDEX_CHANGES_LIMIT, max_results
                    ),
                ));
            }
            // since 晚于 until 是最容易犯的错：放过它只会得到 0 条且无从诊断。
            if let (Some(s), Some(u)) = (&since, &until) {
                if s > u {
                    return Err((
                        INVALID_PARAMS,
                        format!("'since' ({}) must not be later than 'until' ({})", s, u),
                    ));
                }
            }

            let filter = plugin::journal::Filter {
                action,
                path,
                name,
                since,
                until,
                max_results,
            };

            match plugin::journal::query(&filter) {
                Ok(outcome) => {
                    let changes: Vec<Value> = outcome
                        .changes
                        .iter()
                        .map(|c| {
                            json!({
                                "journal_id": c.journal_id,
                                "change_id": c.change_id,
                                "date": c.date,
                                "action": c.action.as_str(),
                                "action_text": c.action_text,
                                "kind": if c.is_folder { "folder" } else { "file" },
                                "path": c.path,
                                "name": c.name(),
                                "new_path": c.new_path,
                            })
                        })
                        .collect();
                    let text = serde_json::to_string_pretty(&json!({
                        "action": filter.action.map(|a| a.as_str()),
                        "path": filter.path,
                        "name": filter.name,
                        "since": filter.since,
                        "until": filter.until,
                        "count": outcome.count,
                        "truncated": outcome.truncated,
                        "days_searched": outcome.days_searched,
                        "skipped_lines": outcome.skipped_lines,
                        "log_directory": outcome.log_directory,
                        "changes": changes,
                    }))
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());

                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("index_changes error: {}", e)), true)),
            }
        }

        "read_file" => {
            // 纯入参问题（缺参数 / 相对路径 / 通配符）在触达磁盘之前就报
            // INVALID_PARAMS —— 与 folder 参数的处理一致，CI 上无主程序也能跑。
            let path = match args.get("path").and_then(Value::as_str) {
                Some(p) => plugin::read::validate_path(p).map_err(|e| (INVALID_PARAMS, e))?,
                None => {
                    return Err((
                        INVALID_PARAMS,
                        "'path' is required and must be a string: the absolute path of the file to read".into(),
                    ))
                }
            };
            let start_line = u64_arg(args, "start_line", 1)? as usize;
            let max_lines =
                u64_arg(args, "max_lines", plugin::read::DEFAULT_MAX_LINES as u64)? as usize;
            if start_line == 0 {
                return Err((
                    INVALID_PARAMS,
                    "'start_line' is 1-based; use 1 for the first line".into(),
                ));
            }

            match plugin::read::read_file(&path, start_line, max_lines) {
                Ok(c) => {
                    let meta = serde_json::to_string_pretty(&json!({
                        "path": c.path,
                        "size": c.size,
                        "total_lines": c.total_lines,
                        "start_line": c.start_line,
                        "lines_returned": c.lines_returned,
                        "truncated": c.truncated,
                        "encoding": c.encoding,
                    }))
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());
                    Ok((content_meta_and_body(&meta, &c.text), false))
                }
                Err(e) => Ok((content_text(&format!("read_file error: {}", e)), true)),
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

/// 构造两段式 `content` 数组：先是元信息，再是正文原文。
///
/// 正文单独成项而不是塞进 JSON —— 否则换行会变成 `\n` 转义，
/// 读代码时既难读也容易在后续引用时出错。
fn content_meta_and_body(meta: &str, body: &str) -> Value {
    json!({
        "content": [
            { "type": "text", "text": meta },
            { "type": "text", "text": body }
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
    fn regex_term_wraps_only_when_enabled_and_nonempty() {
        assert_eq!(regex_term("^README", true), "regex:^README");
        assert_eq!(regex_term("^README", false), "^README");
        // 空 pattern 不包 —— `regex:` 后面没东西会变成无效项。
        assert_eq!(regex_term("", true), "");
        assert_eq!(regex_term("", false), "");
        // 与 exclude 组合时只有 pattern 带前缀。
        assert_eq!(
            combine_query(&regex_term(r"^.*\.rs$", true), &v(&[r"\obj\"])),
            r#"regex:^.*\.rs$ !\obj\"#
        );
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

    #[test]
    fn action_arg_maps_synonyms_and_rejects_junk() {
        use plugin::journal::Action::*;
        // 缺省 / 空串 / any 都表示不限。
        assert_eq!(action_arg(&json!({})).unwrap(), None);
        assert_eq!(action_arg(&json!({ "action": null })).unwrap(), None);
        assert_eq!(action_arg(&json!({ "action": "" })).unwrap(), None);
        assert_eq!(action_arg(&json!({ "action": "any" })).unwrap(), None);
        assert_eq!(action_arg(&json!({ "action": "ANY" })).unwrap(), None);

        // 大小写不敏感；顺手收几个 LLM 常写的同义字与动词原形。
        assert_eq!(
            action_arg(&json!({ "action": "created" })).unwrap(),
            Some(Created)
        );
        assert_eq!(
            action_arg(&json!({ "action": "Deleted" })).unwrap(),
            Some(Deleted)
        );
        assert_eq!(
            action_arg(&json!({ "action": "modified" })).unwrap(),
            Some(Modified)
        );
        assert_eq!(
            action_arg(&json!({ "action": "updated" })).unwrap(),
            Some(Modified)
        );
        assert_eq!(
            action_arg(&json!({ "action": "removed" })).unwrap(),
            Some(Deleted)
        );
        assert_eq!(
            action_arg(&json!({ "action": "rename" })).unwrap(),
            Some(Renamed)
        );
        assert_eq!(
            action_arg(&json!({ "action": "move" })).unwrap(),
            Some(Moved)
        );

        // 消息必须列出合法值，让 LLM 一次改对。
        let e = action_arg(&json!({ "action": "destoryed" })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        assert!(
            e.1.contains("created") && e.1.contains("renamed"),
            "{}",
            e.1
        );
        let e = action_arg(&json!({ "action": ["created"] })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
    }

    #[test]
    fn timestamp_arg_normalizes_and_rejects_bad_types() {
        assert_eq!(timestamp_arg(&json!({}), "since").unwrap(), None);
        assert_eq!(
            timestamp_arg(&json!({ "since": "  " }), "since").unwrap(),
            None
        );
        assert_eq!(
            timestamp_arg(&json!({ "since": "2026-09-23" }), "since").unwrap(),
            Some("2026-09-23 00:00:00".to_string())
        );

        let e = timestamp_arg(&json!({ "until": "昨天" }), "until").unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        assert!(e.1.contains("until"), "{}", e.1);
        // 数字会被 literal 的 normalize_timestamp 当 unix 秒收下，所以类型
        // 错误用小数来试。
        let e = timestamp_arg(&json!({ "since": 3.5 }), "since").unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
    }

    #[test]
    fn optional_text_arg_trims_and_rejects_control_chars() {
        assert_eq!(optional_text_arg(&json!({}), "path").unwrap(), None);
        assert_eq!(
            optional_text_arg(&json!({ "name": "   " }), "name").unwrap(),
            None
        );
        assert_eq!(
            optional_text_arg(&json!({ "name": "  readme  " }), "name").unwrap(),
            Some("readme".to_string())
        );
        let e = optional_text_arg(&json!({ "path": "a\tb" }), "path").unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        assert!(e.1.contains("path"), "{}", e.1);
        let e = optional_text_arg(&json!({ "path": 42 }), "path").unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
    }

    #[test]
    fn bool_arg_accepts_only_json_bools() {
        assert!(!bool_arg(&json!({}), "match_case", false).unwrap());
        assert!(bool_arg(&json!({}), "match_case", true).unwrap());
        assert!(bool_arg(&json!({ "match_case": true }), "match_case", false).unwrap());
        // null 走默认值。
        assert!(bool_arg(&json!({ "match_case": null }), "match_case", true).unwrap());
        // 字符串 "false" 不是布尔 —— 必须报错，否则会被当成真值。
        let e = bool_arg(&json!({ "match_case": "false" }), "match_case", false).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        assert!(e.1.contains("match_case"), "{}", e.1);
    }

    #[test]
    fn sort_arg_defaults_and_rejects_junk() {
        use plugin::search::SortKey;
        assert_eq!(sort_arg(&json!({})).unwrap(), SortKey::Name);
        assert_eq!(sort_arg(&json!({ "sort": null })).unwrap(), SortKey::Name);
        assert_eq!(sort_arg(&json!({ "sort": "size" })).unwrap(), SortKey::Size);
        assert_eq!(
            sort_arg(&json!({ "sort": "modified" })).unwrap(),
            SortKey::DateModified
        );
        // 错误消息必须列出合法值，让 LLM 一次改对。
        let e = sort_arg(&json!({ "sort": "bogus" })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
        assert!(e.1.contains("modified"), "{}", e.1);
        let e = sort_arg(&json!({ "sort": 3 })).unwrap_err();
        assert_eq!(e.0, INVALID_PARAMS);
    }
}
