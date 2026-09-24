//! everything_mcp — Everything 1.5 MCP 服务插件
//!
//! 这一个 crate 编译为单个 DLL（everything_mcp.dll），
//! 被 Everything 1.5 在启动时通过 LoadLibrary 加载。
//!
//! 主程序只调用一个导出函数 [`everything_plugin_proc`]，
//! 我们根据 msg 分发到不同生命周期阶段：
//!   - PM_INIT：索取 host 函数指针
//!   - PM_START：安装主线程消息窗口、读设置并按需启动 MCP HTTP 服务
//!     （db 引用与 query 懒创建于第一次搜索，见 plugin::state）
//!   - PM_ADD_OPTIONS_PAGES..PM_KILL_OPTIONS_PAGE：Everything 选项对话框里
//!     的插件设置页（启用开关、绑定地址、端口、全局搜索档位），见 options 模块
//!   - PM_SAVE_SETTINGS：把设置写回 Plugins.ini（下次启动 PM_START 时
//!     经 plugin::ini_settings 兜底读回，原因见该模块文档）
//!   - PM_STOP / PM_KILL：关闭服务、释放引用
//!
//! 详见 `docs/PLUGIN_SDK_API_CN.md` 与 `README.md`。

#![allow(non_snake_case)]

// mcp 模块设为 pub：DLL 导出表由 everything_mcp.def 控制（仅导出
// everything_plugin_proc），pub 只为让 tests/ 下的集成测试能链接 rlib。
pub mod mcp;
mod options;
mod plugin;

use core::ffi::c_void;
use std::ffi::CStr;

use plugin::host::Host;

// ============================================================
// 插件元数据 —— 主程序通过 PM_GET_PLUGIN_VERSION / PM_GET_NAME /
// PM_GET_DESCRIPTION / PM_GET_AUTHOR / PM_GET_VERSION / PM_GET_LINK 读取。
// 全部是 UTF-8 静态字符串，末尾必须带 \0。
// ============================================================

/// 插件显示名 —— 出现在 Everything 插件管理列表里。
const PLUGIN_NAME: &[u8] = b"Everything MCP\0";

/// 插件版本号。唯一来源是 Cargo.toml —— 别在这里写死字符串，
/// 否则 bump 版本时容易漏掉这一处（MCP 的 serverInfo 也取自同一个来源）。
const PLUGIN_VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

/// 插件描述 —— 说明这个插件做什么。
const PLUGIN_DESCRIPTION: &[u8] =
    b"Expose Everything file search to LLMs over the Model Context Protocol (MCP).\0";

/// 插件作者。
const PLUGIN_AUTHOR: &[u8] = b"JOJO\0";

/// 插件主页链接 —— Everything 插件管理里点「链接」打开的地址。
const PLUGIN_LINK: &[u8] = b"https://github.com/Jotaro2025/everything-mcp\0";

/// Everything 1.5 唯一识别的插件入口符号。
///
/// 调用约定 `extern "system"`（Windows 上等价于 WINAPI / stdcall）。
/// 主程序通过 `LoadLibrary + GetProcAddress("everything_plugin_proc")` 调用它。
///
/// 返回值约定：
///   - PM_INIT：成功返回非 NULL（实际值不重要），失败返回 NULL；
///   - PM_GET_*：返回 UTF-8 静态字符串指针；
///   - PM_START / PM_STOP / PM_KILL：成功返回非 NULL；
///   - 其他消息：返回 NULL 表示未处理。
///
/// # Safety
/// 这是 C ABI 入口，由主程序跨语言调用。`data` 的类型随 `msg` 变化。
#[no_mangle]
pub unsafe extern "system" fn everything_plugin_proc(msg: u32, data: *mut c_void) -> *mut c_void {
    // 入口钩子：把每次调用都记录到磁盘日志，方便排查主程序实际传入了哪些 msg。
    // 注意：这是 PM_INIT 之前的调用，Host 表还没填充，不能用 Host::debug。
    plugin::diag::write(&format!("--> proc(msg={}, data={:p})", msg, data));
    let result = everything_plugin_proc_impl(msg, data);
    plugin::diag::write(&format!("<-- proc(msg={}) => {:p}", msg, result));
    result
}

