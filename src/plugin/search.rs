//! search.rs
//!
//! 对主程序数据库查询的高层封装 —— 把异步 event_proc 模型包成同步等待。
//!
//! 核心思路（与 etp_server 的 RETR 阶段一致）：
//!   1. 创建一个 os_event；
//!   2. 把它的句柄存到 thread-local 槽位，作为 event_proc 的隐式 user_data；
//!   3. 调用 db_query_search —— 异步立即返回；
//!   4. WaitForSingleObject(event, timeout) —— 主线程完成查询后调用 event_proc，
//!      event_proc 在 QUERY_COMPLETE 时 SetEvent；
//!   5. 调用者从结果接口读出已就绪的数据。
//!
//! **线程模型**：
//!   - event_proc 由 Everything 主线程调用（即 db_query_search 触发的工作线程回调）；
//!   - MCP HTTP 工作线程调用 wait_for_results 等待事件；
//!   - 二者通过 os_event 同步，无需额外加锁。
//!
//! **互斥**：调用 db_query_search 之前必须持有 HOST_LOCK，
//! 因为 query 对象是全局唯一的 —— 不能并发提交两次查询。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// windows-sys 0.59 中：
//   - HANDLE 是 *mut c_void
//   - WAIT_OBJECT_0 / WAIT_TIMEOUT / CloseHandle 在 Win32::Foundation
//   - CreateEventW / SetEvent / WaitForSingleObject 在 Win32::System::Threading
use core::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

/// Win32 HANDLE —— 在 windows-sys 0.59 里是 `*mut c_void`。
/// 通过 `usize` 存入 AtomicUsize；转换时保持位模式不变。
type Handle = *mut c_void;

use super::ffi_types::*;
use super::host::Host;

/// 主程序查询事件的 Rust 入口 —— 由主程序在查询完成时回调。
///
/// **注意**：这个 event_proc 会收到*所有*查询的完成事件，包括主程序 UI 自己
/// 发起的查询（它们共用同一套事件回调路径）。因此必须用 `QUERY_SUBMITTED`
/// 守卫过滤 —— 只有我们自己提交过查询、正在等待结果时才允许 SetEvent，
/// 否则会被无关事件提前唤醒、读到空结果（etp_server.c 用 `c->is_query`
/// 做同样的过滤）。
///
/// # Safety
/// `user_data` 目前未使用；我们通过 `EVENT_SLOT` 全局槽位传递事件句柄。
pub unsafe extern "system" fn query_event_proc(_user_data: *mut core::ffi::c_void, evtype: i32) {
    if evtype == db_event::QUERY_COMPLETE || evtype == db_event::SORT_COMPLETE {
        // 只有「我们已提交查询且正在等待」时才响应。
        if !QUERY_SUBMITTED.load(Ordering::SeqCst) {
            return;
        }
        // 通知等待方：查询完成。
        let h = EventSlot::load();
        if !h.is_null() {
            unsafe { SetEvent(h) };
        }
    }
}

/// 「当前有一次由本插件提交、尚未读完结果的查询」标志。
///
/// 在提交 db_query_search2 前置位、读完结果（或放弃）后清零。
/// event_proc 只在该标志为 true 时触发等待方。
static QUERY_SUBMITTED: AtomicBool = AtomicBool::new(false);

/// 全局事件句柄槽位。
///
/// 约定：在持有 HOST_LOCK 的情况下设置 → 触发 search → 等待 → 清空。
struct EventSlot;
static EVENT_SLOT: AtomicUsize = AtomicUsize::new(0);

impl EventSlot {
    fn store(handle: Handle) {
        EVENT_SLOT.store(handle as usize, Ordering::SeqCst);
    }
    fn clear() {
        EVENT_SLOT.store(0, Ordering::SeqCst);
    }
    fn load() -> Handle {
        EVENT_SLOT.load(Ordering::SeqCst) as Handle
    }
}

/// 一条搜索结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub name: String,
    pub path: String,
    pub is_folder: bool,
    pub size: u64,
    /// 修改时间，ISO 8601 UTC（`2026-09-23T11:18:31Z`）。索引项没有该时间时为 None。
    pub modified: Option<String>,
    /// 创建时间，同上。
    pub created: Option<String>,
}

/// 搜索范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// 只搜直接子项 —— Everything 的 `parent:"<folder>"` 语义。
    /// list_folder 用这个。
    Children,
    /// 递归搜整棵子树 —— 路径前缀 `"<folder>\"` 语义。
    /// search_in_folder / count 用这个。
    Recursive,
    /// 整个索引（不加任何路径前缀）。search_everywhere 用这个 ——
    /// 受 `mcp_global_search` 三档开关约束，见 tools.rs 的模式闸门。
    Global,
}

/// 结果排序键。映射到 `property_get_builtin_type` 的内置属性类型 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
}

impl SortKey {
    /// 该排序键对应的内置属性类型 ID（见 ffi_types 的 PROPERTY_TYPE_*）。
    pub fn property_type(self) -> i32 {
        match self {
            SortKey::Name => PROPERTY_TYPE_NAME,
            SortKey::Path => PROPERTY_TYPE_PATH,
            SortKey::Size => PROPERTY_TYPE_SIZE,
            SortKey::DateModified => PROPERTY_TYPE_DATE_MODIFIED,
            SortKey::DateCreated => PROPERTY_TYPE_DATE_CREATED,
        }
    }

