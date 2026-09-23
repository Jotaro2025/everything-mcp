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
}

/// 拼 Everything 搜索字符串。纯函数，单独单测。
///
/// Children：`parent:"<folder>" <pattern>` —— 只命中直接父文件夹。
/// Recursive：`"<folder>\" <pattern>` —— 引号内路径尾部带反斜杠：
///   - 反斜杠让匹配递归进子目录（Everything 对完整路径做子串匹配）；
///   - 同时避免误伤同前缀的兄弟目录 —— `everything-mcp` 不会命中
///     `everything-mcp-old` 里的文件；
///   - 文件夹自身的路径没有尾反斜杠，因此不会把文件夹本身搜出来。
fn build_search_string(folder: &str, pattern: &str, scope: SearchScope) -> String {
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
    }
    s.push_str(pattern);
    s
}

/// 一次搜索的结果：已按 max_results 截断的条目 + 未截断的命中总数。
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    /// 命中总条数（截断前的数量）。与 `results.len()` 相等即未被截断。
    pub total: usize,
}

/// 在指定文件夹下执行搜索。
///
/// `folder` 必须是绝对路径（如 `D:\source\repos\Everything-Plugin`）；
/// `pattern` 是 Everything 搜索语法（如 `*.rs`、`"readme"`、`ext:md;txt`）；
/// `scope` 决定搜直接子项（Children）还是递归整棵子树（Recursive）。
/// `max_results` 限制返回条数（0 表示无上限，但会读取全部结果可能耗时）。
/// `timeout_ms` 是异步等待查询完成的最大时间（建议 5000–30000）。
///
/// 返回值的 `total` 不受 `max_results` 截断影响 —— 调用方据此告诉 LLM
/// 「命中的比返回的多」。
pub fn search_in_folder(
    folder: &str,
    pattern: &str,
    max_results: usize,
    timeout_ms: u32,
    scope: SearchScope,
) -> Result<SearchOutcome, String> {
    let q = submit_query(folder, pattern, scope, timeout_ms)?;
    read_results(q.query, max_results)
}

/// 统计文件夹下匹配 `pattern` 的条目数，不读出任何名字。
///
/// 与 search_in_folder 同范围（递归子树），但只取
/// `db_query_get_result_count` 的计数值 —— 不为每条结果拼 name/path 字符串。
pub fn count_in_folder(folder: &str, pattern: &str, timeout_ms: u32) -> Result<usize, String> {
    let q = submit_query(folder, pattern, SearchScope::Recursive, timeout_ms)?;
    count_results(q.query)
}

/// 一次已提交且已完成的查询。
///
/// 三个字段必须一起活到结果（或计数）读完：
///   - `query`      ：结果读取的目标句柄；
///   - `_keepalive` ：搜索串保活守卫（后台查询线程整个查询期间都可能读它）；
///   - `_host_lock` ：主机互斥锁。read_results / count_results 还要 marshal
///                    到主线程并触碰同一个 query 对象，全程不能并发第二次查询。
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
///   - 按 scope 拼 Everything 搜索字符串限定文件夹（见 build_search_string）；
///   - 主程序异步执行查询，我们用 Win32 事件等待 QUERY_COMPLETE 回调；
///   - 完成后 query 即可读结果（读取仍需 marshal 到主线程，由调用方负责）。
fn submit_query(
    folder: &str,
    pattern: &str,
    scope: SearchScope,
    timeout_ms: u32,
) -> Result<CompletedQuery, String> {
    let host = Host::get();

    // 安全护栏：阻止搜索空目录或异常短路径导致的全部磁盘扫描。
    if folder.is_empty() {
        return Err("folder must not be empty".into());
    }

    // 1. 拼接 Everything 搜索字符串（限定范围 + 用户 pattern）。
    let search_string = build_search_string(folder, pattern, scope);

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
    error: Option<String>,
}

/// read_results 的主线程调用上下文。结果与命中总数写回本结构体字段。
struct ReadCtx {
    query: DbQueryHandle,
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
    let null_str: *const u8 = b"\0".as_ptr();

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

    // 排序属性：必须取内置 NAME 属性指针。etp_server.c 同样传这个而不是 NULL ——
    // 传 NULL 会让主程序在排序阶段解引用空指针崩溃。
    let sort_property = match Host::get().property_get_builtin_type {
        Some(f) => unsafe { f(PROPERTY_TYPE_NAME) },
        None => core::ptr::null(),
    };
    if sort_property.is_null() {
        super::diag::write("run_search_on_main: property_get_builtin_type(NAME) returned NULL");
    }

    super::diag::write_flush(&format!(
        "run_search_on_main: calling db_query_search2 query={:p} sort={:p}...",
        query, sort_property
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
    unsafe {
        (c.search_fn)(
            c.query,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0, // 10 个 match_* / ignore_* 开关
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
            1, // sort_property_type = NAME / ascending（不能是 NULL）
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
    match read_all(c.query, c.max_results) {
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
fn read_results(query: DbQueryHandle, max_results: usize) -> Result<SearchOutcome, String> {
    let ctx = Box::new(ReadCtx {
        query,
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

/// 从已完成的 query 对象中读出（最多 max_results 条）结果与命中总数。
/// 在主线程上运行；缓冲区生命周期自行管理。
unsafe fn read_all(
    query: DbQueryHandle,
    max_results: usize,
) -> Result<(Vec<SearchResult>, usize), String> {
    let host = Host::get();
    let total = unsafe { read_result_count(query)? };
    super::diag::write(&format!(
        "read_all: total={} take<=max={}",
        total, max_results
    ));
    let take = if max_results == 0 {
        total
    } else {
        total.min(max_results)
    };

    let mut out = Vec::with_capacity(take);
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
        for i in 0..take {
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

            // db_query_get_result_path 只返回父路径（SDK 语义：不含文件名），
            // 完整路径要自己拼；盘符根（C:\）结尾时不再补分隔符。
            let mut path = parent;
            if !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(&name);

            out.push(SearchResult {
                name,
                path,
                is_folder,
                size,
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
    fn inner_quotes_are_stripped() {
        assert_eq!(
            build_search_string("D:\\pr\"oj", "x", SearchScope::Recursive),
            r#""D:\proj\" x"#
        );
    }
}
