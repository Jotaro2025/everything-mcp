//! state.rs
//!
//! 插件运行期状态。
//!
//! **db 引用与 db_query 对象是懒创建的** —— 不在 PM_START 时创建，而是在
//! 第一次搜索时（已经过主程序完整启动、主窗口就绪之后）于主线程上创建。
//! 这与 etp_server.c 的做法一致：它的 query 是在客户端连接时
//! (`etp_server_client_create`) 才创建的，而不是在 `etp_server_start` 里。
//!
//! 实测结论：在 PM_START 期间创建 query 并调用 `db_query_search2` 会导致
//! Everything.exe 在启动后约 1 秒崩溃（异常码 0xc0000005，模块 Everything.exe
//! 自身）。推迟到首次搜索时创建可避开该问题。
//!
//! 这些资源跨多个 MCP 请求共享，因此放在全局单例里，并与 `host::HOST_LOCK`
//! 一起保证主程序接口的串行访问。

use std::sync::OnceLock;

use super::ffi_types::*;
use super::host::Host;

/// 插件运行期状态。
///
/// `shutdown` 是 MCP HTTP 服务的关闭标志，由 PM_STOP 设置、监听线程读取。
pub struct PluginState {
    /// 关闭标志位：MCP 服务线程每次循环检查，true 时优雅退出。
    /// 由 PM_STOP 设置。
    pub shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// 进程级插件状态。
pub static STATE: OnceLock<PluginState> = OnceLock::new();

/// 懒创建的数据库引用 —— 首次搜索时在主线程上创建。
/// 以 usize 存储指针位模式：裸指针不是 Sync，不能直接放进 static。
pub static DB_HANDLE: OnceLock<usize> = OnceLock::new();

/// 懒创建的查询对象 —— 首次搜索时在主线程上创建，之后复用。
pub static QUERY_HANDLE: OnceLock<usize> = OnceLock::new();

/// 确保 db 引用与 query 对象已创建；返回 query 句柄。
///
/// **必须在主线程上调用**（db_query_create 的线程亲和性要求）。
/// 重复调用是幂等的 —— 已创建时直接返回现有句柄。
///
/// # Safety
/// 调用者必须持有 `host::HOST_LOCK` 且运行在 Everything 主线程上。
pub unsafe fn ensure_query() -> Result<DbQueryHandle, String> {
    if let Some(q) = QUERY_HANDLE.get() {
        return Ok(*q as DbQueryHandle);
    }

    let host = Host::get();

    let db = if let Some(d) = DB_HANDLE.get() {
        *d as DbHandle
    } else {
        let add_ref = host.db_add_local_ref.ok_or("db_add_local_ref null")?;
        let d = add_ref();
        if d.is_null() {
            return Err("db_add_local_ref returned NULL".to_string());
        }
        // 竞争无害：多线程同时 set 时只有一个成功，两个都拿到有效句柄。
        let _ = DB_HANDLE.set(d as usize);
        d
    };

    let create = host.db_query_create.ok_or("db_query_create null")?;
    let query = create(db, super::search::query_event_proc, core::ptr::null_mut());
    if query.is_null() {
        return Err("db_query_create returned NULL".to_string());
    }
    let _ = QUERY_HANDLE.set(query as usize);

    super::diag::write(&format!("ensure_query: db={:p} query={:p}", db, query));
    Ok(query)
}

/// 在 PM_START 时调用：只做自检，不创建 db/query（见模块文档）。
pub fn create() -> PluginState {
    let host = Host::get();
    if host.os_thread_create.is_none() {
        super::diag::write("WARNING: os_thread_create MISSING — search calls will fail");
    } else {
        super::diag::write("PM_START: os_thread_create available");
    }
    PluginState {
        shutdown: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

/// 在 PM_STOP/PM_KILL 时调用：销毁 query、释放 db 引用（若已懒创建）。
///
/// # Safety
/// 调用者必须保证此时没有其他线程正在调用主程序数据库接口。
pub unsafe fn destroy() {
    if let Some(s) = STATE.get() {
        s.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    let host = Host::get();
    if let Some(q) = QUERY_HANDLE.get() {
        if let Some(destroy) = host.db_query_destroy {
            destroy(*q as DbQueryHandle);
        }
    }
    if let Some(d) = DB_HANDLE.get() {
        if let Some(release) = host.db_release {
            release(*d as DbHandle);
        }
    }
}