    /// 规范名（回显给调用方用）。
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Path => "path",
            SortKey::Size => "size",
            SortKey::DateModified => "modified",
            SortKey::DateCreated => "created",
        }
    }

    /// 解析排序键。大小写不敏感，并收几个 LLM 常写的同义字。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "name" | "filename" => Some(SortKey::Name),
            "path" => Some(SortKey::Path),
            "size" | "filesize" => Some(SortKey::Size),
            "modified" | "date_modified" | "datemodified" | "mtime" | "date" => {
                Some(SortKey::DateModified)
            }
            "created" | "date_created" | "datecreated" | "ctime" => Some(SortKey::DateCreated),
            _ => None,
        }
    }
}

/// 一次搜索的完整选项（范围 + 匹配开关 + 排序）。
///
/// 打包成一个结构体，避免 `search_in_folder` 的形参长到失控，也便于
/// `count` 这类只需要范围的调用方用 [`SearchOptions::for_scope`] 取默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub scope: SearchScope,
    /// 区分大小写（`db_query_search2` 的 match_case）。
    pub match_case: bool,
    /// 全字匹配（match_whole_word）。
    pub match_whole_word: bool,
    pub sort: SortKey,
    /// 是否降序。默认升序（按名字 A→Z）。
    pub descending: bool,
}

impl SearchOptions {
    /// 只指定范围的默认选项：不区分大小写、按名字升序。
    pub fn for_scope(scope: SearchScope) -> Self {
        Self {
            scope,
            match_case: false,
            match_whole_word: false,
            sort: SortKey::Name,
            descending: false,
        }
    }
}


/// 拼 Everything 搜索字符串。纯函数，单独单测。
///
/// Children：`parent:"<folder>" <pattern>` —— 只命中直接父文件夹。
/// Recursive：`"<folder>\" <pattern>` —— 引号内路径尾部带反斜杠：
///   - 反斜杠让匹配递归进子目录（Everything 对完整路径做子串匹配）；
///   - 同时避免误伤同前缀的兄弟目录 —— `everything-mcp` 不会命中
///     `everything-mcp-old` 里的文件；
///   - 文件夹自身的路径没有尾反斜杠，因此不会把文件夹本身搜出来。
///
/// Global：`<pattern>` 原样 —— 不加任何路径前缀，搜整个索引。
fn build_search_string(folder: &str, pattern: &str, scope: SearchScope) -> String {
    if scope == SearchScope::Global {
        return pattern.to_string();
    }
    // Everything 路径里几乎不会有双引号，但安全起见去掉，避免破坏引号包裹。
    let folder = folder.replace('"', "");
    let mut s = String::new();
    match scope {
        SearchScope::Children => {
            s.push_str("parent:\"");
            s.push_str(&folder);
            s.push_str("\" ");
        }
        SearchScope::Recursive => {
            let mut f = folder;
            if !f.ends_with('\\') {
                f.push('\\');
            }
            s.push('"');
            s.push_str(&f);
            s.push_str("\" ");
        }
        SearchScope::Global => unreachable!("handled above"),
    }
    s.push_str(pattern);
    s
}

/// 单次响应里结果条目的 JSON 字节预算（512 KiB）。
///
/// 为什么需要它、而不只是 `max_results` 上限：条目字节数随路径长度变化，
/// 275 字节/条只是 `C:\Windows\System32` 那种短路径的实测值；深目录加长文件名
/// 下同样 2000 条可以大出好几倍。**条数上限管的是「结果还有没有用」，
/// 字节预算管的才是「调用方能不能把这份 JSON 读下来」**，两者不能互相替代。
///
/// 为什么是 512 KiB 而不是更小：本服务是本地回环，实测 548 KiB 的响应 53 ms
/// 就传完了 —— 带宽不是约束，唯一的约束是模型的上下文。卡在半兆这个量级，
/// 既容得下约 1900 条常规条目，又不会大到模型读不动。
///
/// 命中预算时**至少保留一条**：否则调用方拿到空结果、`offset` 推不动，
/// 会卡在死循环里。单条路径最长约 32 KiB，远小于预算，这个保底恒可满足。
pub const MAX_RESULT_WINDOW_BYTES: usize = 512 * 1024;

/// 一条结果在响应 JSON 里的固定开销（不含 name / path 本身）。
///
/// 覆盖 kind（"folder"/"file"）、size（最多 20 位）、两个各 20 字符的 ISO 时间戳，
/// 外加键名、引号、逗号与缩进。实测这部分约 173 字节（见 `estimate_entry_bytes`），
/// 取 200 留出余量。
const ENTRY_JSON_OVERHEAD: usize = 200;

/// 一条结果在响应 JSON 里占用的字节数估算 —— **必须是上界**。
///
/// **反斜杠按两倍算**：JSON 里 `\` 要转义成 `\\`，而 Windows 路径几乎全是
/// 反斜杠。只按 `name + path + 常量` 估会低估约 20%，预算就被击穿：
/// 2026-09-24 实测 System32 的 2000 条 `*.dll`，按 `name + path + 160` 估出
/// 218 字节/条，实际 274 字节/条，于是 548 KB 的响应越过 512 KiB 预算却没有触发。
/// name 也一并翻倍，省得为文件名里的引号、控制字符开特例。
///
/// 纯函数，单独单测（`read_all` 需要宿主，CI 里跑不了）。
///
/// 对 `list_folder` 偏保守：它的响应只带 `name`，不带 `path`，但这里照样把
/// 完整路径算进去（实测 System32：1764 条占 345 KB，没填满 512 KiB 预算）。
/// 方向是安全的一侧 —— 预算只承诺「不超过」，不承诺「填满」——所以不值得为
/// 它把「响应是否含 path」这个形状信息从 tools 层穿透到读取层。
fn estimate_entry_bytes(name_len: usize, path_len: usize) -> usize {
    ENTRY_JSON_OVERHEAD + 2 * (name_len + path_len)
}