unsafe fn everything_plugin_proc_impl(msg: u32, data: *mut c_void) -> *mut c_void {
    match msg {
        // ============================================================
        // PM_INIT：主程序传入 get_proc_address 回调，插件索取 host 函数。
        // ============================================================
        plugin::PM_INIT => {
            plugin::diag::write("=== everything_plugin_proc PM_INIT ===");
            if data.is_null() {
                plugin::diag::write("PM_INIT: data is NULL");
                return core::ptr::null_mut();
            }
            let get_proc_address: plugin::host::GetProcAddressFn =
                unsafe { core::mem::transmute(data) };
            match Host::install(get_proc_address) {
                Ok(()) => {
                    plugin::diag::write("PM_INIT: install OK");
                    Host::debug("everything_mcp: PM_INIT ok");
                    1 as *mut c_void
                }
                Err(missing) => {
                    let msg = format!("PM_INIT FAILED: missing host fn: {}", missing);
                    plugin::diag::write(&msg);
                    Host::debug("everything_mcp: PM_INIT failed (missing host fn)");
                    let mut msg_buf = String::from("everything_mcp: missing ");
                    msg_buf.push_str(missing);
                    Host::debug(&msg_buf);
                    core::ptr::null_mut()
                }
            }
        }

        // ============================================================
        // PM_START：读取设置、注册主线程窗口、按需启动 HTTP 服务。
        // db 引用与 query 对象懒创建（首次搜索时）—— 见 plugin::state 文档。
        // ============================================================
        plugin::PM_START => {
            plugin::diag::write("=== everything_plugin_proc PM_START ===");
            if data.is_null() {
                plugin::diag::write("PM_START: data is NULL");
                return core::ptr::null_mut();
            }
            // 此时 host 表已就绪。读取我们关心的配置项：
            //   - mcp_enabled：是否启用 MCP 服务（默认 0 —— 不启用，
            //     用户在 Everything 选项 → 插件 → MCP 设置页勾选启用）
            //   - mcp_port：HTTP 监听端口（默认 8285）
            //   - mcp_bind：监听地址（默认 127.0.0.1）
            //   - mcp_global_search：全局搜索档位（默认 0 拒绝 / 1 审核 / 2 允许）
            let mut enabled = read_setting_int(data, "mcp_enabled\0", 0) != 0;
            let mut port_raw = read_setting_int(data, "mcp_port\0", 8285);
            let mut bind = read_setting_string(data, "mcp_bind\0", "127.0.0.1\0");
            let mut global_raw = read_setting_int(data, "mcp_global_search\0", 0);

            // host 在 PM_START 读不到插件自己的设置（返回默认值，原因见
            // plugin::ini_settings 文档）。host 说未启用时，用自己解析的
            // Plugins.ini 兜底 —— 否则选项对话框启用后一重启就失效。
            if !enabled {
                if let Some(p) = plugin::ini_settings::read_persisted() {
                    enabled = p.enabled;
                    if let Some(port) = p.port {
                        port_raw = port as i32;
                    }
                    if let Some(b) = p.bind {
                        bind = b;
                    }
                    if let Some(g) = p.global_search {
                        global_raw = g;
                    }
                }
            }

            let port = if (1..=65535).contains(&port_raw) {
                port_raw as u16
            } else {
                options::DEFAULT_PORT
            };
            // 越界/负数一律归到默认档（拒绝）。
            let global_search =
                mcp::protocol::GlobalSearchMode::from_int(global_raw as i64).unwrap_or_default();
            plugin::diag::write(&format!(
                "PM_START: enabled={} port={} bind={} global_search={}",
                enabled,
                port,
                bind,
                global_search.as_str()
            ));

            Host::debug("everything_mcp: PM_START");

            // 在主线程上注册并创建 message-only 窗口。
            // 这是后续在主线程上调用 db_query_search2 的关键基础设施。
            if let Err(e) = plugin::main_thread::install_on_main_thread() {
                let msg = format!("PM_START: install main window failed: {}", e);
                plugin::diag::write(&msg);
                // 不返回错误 —— 即使没有主窗口，tools/list / ping 仍可用；
                // 仅 search_in_folder 会失败。
            }

            let state = plugin::state::create();
            plugin::diag::write("PM_START: state created (db/query are lazy)");

            // 把状态装到全局槽位。
            let _ = plugin::state::STATE.set(state);

            // 载入设置并应用：启用则启动监听，否则保持关闭。
            // 之后用户在设置页的改动也走同一条应用路径（options::apply）。
            options::init(enabled, bind, port, global_search);

            1 as *mut c_void
        }

        // ============================================================
        // PM_STOP：优雅关闭 —— 停 HTTP 服务、释放 db 引用、销毁主线程窗口。
        // state::destroy 与 destroy_main_window 都是幂等的 —— 主程序在
        // 关闭插件时常常先 PM_STOP 再 PM_KILL，不会造成双重释放。
        // ============================================================
        plugin::PM_STOP => {
            Host::debug("everything_mcp: PM_STOP");
            mcp::server::stop();
            options::mark_stopped();
            plugin::main_thread::destroy_main_window();
            unsafe { plugin::state::destroy() };
            1 as *mut c_void
        }

        plugin::PM_KILL => {
            Host::debug("everything_mcp: PM_KILL");
            mcp::server::stop();
            options::mark_stopped();
            plugin::main_thread::destroy_main_window();
            unsafe { plugin::state::destroy() };
            1 as *mut c_void
        }

        // ============================================================
        // 选项页消息组 —— Everything 选项对话框里的「MCP」设置页。
        // 控件由主程序的 os_create_* 工厂创建，消息处理见 options 模块。
        // ============================================================
        plugin::PM_ADD_OPTIONS_PAGES => options::add_page(data),
        plugin::PM_LOAD_OPTIONS_PAGE => options::load_page(data),
        plugin::PM_SAVE_OPTIONS_PAGE => options::save_page(data),
        plugin::PM_GET_OPTIONS_PAGE_MINMAX => options::minmax(data),
        plugin::PM_SIZE_OPTIONS_PAGE => options::size_page(data),
        plugin::PM_OPTIONS_PAGE_PROC => options::page_proc(data),
        plugin::PM_KILL_OPTIONS_PAGE => options::kill_page(data),

        // ============================================================
        // PM_SAVE_SETTINGS：主程序在关闭/保存设置时调用，期望插件把自己的
        // 配置项写回设置上下文（data）。设置来源可能是设置页、也可能是用户
        // 手改 Everything.ini —— 统一由 options 模块持有并在此持久化。
        // ============================================================
        plugin::PM_SAVE_SETTINGS => options::save_settings(data),

        // ============================================================
        // 元信息查询：返回静态 UTF-8 字符串指针。
        // ============================================================
        plugin::PM_GET_NAME => static_cstr_bytes(PLUGIN_NAME).as_ptr() as *mut c_void,
        plugin::PM_GET_DESCRIPTION => {
            static_cstr_bytes(PLUGIN_DESCRIPTION).as_ptr() as *mut c_void
        }
        plugin::PM_GET_AUTHOR => static_cstr_bytes(PLUGIN_AUTHOR).as_ptr() as *mut c_void,
        plugin::PM_GET_LINK => static_cstr_bytes(PLUGIN_LINK).as_ptr() as *mut c_void,
        plugin::PM_GET_VERSION => static_cstr_bytes(PLUGIN_VERSION).as_ptr() as *mut c_void,
        plugin::PM_GET_PLUGIN_VERSION => 1 as *mut c_void, // 协议版本

        // 未处理的消息统一返回 NULL，但记录到诊断日志方便排查。
        other => {
            plugin::diag::write(&format!("UNHANDLED msg={}", other));
            core::ptr::null_mut()
        }
    }
}

