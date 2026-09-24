//! tools.rs — 工具实现
//!
//! 把 JSON-RPC 调用分发到具体逻辑，调用插件搜索层。
//! 入参先过 `validate` 规范化/校验：错误消息带范例，LLM 客户端拿到
//! INVALID_PARAMS 就能一次改对，不会带着坏参数盲目重试。

use serde_json::{json, Value};

use crate::mcp::protocol::{GlobalSearchMode, INVALID_PARAMS, METHOD_NOT_FOUND};
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
///
/// 每项过 [`validate::normalize_exclude_term`]：折叠重复反斜杠，否则
/// `\\target\\` 这种转义失误会静默不生效（见该函数的注释与实测数据）。
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
        terms.push(validate::normalize_exclude_term(t));
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

/// 取 `timeout_ms`：必须是能装进 u32 的正整数。
///
/// 原先三处都是 `u64_arg(...)? as u32` —— 传 4294967296（2^32）截断成 0，
/// 等待循环一次都不跑，调用方拿到 "search timeout after 0 ms" 这种毫无线索的
/// 错误；0 同理（等价于立刻放弃），也按写错处理。这类「坏值静默变形」与
/// exclude 的重复反斜杠是同一类毛病，一律改成带范围的显式校验。
fn timeout_arg(args: &Value, default: u32) -> Result<u32, (i32, String)> {
    let v = u64_arg(args, "timeout_ms", default as u64)?;
    if v == 0 || v > u32::MAX as u64 {
        return Err((
            INVALID_PARAMS,
            format!(
                "'timeout_ms' must be between 1 and {}; received {}",
                u32::MAX,
                v
            ),
        ));
    }
    Ok(v as u32)
}