/// 一次搜索的结果：已按 `offset`/`max_results` 截断的条目 + 未截断的命中总数。
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    /// 命中总条数（截断前的数量）。`results.len()` 小于它即说明还有更多。
    pub total: usize,
    /// 本次结果在完整命中列表中的起始下标（回显给调用方，便于翻页）。
    pub offset: usize,
}

/// 在指定文件夹下执行搜索。
///
/// `folder` 必须是绝对路径（如 `D:\source\repos\Everything-Plugin`）；
/// `pattern` 是 Everything 搜索语法（如 `*.rs`、`"readme"`、`ext:md;txt`）。
/// `offset` / `max_results` 一起圈定返回窗口。`max_results == 0` 在本层仍表示
/// 「读到末尾」，但 tools 层已不再产生 0（那里按 1..=2000 校验），保留该语义
/// 只是防御。真正兜底的是 [`MAX_RESULT_WINDOW_BYTES`]：无论条数上限多大，
/// 响应体都不会超过字节预算。
/// `timeout_ms` 是异步等待查询完成的最大时间（建议 5000–30000）。
/// `options` 决定搜索范围、匹配开关与排序。
///
/// 返回值的 `total` 不受窗口截断影响 —— 调用方据此告诉 LLM
/// 「命中的比返回的多」以及下一批的 `offset` 该取多少。
pub fn search_in_folder(
    folder: &str,
    pattern: &str,
    offset: usize,
    max_results: usize,
    timeout_ms: u32,
    options: SearchOptions,
) -> Result<SearchOutcome, String> {
    let q = submit_query(folder, pattern, options, timeout_ms)?;
    read_results(q.query, offset, max_results)
}

/// 统计文件夹下匹配 `pattern` 的条目数，不读出任何名字。
///
/// 与 search_in_folder 同范围（递归子树），但只取
/// `db_query_get_result_count` 的计数值 —— 不为每条结果拼 name/path 字符串。
pub fn count_in_folder(folder: &str, pattern: &str, timeout_ms: u32) -> Result<usize, String> {
    let q = submit_query(
        folder,
        pattern,
        SearchOptions::for_scope(SearchScope::Recursive),
        timeout_ms,
    )?;
    count_results(q.query)
}

/// 在整个 Everything 索引上按 pattern 搜索（不限文件夹）。
///
/// 与 search_in_folder 的差别只有范围：搜索串不带任何路径前缀，因此会命中
/// 所有已索引位置（本地磁盘 + 网络共享）。**调用前必须过 tools.rs 的
/// `mcp_global_search` 模式闸门** —— 这一层只管搜，不管权限。
/// `options.scope` 在这里被强制为 [`SearchScope::Global`]。
pub fn search_everywhere(
    pattern: &str,
    offset: usize,
    max_results: usize,
    timeout_ms: u32,
    options: SearchOptions,
) -> Result<SearchOutcome, String> {
    let options = SearchOptions {
        scope: SearchScope::Global,
        ..options
    };
    let q = submit_query("", pattern, options, timeout_ms)?;
    read_results(q.query, offset, max_results)
}

/// 一次已提交且已完成的查询。
///
/// 三个字段必须一起活到结果（或计数）读完：
///   - `query`      ：结果读取的目标句柄；
///   - `_keepalive` ：搜索串保活守卫（后台查询线程整个查询期间都可能读它）；
///   - `_host_lock` ：主机互斥锁。read_results / count_results 还要 marshal
///     到主线程并触碰同一个 query 对象，全程不能并发第二次查询。
///
/// 字段声明顺序即 Drop 顺序：先释放缓冲区，最后才解锁。
struct CompletedQuery {
    query: DbQueryHandle,
    _keepalive: LeakedBuf,
    _host_lock: std::sync::MutexGuard<'static, ()>,
}