// ====================================================================
// 辅助：读取主程序设置（仅在 PM_START 期间有效）
// ====================================================================

/// 调用主程序 get_setting_int，读取一个整数设置项。
///
/// `name` 必须是末尾带 `\0` 的字面量（例如 `"mcp_enabled\0"`）。
///
/// # Safety
/// `data` 必须是 PM_START 时主程序传入的「设置上下文」指针。
unsafe fn read_setting_int(data: *mut c_void, name: &'static str, default: i32) -> i32 {
    let host = Host::get();
    match host.get_setting_int {
        Some(f) => unsafe { f(data, name.as_ptr(), default) },
        None => default,
    }
}

/// 调用主程序 get_setting_string，读取字符串设置。
/// 返回时把主程序返回的 UTF-8 指针拷成 String。
///
/// `name` 必须是末尾带 `\0` 的字面量；`default` 仅在 host 未导出该函数时使用。
///
/// # Safety
/// `data` 必须是 PM_START 时的设置上下文。
unsafe fn read_setting_string(
    data: *mut c_void,
    name: &'static str,
    default: &'static str,
) -> String {
    let host = Host::get();
    match host.get_setting_string {
        Some(f) => {
            // 主程序签名：返回 char*，所有权转移给插件，用完必须 mem_free
            // （http_server.c 正是在 PM_KILL 里逐项 mem_free 这些缓冲）。
            //
            // 第三个参数 current_string 传 NULL 而不是 default：主程序可能
            // 直接持有或 free 这个指针，而 default 是 .rdata 里的静态字面量，
            // 交给主程序去 free 会崩溃。host 在没有已存值时忽略该参数。
            let p = unsafe { f(data, name.as_ptr(), core::ptr::null_mut()) };
            if p.is_null() {
                return default.trim_end_matches('\0').to_string();
            }
            // host 返回的是 *mut u8（即 everything_plugin_utf8_t*），
            // 按 UTF-8 null 结尾读取，复制后立刻归还主程序分配器。
            let cs = unsafe { CStr::from_ptr(p as *const core::ffi::c_char) };
            let out = cs.to_string_lossy().into_owned();
            if let Some(free) = host.mem_free {
                unsafe { free(p as *mut c_void) };
            }
            out
        }
        None => default.trim_end_matches('\0').to_string(),
    }
}

/// 把字面量字节串构造成 CStr。`b` 必须以 `\0` 结尾且内部不含 nul。
fn static_cstr_bytes(b: &'static [u8]) -> &'static CStr {
    unsafe { CStr::from_bytes_with_nul_unchecked(b) }
}