/// 目录可用性软信号：文件夹看起来不对劲时给一句话，正常时 None。
///
/// 为什么是软信号而不是报错：搜不到东西可能是「路径写错」，也可能是「网络共享
/// 没加进 Everything 的索引」或「共享当前离线」—— 后两种是合法用法（本插件的
/// NAS 场景正是它），报错会把它们误判成参数错误。所以结果照常返回，只在响应里
/// 附一句 `folder_warning` 让调用方分得清。
///
/// **只在结果为空时才调用**（调用方负责）：一是正常路径上不做多余的磁盘探测，
/// 二是探测本身对离线共享可能阻塞到 SMB 超时 —— 那种代价只该付在「反正什么
/// 都没搜到」的调用上。
fn folder_warning(folder: &str) -> Option<String> {
    match std::fs::metadata(folder) {
        Ok(m) if m.is_dir() => None,
        Ok(_) => Some(format!(
            "{:?} is a file, not a folder — nothing can be listed or searched under it; use read_file for its contents",
            folder
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(format!(
            "{:?} does not exist on this machine (or its drive/share is offline), so the empty result is expected — check the path. A network share also returns 0 results until it is added in Everything > Tools > Options > Indexes > Folders",
            folder
        )),
        Err(e) => Some(format!(
            "{:?} could not be accessed ({}), so the result may be incomplete — for a network share this usually means it is offline",
            folder, e
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

/// `search_everywhere` 的返回条数上限（1..=500）。全局命中集可能是几十万条，
/// 窗口必须封顶 —— 0 也不再有「不限」的第二含义，越界直接报 INVALID_PARAMS。
const GLOBAL_MAX_RESULTS: u64 = 500;

/// `list_folder` 每页的子项上限（同时是默认值）。目录的直接子项可以上千
/// （实测 `C:\Windows\System32` 有 4889 个），所以窗口必须能翻页：`offset`
/// 往后走，`truncated` 告诉调用方还有没有下一页。上限取 500 是为了让一页的
/// 响应体不至于大到被客户端截断。
const LIST_MAX_RESULTS: usize = 500;

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
            let timeout_ms = timeout_arg(args, 10_000)?;
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
                    let mut payload = json!({
                        "folder": folder,
                        "pattern": pattern,
                        "exclude": excludes,
                        "sort": sort.as_str(),
                        "descending": descending,
                        "offset": outcome.offset,
                        "count": entries.len(),
                        "total": outcome.total,
                        "results": entries,
                    });
                    // 一条都没搜到时才去探目录 —— 分得清「空目录」与「路径不对」。
                    if outcome.total == 0 {
                        if let Some(w) = folder_warning(&folder) {
                            payload["folder_warning"] = json!(w);
                        }
                    }
                    let text = serde_json::to_string_pretty(&payload)
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
            let offset = u64_arg(args, "offset", 0)? as usize;
            // 窗口必须能翻页：大目录的直接子项可以上千（实测 C:\Windows\System32
            // 有 4889 个），旧实现写死 500 条且没有 offset，第 501 项之后拿不到。
            let max_results = u64_arg(args, "max_results", LIST_MAX_RESULTS as u64)? as usize;
            if max_results == 0 || max_results > LIST_MAX_RESULTS {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'max_results' must be between 1 and {}; received {}",
                        LIST_MAX_RESULTS, max_results
                    ),
                ));
            }
            // 用「直接子项」范围（SearchScope::Children → parent:"<folder>"）：
            // 无排除项时 query 为空串，等价于列出该目录全部直接子项，不递归。
            // 超时同样可调：一个挂着离线共享的目录要等多久，调用方说了算
            // （原先写死 10 秒，与另外三个搜索类工具不对称）。
            let timeout_ms = timeout_arg(args, 10_000)?;
            match plugin::search::search_in_folder(
                &folder,
                &query,
                offset,
                max_results,
                timeout_ms,
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
                    let returned = entries.len();
                    let mut payload = json!({
                        "folder": folder,
                        "exclude": excludes,
                        "offset": outcome.offset,
                        "count": returned,
                        "total": outcome.total,
                        // 还有子项没返回 —— 用 offset 翻下一页（与 search_in_folder 同形）。
                        "truncated": outcome.offset + returned < outcome.total,
                        "items": entries,
                    });
                    if outcome.total == 0 {
                        if let Some(w) = folder_warning(&folder) {
                            payload["folder_warning"] = json!(w);
                        }
                    }
                    let text = serde_json::to_string_pretty(&payload)
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
                    // 用 json! 而不是手拼 format! —— 手拼的 `{:?}` 走的是 Rust
                    // Debug 转义（非 JSON 转义），只是个巧合才对得上。
                    let mut payload = json!({
                        "folder": folder,
                        "pattern": pattern,
                        "exclude": excludes,
                        "count": n,
                    });
                    if n == 0 {
                        if let Some(w) = folder_warning(&folder) {
                            payload["folder_warning"] = json!(w);
                        }
                    }
                    let text = serde_json::to_string(&payload)
                        .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());
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
            // 与 PI-Desktop 的 schema 对齐：min 1 / max 4000，不再有「0 = 不限」。
            if max_lines == 0 || max_lines > plugin::read::MAX_MAX_LINES {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'max_lines' must be between 1 and {}; received {}",
                        plugin::read::MAX_MAX_LINES,
                        max_lines
                    ),
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
                        "next_start_line": c.next_start_line,
                        "truncated": c.truncated,
                        "clipped_lines": c.clipped_lines,
                        "encoding": c.encoding,
                    }))
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());
                    Ok((content_meta_and_body(&meta, &c.text), false))
                }
                Err(e) => {
                    // 结构化错误：给机器可读的 code，调用方据此决定换工具还是
                    // 改参数，不必解析文案。
                    let plugin::read::ReadError {
                        code,
                        message,
                        path,
                    } = e;
                    let mut payload = json!({ "error": message, "code": code });
                    // 目录是唯一有明确替代品的情况 —— 直接把建议的调用参数
                    // 一并给出，省掉一轮试错。
                    if code == plugin::read::ERR_PATH_IS_DIRECTORY {
                        payload["suggested_tool"] = json!("list_folder");
                        payload["suggested_args"] = json!({ "folder": path });
                    }
                    let text = serde_json::to_string_pretty(&payload)
                        .unwrap_or_else(|_| format!("read_file error: {}", code));
                    Ok((content_text(&text), true))
                }
            }
        }

        "grep" => {
            // 入参校验先行：这些都在触达 Everything 之前完成（CI 无主程序也能跑）。
            let folder = require_folder(args)?;
            let pattern = match args.get("pattern").and_then(Value::as_str) {
                Some(p) if !p.trim().is_empty() => p.to_string(),
                Some(_) => {
                    return Err((
                        INVALID_PARAMS,
                        "'pattern' must not be empty: it is a regular expression matched per line".into(),
                    ))
                }
                None => {
                    return Err((
                        INVALID_PARAMS,
                        "'pattern' is required and must be a string: a regular expression matched per line".into(),
                    ))
                }
            };
            // 'filter' 是交给 Everything 的候选筛选串，不是正则 —— 与 pattern 分工不同。
            let filter = optional_text_arg(args, "filter")?.unwrap_or_default();
            let excludes = exclude_arg(args)?;
            let candidate_filter = combine_query(&filter, &excludes);

            // 正则能不能编译属于纯入参问题 —— 在触达 Everything 之前就报。
            plugin::grep::validate_regex(&pattern).map_err(|e| (INVALID_PARAMS, e))?;

            let mode = match args.get("output_mode") {
                None | Some(Value::Null) => plugin::grep::OutputMode::Content,
                Some(Value::String(s)) => plugin::grep::OutputMode::parse(s).ok_or_else(|| {
                    (
                        INVALID_PARAMS,
                        format!(
                            "'output_mode' must be one of content|filesWithMatches|count; received {:?}",
                            s
                        ),
                    )
                })?,
                Some(v) => {
                    return Err((
                        INVALID_PARAMS,
                        format!("'output_mode' must be a string; received {}", v),
                    ))
                }
            };

            let head_limit =
                u64_arg(args, "head_limit", plugin::grep::DEFAULT_HEAD_LIMIT as u64)? as usize;
            if head_limit == 0 || head_limit > plugin::grep::MAX_HEAD_LIMIT {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'head_limit' must be between 1 and {}; received {}",
                        plugin::grep::MAX_HEAD_LIMIT,
                        head_limit
                    ),
                ));
            }
            let case_insensitive = bool_arg(args, "case_insensitive", false)?;
            let timeout_ms = timeout_arg(args, plugin::grep::DEFAULT_TIMEOUT_MS)?;

            match plugin::grep::grep(
                &folder,
                &pattern,
                &candidate_filter,
                mode,
                head_limit,
                case_insensitive,
                timeout_ms,
            ) {
                Ok(o) => {
                    let mut out = json!({
                        "folder": folder,
                        "pattern": pattern,
                        "filter": candidate_filter,
                        "output_mode": o.mode.as_str(),
                        "head_limit": o.head_limit,
                        "candidates": o.candidates,
                        "candidates_truncated": o.candidates_truncated,
                        "files_scanned": o.files_scanned,
                        "bytes_scanned": o.bytes_scanned,
                        "files_with_matches": o.files_with_matches,
                        "clipped_lines": o.clipped_lines,
                        "count": o.payload.len(),
                        "truncated": o.truncated,
                    });
                    // 载荷键随模式变：matches / files / counts。
                    match &o.payload {
                        plugin::grep::GrepPayload::Content(hits) => {
                            out["matches"] = json!(hits
                                .iter()
                                .map(|h| json!({
                                    "path": h.path,
                                    "line": h.line,
                                    "text": h.text,
                                }))
                                .collect::<Vec<_>>());
                        }
                        plugin::grep::GrepPayload::Files(files) => {
                            out["files"] = json!(files);
                        }
                        plugin::grep::GrepPayload::Counts(counts) => {
                            out["counts"] = json!(counts
                                .iter()
                                .map(|(path, count)| json!({ "path": path, "count": count }))
                                .collect::<Vec<_>>());
                        }
                    }
                    // 一个命中都没有时才探目录 —— 分得清「确实没有」与「路径不对」。
                    if o.files_with_matches == 0 {
                        if let Some(w) = folder_warning(&folder) {
                            out["folder_warning"] = json!(w);
                        }
                    }
                    let text = serde_json::to_string_pretty(&out)
                        .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());
                    Ok((content_text(&text), false))
                }
                Err(e) => Ok((content_text(&format!("grep error: {}", e)), true)),
            }
        }

        "search_everywhere" => {
            // 全局按名搜索：整个索引（所有本地盘 + 已索引网络共享），不限文件夹。
            // 顺序刻意如此：入参校验先行（纯入参问题，无主程序环境的 CI 也能跑），
            // 再过 mcp_global_search 模式闸门，最后才触达搜索层。
            let pattern = pattern_arg(args)?;
            let pattern =
                validate::validate_global_pattern(&pattern).map_err(|e| (INVALID_PARAMS, e))?;
            let excludes = exclude_arg(args)?;
            // exclude 项会拼进同一条查询，content: 闸门必须两边都查 ——
            // 只查 pattern 的话 `exclude: ["content:…"]` 就绕过去了。
            validate::validate_global_excludes(&excludes).map_err(|e| (INVALID_PARAMS, e))?;
            let match_regex = bool_arg(args, "match_regex", false)?;
            let query = combine_query(&regex_term(&pattern, match_regex), &excludes);
            let offset = u64_arg(args, "offset", 0)? as usize;
            // 全局窗口必须封顶：0 不是「不限」而是写错（与 read_file 的
            // max_lines 同一形态）。
            let max_results = u64_arg(args, "max_results", 50)?;
            if max_results == 0 || max_results > GLOBAL_MAX_RESULTS {
                return Err((
                    INVALID_PARAMS,
                    format!(
                        "'max_results' must be between 1 and {}; received {}",
                        GLOBAL_MAX_RESULTS, max_results
                    ),
                ));
            }
            let timeout_ms = u64_arg(args, "timeout_ms", 10_000)? as u32;
            let sort = sort_arg(args)?;
            let descending = bool_arg(args, "descending", false)?;

            // 模式闸门：只有 Deny 档在这里硬拒（结构化错误 + 开启指引）。
            // Review / Allow 都放行到搜索层 —— Review 档的把关是客户端的
            // 确认弹窗（工具注解驱动），服务端不重复设卡。
            let mode = crate::options::global_search_mode();
            if mode == GlobalSearchMode::Deny {
                let payload = serde_json::to_string_pretty(&json!({
                    "error": "global search is disabled on this server (global search mode = 'deny')",
                    "code": "GLOBAL_SEARCH_DISABLED",
                    "mode": mode.as_str(),
                    "how_to_enable": "set Everything > Options > Plugins > MCP > global search to 'review' or 'allow' and click Apply; until then use search_in_folder with a folder",
                }))
                .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into());
                return Ok((content_text(&payload), true));
            }

            let options = plugin::search::SearchOptions {
                scope: plugin::search::SearchScope::Global,
                match_case: bool_arg(args, "match_case", false)?,
                match_whole_word: bool_arg(args, "match_whole_word", false)?,
                sort,
                descending,
            };

            match plugin::search::search_everywhere(
                &query,
                offset,
                max_results as usize,
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
                        "mode": mode.as_str(),
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
    fn exclude_arg_collapses_doubled_backslashes() {
        // 1.1.4 实机实测：`\\target\\` 一条都没排除掉且不报错（30 条 vs 22 条），
        // 因为 Everything 拿它当字面路径片段匹配。这里确认参数层已经折叠。
        let doubled = json!({ "exclude": [r"\\target\\", r"\\obj\\"] });
        assert_eq!(
            exclude_arg(&doubled).unwrap(),
            v(&[r"\target\", r"\obj\"]),
            "重复反斜杠要折叠成单个"
        );
        // UNC 排除项（排除整个共享）同样只留单个反斜杠：Everything 对完整路径
        // 做子串匹配，`\NAS\old\` 与 `\\NAS\old\` 命中同一批文件。
        assert_eq!(
            exclude_arg(&json!({ "exclude": r"\\NAS\old\" })).unwrap(),
            v(&[r"\NAS\old\"])
        );
        // 没有重复反斜杠的项原样保留（含搜索函数与普通名字）。
        for ok in [r"\obj\", "ext:tmp", "node_modules", r"dm:lastweek"] {
            assert_eq!(
                exclude_arg(&json!({ "exclude": ok })).unwrap(),
                v(&[ok]),
                "{ok} 不该被改动"
            );
        }
    }

    #[test]
    fn timeout_arg_rejects_values_that_used_to_truncate_silently() {
        // 缺省走默认值。
        assert_eq!(timeout_arg(&json!({}), 10_000).unwrap(), 10_000);
        assert_eq!(timeout_arg(&json!({ "timeout_ms": null }), 7).unwrap(), 7);
        // 正常值原样通过。
        assert_eq!(timeout_arg(&json!({ "timeout_ms": 30_000 }), 1).unwrap(), 30_000);
        assert_eq!(
            timeout_arg(&json!({ "timeout_ms": u32::MAX }), 1).unwrap(),
            u32::MAX
        );
        // 2^32 原先 `as u32` 截成 0 → 等待循环一次不跑 → "search timeout after 0 ms"。
        // 0 等价于立刻放弃，同样按写错处理。
        for bad in [0u64, 1u64 << 32, u64::MAX] {
            let e = timeout_arg(&json!({ "timeout_ms": bad }), 1).unwrap_err();
            assert_eq!(e.0, INVALID_PARAMS, "timeout_ms={bad}");
            assert!(e.1.contains("timeout_ms"), "{}", e.1);
            assert!(e.1.contains("between 1 and"), "{}", e.1);
        }
    }

    #[test]
    fn folder_warning_distinguishes_empty_from_wrong_path() {
        // 真实存在的目录：没有话要说。
        let dir = std::env::temp_dir();
        assert_eq!(folder_warning(&dir.to_string_lossy()), None);

        // 真实存在的文件：提醒改用 read_file。
        let file = dir.join("everything_mcp_folder_warning_probe.txt");
        std::fs::write(&file, b"x").unwrap();
        let w = folder_warning(&file.to_string_lossy()).expect("文件应给提示");
        assert!(w.contains("is a file"), "{w}");
        assert!(w.contains("read_file"), "{w}");
        let _ = std::fs::remove_file(&file);

        // 不存在的路径：说清「空结果是必然的」，并点出网络共享未索引这一可能。
        let missing = dir.join("everything_mcp_no_such_dir_probe");
        let w = folder_warning(&missing.to_string_lossy()).expect("缺失路径应给提示");
        assert!(w.contains("does not exist"), "{w}");
        assert!(w.contains("Indexes"), "{w}");
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