/// 提交一次查询并阻塞等待其完成。成功时返回 [`CompletedQuery`]（调用方持有
/// 它直到读完结果/计数）；超时会取消查询并返回错误。
///
/// 实现细节：
///   - 按 options.scope 拼 Everything 搜索字符串限定文件夹（见 build_search_string）；
///   - 主程序异步执行查询，我们用 Win32 事件等待 QUERY_COMPLETE 回调；
///   - 完成后 query 即可读结果（读取仍需 marshal 到主线程，由调用方负责）。
fn submit_query(
    folder: &str,
    pattern: &str,
    options: SearchOptions,
    timeout_ms: u32,
) -> Result<CompletedQuery, String> {
    let host = Host::get();

    // 安全护栏：阻止搜索空目录或异常短路径导致的全部磁盘扫描。
    // Global 范围本来就没有 folder（空串是唯一合法值），不受此限。
    if folder.is_empty() && options.scope != SearchScope::Global {
        return Err("folder must not be empty".into());
    }

    // 1. 拼接 Everything 搜索字符串（限定范围 + 用户 pattern）。
    let search_string = build_search_string(folder, pattern, options.scope);

    // 2. 加锁、创建事件、设置槽位、提交查询、等待。
    let guard = Host::lock_host();

    // 把 UTF-8 字符串末尾补 null，供主程序读取。
    let mut search_bytes = search_string.into_bytes();
    search_bytes.push(0);

    // 创建一个自动重置事件 —— 主程序 SetEvent 后状态自动变回 non-signaled。
    // windows-sys 0.59 的 CreateEventW 第一个参数是 *const SECURITY_ATTRIBUTES，
    // 因此需要 Win32_Security feature；这里传 null 表示默认属性。
    let event = unsafe {
        CreateEventW(
            core::ptr::null(), // lpEventAttributes
            0,                 // bManualReset = FALSE（自动重置）
            0,                 // bInitialState = FALSE
            core::ptr::null(), // lpName
        )
    };
    if event.is_null() {
        return Err("CreateEventW failed".into());
    }

    // 提交搜索。
    // 参数表严格按 etp_server.c 中 everything_plugin_db_query_search2 的调用形式：
    // 大量参数传 0/NULL，仅保留 force/allow_*/clear_* 这些必要开关。
    let search = host.db_query_search.ok_or("db_query_search2 null")?;
    super::diag::write(&format!(
        "submit_query: search_bytes_len={} search={:?}",
        search_bytes.len(),
        std::str::from_utf8(&search_bytes).unwrap_or("<utf8err>")
    ));

    // 把事件放进槽位，以便 event_proc 找到它。
    // 放在所有可能提前返回的检查之后，避免槽位残留失效句柄。
    EventSlot::store(event);

    // 关键：db_query_search2 必须从 Everything 主线程上发起，否则会崩溃
    // （主程序的 TLS / 线程状态只在主线程上有效）。
    // 通过 main_thread::invoke_c 把这次调用 marshal 到主线程的 wndproc 里。
    // db_query_search2 本身是异步的：提交后立即返回，真正的查询在主程序后台线程
    // 上跑，完成时通过 event_proc 触发我们这里 SetEvent 的事件。
    //
    // query 对象的懒创建也放在同一个主线程任务里完成（db_query_create 同样有
    // 主线程亲和性），句柄通过 ctx.query 写回给本函数后续使用。
    // SearchCtx 必须活到主线程完成调用为止 —— invoke_c 阻塞返回后才释放。
    //
    // search_bytes 必须保活到结果读出之后：异步查询在后台线程执行期间
    // 仍可能读取 search_string。因此移到堆上由 LeakedBuf 在 CompletedQuery
    // 析构时回收，不能像栈上 Vec 那样在提交后就释放。
    let leaked = LeakedBuf(Box::leak(search_bytes.into_boxed_slice()));
    let ctx = Box::new(SearchCtx {
        search_fn: search,
        query: core::ptr::null_mut(),
        search_bytes: leaked.0.as_ptr(),
        options,
        error: None,
    });
    // 提交前置位守卫：只有这次提交触发的完成事件才允许唤醒等待方，
    // 挡住主程序 UI 自己的查询完成事件。
    // 注意：db_query_search2 是异步的 —— 提交返回后查询仍在后台跑，
    // 因此守卫必须保持到等待结束，不能在提交后就撤（etp_server.c 的
    // c->is_query 同样是在 event_proc 收到完成事件后才清零）。
    QUERY_SUBMITTED.store(true, Ordering::SeqCst);
    let raw_ctx = Box::into_raw(ctx) as *mut core::ffi::c_void;
    let r = super::main_thread::invoke_c(run_search_on_main, raw_ctx);
    // 无论成败都回收 ctx —— run_search_on_main 不持有它超过调用期。
    let mut ctx = unsafe { Box::from_raw(raw_ctx as *mut SearchCtx) };
    let query = ctx.query;
    let setup_err = match r {
        Ok(()) => ctx.error.take(),
        Err(e) => Some(format!("db_query_search2 invoke failed: {}", e)),
    };
    drop(ctx);
    if let Some(e) = setup_err {
        // 查询未提交成功 —— 此时撤守卫是安全的。
        QUERY_SUBMITTED.store(false, Ordering::SeqCst);
        EventSlot::clear();
        unsafe { CloseHandle(event) };
        return Err(e);
    }

    // WaitForSingleObject 第一个参数是 HANDLE (isize)。
    let mut waited = 0u32;
    let step = 200u32;
    let mut signaled = false;
    while waited < timeout_ms {
        let r = unsafe { WaitForSingleObject(event, step) };
        if r == WAIT_OBJECT_0 {
            signaled = true;
            break;
        }
        if r == WAIT_TIMEOUT {
            waited += step;
            continue;
        }
        // 其他错误返回（WAIT_FAILED 等）：立即跳出
        break;
    }

    // 等待结束（无论是否等到）即撤守卫 —— 之后再来完成事件都与本次无关。
    QUERY_SUBMITTED.store(false, Ordering::SeqCst);
    super::diag::write(&format!(
        "submit_query: wait done signaled={} waited={}ms",
        signaled, waited
    ));
    EventSlot::clear();
    unsafe { CloseHandle(event) };

    if !signaled {
        // 取消查询以免后续调用受影响 —— 主程序文档建议在放弃时调用。
        // db_query_cancel 与 search2/结果读取一样有主线程亲和性（query
        // 对象归主线程所有），因此同样 marshal 过去，不能在本线程直接调。
        // 此刻仍持有 HOST_LOCK —— 主线程 wndproc 不取该锁，不会死锁。
        cancel_query(query);
        return Err(format!("search timeout after {} ms", timeout_ms));
    }

    // 查询已完成 —— 句柄、保活守卫、主机锁一并交还给调用方，
    // 直到它读完结果（或计数）才释放。
    Ok(CompletedQuery {
        query,
        _keepalive: leaked,
        _host_lock: guard,
    })
}

/// 堆上搜索字符串的保活守卫。
///
/// db_query_search2 是异步的 —— 后台查询线程在整个查询期间都可能读取
/// search_string 指针，因此提交方不能在调用返回后就释放它。这个守卫把
/// 缓冲区保活到 CompletedQuery 析构（那时查询已完成、结果已复制，
/// 或已取消），由 Drop 统一回收。
struct LeakedBuf(&'static mut [u8]);

impl Drop for LeakedBuf {
    fn drop(&mut self) {
        unsafe { drop(Box::from_raw(self.0.as_mut_ptr())) };
    }
}

/// db_query_search2 的主线程调用上下文。
///
/// search_bytes 指向的数据由调用方（search_in_folder 栈帧上的 Vec）保活，
/// 直到 invoke_c 返回。
/// query 由 run_search_on_main 通过 ensure_query 懒创建后写回；
/// error 在 query 创建失败时写入。
struct SearchCtx {
    search_fn: super::host::DbQuerySearchFn,
    query: DbQueryHandle,
    search_bytes: *const u8,
    /// 匹配开关与排序键 —— 由 run_search_on_main 透传给 db_query_search2。
    options: SearchOptions,
    error: Option<String>,
}

/// read_results 的主线程调用上下文。结果与命中总数写回本结构体字段。
struct ReadCtx {
    query: DbQueryHandle,
    offset: usize,
    max_results: usize,
    result: Vec<SearchResult>,
    total: usize,
    error: Option<String>,
}

/// count_results 的主线程调用上下文。
struct CountCtx {
    query: DbQueryHandle,
    total: usize,
    error: Option<String>,
}

/// 在主线程 wndproc 里执行 db_query_search2。
///
/// # Safety
/// `ctx` 必须指向一个有效的、由调用方保活的 `SearchCtx`。
unsafe extern "system" fn run_search_on_main(ctx: *mut core::ffi::c_void) {
    if ctx.is_null() {
        super::diag::write("run_search_on_main: NULL ctx");
        return;
    }
    let c = &mut *(ctx as *mut SearchCtx);
    let null_str: *const u8 = c"".to_bytes().as_ptr();

    // 懒创建 db 引用与 query 对象（首次搜索时）。
    // db_query_create 与 db_query_search2 一样有主线程亲和性，因此放在这里。
    let query = match super::state::ensure_query() {
        Ok(q) => q,
        Err(e) => {
            super::diag::write(&format!("run_search_on_main: ensure_query failed: {}", e));
            c.error = Some(e);
            return;
        }
    };
    c.query = query;

    let opts = c.options;
    // 排序属性：必须取内置属性指针（NAME/SIZE/DATE_MODIFIED…）。etp_server.c 同样
    // 传有效指针而不是 NULL —— 传 NULL 会让主程序在排序阶段解引用空指针崩溃。
    let sort_type = opts.sort.property_type();
    let sort_property = match Host::get().property_get_builtin_type {
        Some(f) => unsafe { f(sort_type) },
        None => core::ptr::null(),
    };
    if sort_property.is_null() {
        super::diag::write(&format!(
            "run_search_on_main: property_get_builtin_type({}) returned NULL",
            sort_type
        ));
    }
    // db_query_search2 的 sort_ascending：1 = 升序，0 = 降序。
    let sort_ascending = if opts.descending { 0 } else { 1 };

    super::diag::write_flush(&format!(
        "run_search_on_main: calling db_query_search2 query={:p} sort={:p} asc={} case={} ww={}...",
        query, sort_property, sort_ascending, opts.match_case, opts.match_whole_word
    ));
    // 搜索函数权限位。映射来自官方 http_server 插件的注释
    // （reference/http_server-1.0.5.6/src/http_server.c:408-410）：
    //   allow_query_access → is-open: / online: / runcount: / is-running:
    //   allow_read_access  → content:（正文检索）
    //   allow_disk_access  → include-filelist:
    // 全部置 1。etp_server 传 0 是面向远程客户端的保守选择；本服务默认只监听
    // 127.0.0.1（options.rs 的 DEFAULT_BIND），且 SDK 文档
    // （docs/PLUGIN_SDK_API_CN.md:582）明确建议 MCP 场景放开这些权限位。
    // 置 0 的后果：content: 一类函数直接返回 0 条 —— 评测里「content: 搜索
    // 不可用」正是这三个 0 造成的，与 Everything 自身能力无关。
    //
    // 前 10 个 match_* / ignore_* 开关：只放开用户显式要的 case / whole word。
    // 其余保持 0。特别注意 match_regex 必须传 0 —— 实测它会把这整条搜索串
    // （含我们前置的文件夹路径）当成一个正则编译，路径里的 `\s` 之类让结果
    // 恒为 0。正则改由搜索串里的 `regex:` 搜索函数表达（见 tools.rs）。
    unsafe {
        (c.search_fn)(
            c.query,
            opts.match_case as i32,
            opts.match_whole_word as i32,
            0, // match_path
            0, // match_diacritics
            0, // match_prefix
            0, // match_suffix
            0, // ignore_punctuation
            0, // ignore_whitespace
            0, // match_regex —— 见上方注释，永远 0
            0, // hide_empty_search_results
            1, // clear_selection
            1, // clear_item_refs
            c.search_bytes,
            0, // filter_flags
            null_str,
            null_str, // filter / filter_columns
            core::ptr::null(),
            0,
            -1,
            1, // filter_sort / asc / view / fast_sort_only
            sort_property,
            sort_ascending, // sort_property_type / ascending（不能是 NULL）
            core::ptr::null(),
            0, // sort 2 —— etp 同样传 NULL
            core::ptr::null(),
            0, // sort 3 —— etp 同样传 NULL
            0,
            0,
            0,
            0,
            0, // folders_first / dialog_center_x / y / track_size / track_folder
            0,
            1,
            1,
            1, // force / allow_query / allow_read / allow_disk
            0,
            SIZE_STANDARD_JEDEC,
            0,
            1,
            0,
        );
    }
    super::diag::write_flush("run_search_on_main: db_query_search2 returned");
}

/// 在主线程 wndproc 里读取查询结果，写回 ctx 字段。
///
/// # Safety
/// `ctx` 必须指向一个有效的、由调用方保活的 `ReadCtx`。
unsafe extern "system" fn run_read_on_main(ctx: *mut core::ffi::c_void) {
    if ctx.is_null() {
        return;
    }
    let c = &mut *(ctx as *mut ReadCtx);
    match read_all(c.query, c.offset, c.max_results) {
        Ok((v, total)) => {
            c.result = v;
            c.total = total;
        }
        Err(e) => c.error = Some(e),
    }
}

/// 在主线程 wndproc 里只读查询命中总数，写回 ctx 字段。
///
/// # Safety
/// `ctx` 必须指向一个有效的、由调用方保活的 `CountCtx`。
unsafe extern "system" fn run_count_on_main(ctx: *mut core::ffi::c_void) {
    if ctx.is_null() {
        return;
    }
    let c = &mut *(ctx as *mut CountCtx);
    match read_result_count(c.query) {
        Ok(n) => c.total = n,
        Err(e) => c.error = Some(e),
    }
}

/// 取消查询的主线程调用上下文。
struct CancelCtx {
    cancel: super::host::DbCancelQueryFn,
    query: DbQueryHandle,
}

/// 在主线程 wndproc 里取消一次查询。
///
/// # Safety
/// `ctx` 必须指向一个有效的、由调用方保活的 `CancelCtx`。
unsafe extern "system" fn run_cancel_on_main(ctx: *mut core::ffi::c_void) {
    if ctx.is_null() {
        return;
    }
    let c = &*(ctx as *const CancelCtx);
    unsafe { (c.cancel)(c.query) };
}

/// 放弃一次查询时调用：把 db_query_cancel marshal 到主线程执行。
///
/// 主程序没有导出该函数时静默返回（查询本体最终也会超时收场）。
/// marshal 失败也只记诊断 —— 取消是尽力而为，不该把原始错误盖掉。
fn cancel_query(query: DbQueryHandle) {
    let host = Host::get();
    let Some(cancel) = host.db_query_cancel else {
        super::diag::write("cancel_query: db_query_cancel not available");
        return;
    };
    let ctx = Box::new(CancelCtx { cancel, query });
    let raw = Box::into_raw(ctx) as *mut core::ffi::c_void;
    let r = super::main_thread::invoke_c(run_cancel_on_main, raw);
    // invoke_c 返回即代表主线程已执行完（或确认未执行），ctx 可以回收。
    drop(unsafe { Box::from_raw(raw as *mut CancelCtx) });
    if let Err(e) = r {
        super::diag::write(&format!("cancel_query: invoke failed: {}", e));
    }
}

/// 从已完成的 query 对象中读出结果列表与命中总数。
///
/// db_query_get_result_* 有主线程亲和性（共享同一个 query 对象的主线程
/// 所有权语义），因此 marshal 到主线程执行。
fn read_results(
    query: DbQueryHandle,
    offset: usize,
    max_results: usize,
) -> Result<SearchOutcome, String> {
    let ctx = Box::new(ReadCtx {
        query,
        offset,
        max_results,
        result: Vec::new(),
        total: 0,
        error: None,
    });
    let raw_ctx = Box::into_raw(ctx) as *mut core::ffi::c_void;
    let r = super::main_thread::invoke_c(run_read_on_main, raw_ctx);
    // 取回 ctx（主线程已把结果与总数写进它的字段）。
    let mut ctx = unsafe { Box::from_raw(raw_ctx as *mut ReadCtx) };
    r.map_err(|e| format!("read_results invoke failed: {}", e))?;
    match ctx.error.take() {
        Some(e) => Err(e),
        None => Ok(SearchOutcome {
            results: core::mem::take(&mut ctx.result),
            total: ctx.total,
            offset: ctx.offset,
        }),
    }
}

/// 只读查询命中总数（不碰任何条目的 name/path）。同样 marshal 到主线程。
fn count_results(query: DbQueryHandle) -> Result<usize, String> {
    let ctx = Box::new(CountCtx {
        query,
        total: 0,
        error: None,
    });
    let raw_ctx = Box::into_raw(ctx) as *mut core::ffi::c_void;
    let r = super::main_thread::invoke_c(run_count_on_main, raw_ctx);
    let mut ctx = unsafe { Box::from_raw(raw_ctx as *mut CountCtx) };
    r.map_err(|e| format!("count_results invoke failed: {}", e))?;
    match ctx.error.take() {
        Some(e) => Err(e),
        None => Ok(ctx.total),
    }
}

/// 读查询命中的总条数（不受 max_results 截断影响）。主线程亲和。
///
/// # Safety
/// 调用者必须持有 `host::HOST_LOCK` 且运行在 Everything 主线程上。
unsafe fn read_result_count(query: DbQueryHandle) -> Result<usize, String> {
    let host = Host::get();
    let total = unsafe {
        (host
            .db_query_get_result_count
            .ok_or("get_result_count null")?)(query)
    };
    Ok(total)
}

/// 由总数、`offset`、`max_results` 算出实际要读的下标窗口 `(start, take)`。
///
/// `max_results == 0` 表示读到末尾（tools 层已不产生 0，见 `search_in_folder`
/// 文档）；`offset` 超出总数时返回空窗口（take=0）。
/// 纯函数，单独单测。
///
/// 注意这里算出的 `take` 只是**上界**：`read_all` 还会按
/// [`MAX_RESULT_WINDOW_BYTES`] 提前收尾，所以实际返回条数可能更少。
fn page_window(total: usize, offset: usize, max_results: usize) -> (usize, usize) {
    let start = offset.min(total);
    let remaining = total - start;
    let take = if max_results == 0 {
        remaining
    } else {
        remaining.min(max_results)
    };
    (start, take)
}

/// 字节预算判定：`used` 是已累计的条目字节，`entry` 是这一条的估算字节。
/// 返回 true 表示预算还容得下这一条。
///
/// `used == 0` 时恒 true —— **至少收一条**。否则调用方会拿到空结果、`offset`
/// 推不动，卡在死循环里。单条路径最长约 32 KiB，远小于预算，这个保底恒可满足。
/// 纯函数，单独单测（`read_all` 需要宿主，CI 里跑不了）。
fn fits_budget(used: usize, entry: usize) -> bool {
    used == 0 || used + entry <= MAX_RESULT_WINDOW_BYTES
}

/// 从已完成的 query 对象中读出窗口 `[offset, offset+max_results)` 的结果与命中总数。
/// 在主线程上运行；缓冲区生命周期自行管理。
unsafe fn read_all(
    query: DbQueryHandle,
    offset: usize,
    max_results: usize,
) -> Result<(Vec<SearchResult>, usize), String> {
    let host = Host::get();
    let total = unsafe { read_result_count(query)? };
    let (start, take) = page_window(total, offset, max_results);
    super::diag::write(&format!(
        "read_all: total={} window=[{}, {})",
        total,
        start,
        start + take
    ));

    let mut out = Vec::with_capacity(take);
    let mut used_bytes = 0usize;
    let mut name_buf = Utf8Buf::default();
    let mut path_buf = Utf8Buf::default();
    let mut fd = FileInfoFd::default();

    unsafe {
        (host.utf8_buf_init.ok_or("utf8_buf_init null")?)(&mut name_buf);
        (host.utf8_buf_init.ok_or("utf8_buf_init null")?)(&mut path_buf);
    }

    // 读出结果。包在闭包里是为了无论中途哪个 host 调用缺失、以 `?` 提前
    // 返回，下面的 utf8_buf_kill 都一定执行 —— 否则泄漏主程序分配的缓冲区。
    let read = || -> Result<(), String> {
        for i in start..start + take {
            let name = unsafe {
                (host
                    .db_query_get_result_name
                    .ok_or("get_result_name null")?)(query, i, &mut name_buf);
                name_buf.to_string()
            };
            let parent = unsafe {
                (host
                    .db_query_get_result_path
                    .ok_or("get_result_path null")?)(query, i, &mut path_buf);
                path_buf.to_string()
            };
            let is_folder = unsafe {
                (host
                    .db_query_is_folder_result
                    .ok_or("is_folder_result null")?)(query, i)
                    != 0
            };
            let size = unsafe {
                (host
                    .db_query_get_result_indexed_fd
                    .ok_or("get_indexed_fd null")?)(query, i, &mut fd);
                if is_folder {
                    0
                } else {
                    fd.file_size()
                }
            };
            // fd 里同时带时间戳（同一结构体，读出即免费）。转成 ISO 便于 LLM 读。
            let modified = super::timefmt::filetime_to_iso(fd.date_modified);
            let created = super::timefmt::filetime_to_iso(fd.date_created);

            // db_query_get_result_path 只返回父路径（SDK 语义：不含文件名），
            // 完整路径要自己拼；盘符根（C:\）结尾时不再补分隔符。
            let mut path = parent;
            if !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(&name);

            // 字节预算：条数上限之外的第二道闸。命中即停，至少留一条（见 fits_budget）。
            let entry_bytes = estimate_entry_bytes(name.len(), path.len());
            if !fits_budget(used_bytes, entry_bytes) {
                super::diag::write(&format!(
                    "read_all: byte budget hit at {} of {} window entries ({} bytes)",
                    out.len(),
                    take,
                    used_bytes
                ));
                break;
            }
            used_bytes += entry_bytes;

            out.push(SearchResult {
                name,
                path,
                is_folder,
                size,
                modified,
                created,
            });
        }
        Ok(())
    }();

    // 清理 UTF-8 缓冲区。init/kill 在 PM_INIT 时都是强制依赖项，必然存在。
    if let Some(kill) = host.utf8_buf_kill {
        unsafe {
            kill(&mut name_buf);
            kill(&mut path_buf);
        }
    }

    read?;
    Ok((out, total))
}

impl Host {
    /// 所有 host 函数调用前需要持有的全局互斥锁的便捷封装。
    pub fn lock_host() -> std::sync::MutexGuard<'static, ()> {
        super::host::HOST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_scope_uses_parent_syntax() {
        assert_eq!(
            build_search_string(r"D:\proj", "ext:rs", SearchScope::Children),
            r#"parent:"D:\proj" ext:rs"#
        );
    }

    #[test]
    fn recursive_scope_appends_trailing_backslash() {
        assert_eq!(
            build_search_string(r"D:\proj", "ext:rs", SearchScope::Recursive),
            r#""D:\proj\" ext:rs"#
        );
    }

    #[test]
    fn recursive_scope_keeps_single_trailing_backslash() {
        assert_eq!(
            build_search_string(r"D:\proj\", "", SearchScope::Recursive),
            r#""D:\proj\" "#
        );
    }

    #[test]
    fn recursive_scope_drive_root_stays_root() {
        assert_eq!(
            build_search_string(r"D:\", "ext:rs", SearchScope::Recursive),
            r#""D:\" ext:rs"#
        );
    }

    #[test]
    fn global_scope_has_no_folder_prefix() {
        // Global 原样传 pattern —— 不加路径前缀，folder 参数直接忽略。
        assert_eq!(
            build_search_string("", "*.vhd", SearchScope::Global),
            "*.vhd"
        );
        assert_eq!(
            build_search_string(r"C:\ignored", r"ext:pdf !\old\", SearchScope::Global),
            r"ext:pdf !\old\"
        );
    }

    #[test]
    fn inner_quotes_are_stripped() {
        assert_eq!(
            build_search_string("D:\\pr\"oj", "x", SearchScope::Recursive),
            r#""D:\proj\" x"#
        );
    }

    #[test]
    fn page_window_clamps_offset_and_limit() {
        // 常规翻页。
        assert_eq!(page_window(100, 0, 50), (0, 50));
        assert_eq!(page_window(100, 50, 50), (50, 50));
        assert_eq!(page_window(100, 90, 50), (90, 10)); // 最后一批不足一页
        // max_results == 0 表示读到末尾。
        assert_eq!(page_window(100, 40, 0), (40, 60));
        assert_eq!(page_window(100, 0, 0), (0, 100));
        // offset 越界 → 空窗口，不 panic。
        assert_eq!(page_window(100, 100, 50), (100, 0));
        assert_eq!(page_window(100, 999, 50), (100, 0));
        // 空结果集。
        assert_eq!(page_window(0, 0, 50), (0, 0));
    }

    #[test]
    fn byte_budget_always_admits_the_first_entry() {
        // 保底：无论这一条多大，预算为空时必须收下 —— 否则调用方拿到空结果、
        // offset 推不动，会卡死。单条路径最长约 32 KiB，实际到不了预算。
        assert!(fits_budget(0, MAX_RESULT_WINDOW_BYTES));
        assert!(fits_budget(0, MAX_RESULT_WINDOW_BYTES * 10));
        assert!(fits_budget(0, usize::MAX));
    }

    #[test]
    fn byte_budget_capacity_is_the_same_ballpark_as_the_entry_cap() {
        // 边界是闭区间：正好用满预算收下，超一个字节就停。
        assert!(fits_budget(MAX_RESULT_WINDOW_BYTES - 100, 100));
        assert!(!fits_budget(MAX_RESULT_WINDOW_BYTES - 100, 101));
        assert!(!fits_budget(MAX_RESULT_WINDOW_BYTES, 1));

        // 按实测条目尺寸算，512 KiB 能装约 1600 条 —— 与 RESULT_WINDOW_MAX
        // （2000 条）同一档。两道闸管的是不同东西（字节 vs 条数），但正常路径
        // 长度下不该差一个数量级，否则其中一道就成了摆设。
        let per_entry = estimate_entry_bytes(16, 43);
        let capacity = MAX_RESULT_WINDOW_BYTES / per_entry;
        assert!(fits_budget((capacity - 1) * per_entry, per_entry));
        assert!(!fits_budget(capacity * per_entry, per_entry));
        assert!((1000..2500).contains(&capacity), "capacity={capacity}");
    }

    #[test]
    fn entry_estimate_is_an_upper_bound_on_the_measured_entry() {
        // 2026-09-24 实测：System32 的 2000 条 *.dll，平均 name 16 字符、
        // path 43 字符，序列化后 274 字节/条。估算必须 ≥ 实测，否则预算被击穿
        // —— 这正是上一版（`name + path + 160` = 218）犯的错：548 KB 的响应
        // 越过了 512 KiB 预算却没有触发。
        let (name, path, measured) = (16usize, 43usize, 274usize);
        let est = estimate_entry_bytes(name, path);
        assert!(est >= measured, "est={est} measured={measured}");
        // 反斜杠按两倍算是这条公式的关键，别被「优化」掉。
        assert!(est > name + path + ENTRY_JSON_OVERHEAD, "est={est}");
        // 长路径随长度线性增长：深目录不会因为条数少就撑爆预算。
        assert!(estimate_entry_bytes(16, 200) > estimate_entry_bytes(16, 43));
        const { assert!(MAX_RESULT_WINDOW_BYTES == 512 * 1024) };
    }

    #[test]
    fn sort_key_maps_to_builtin_property_ids() {
        assert_eq!(SortKey::Name.property_type(), PROPERTY_TYPE_NAME);
        assert_eq!(SortKey::Path.property_type(), PROPERTY_TYPE_PATH);
        assert_eq!(SortKey::Size.property_type(), PROPERTY_TYPE_SIZE);
        assert_eq!(
            SortKey::DateModified.property_type(),
            PROPERTY_TYPE_DATE_MODIFIED
        );
        assert_eq!(
            SortKey::DateCreated.property_type(),
            PROPERTY_TYPE_DATE_CREATED
        );
    }

    #[test]
    fn sort_key_parses_names_and_synonyms() {
        assert_eq!(SortKey::parse(""), Some(SortKey::Name));
        assert_eq!(SortKey::parse("name"), Some(SortKey::Name));
        assert_eq!(SortKey::parse("SIZE"), Some(SortKey::Size));
        assert_eq!(SortKey::parse(" modified "), Some(SortKey::DateModified));
        assert_eq!(SortKey::parse("date"), Some(SortKey::DateModified));
        assert_eq!(SortKey::parse("created"), Some(SortKey::DateCreated));
        assert_eq!(SortKey::parse("path"), Some(SortKey::Path));
        assert_eq!(SortKey::parse("bogus"), None);
    }

    #[test]
    fn default_options_are_name_ascending_without_modifiers() {
        let o = SearchOptions::for_scope(SearchScope::Recursive);
        assert_eq!(o.scope, SearchScope::Recursive);
        assert_eq!(o.sort, SortKey::Name);
        assert!(!o.descending);
        assert!(!o.match_case && !o.match_whole_word);
    }
}
