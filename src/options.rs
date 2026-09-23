//! options.rs — 插件设置页（Everything 选项对话框内）与设置状态机。
//!
//! 做法参考官方 http_server 插件（reference/http_server-1.0.5.6/src/http_server.c）：
//! Everything 通过 PM_ADD_OPTIONS_PAGES..PM_KILL_OPTIONS_PAGE 一组消息驱动
//! 插件设置页；控件由主程序提供的 `os_create_*` 工厂函数创建（保持主程序的
//! UI 风格 / DPI 缩放），勾选与数字框状态用标准 Win32 对话框函数读写
//! （http_server.c 同样直接用 IsDlgButtonChecked / GetDlgItemInt）。
//!
//! 设置项（键名与 Everything.ini 中一致，读取/写回对称）：
//!   - `mcp_enabled`       ：是否启用 MCP 服务（**默认 0 = 不启用**）
//!   - `mcp_bind`          ：监听地址（默认 127.0.0.1，仅本机）
//!   - `mcp_port`          ：监听端口（默认 8285）
//!   - `mcp_global_search` ：全局搜索档位 —— 0 拒绝（默认）/ 1 审核 / 2 允许，
//!     见 [`GlobalSearchMode`]。设置页用三个互斥复选框呈现（SDK 没有单选框
//!     工厂，点选即自动取消另外两个，行为等同单选组）。
//!
//! 生命周期（全部消息都在 Everything 主线程上到达）：
//!   PM_START              options::init —— 读设置并按需启动/保持关闭
//!   PM_ADD_OPTIONS_PAGES  options::add_page —— 注册设置页
//!   PM_LOAD_OPTIONS_PAGE  options::load_page —— 创建控件
//!   PM_GET_OPTIONS_PAGE_MINMAX / PM_SIZE_OPTIONS_PAGE —— 尺寸与布局
//!   PM_OPTIONS_PAGE_PROC  options::page_proc —— 处理 WM_COMMAND
//!   PM_SAVE_OPTIONS_PAGE  options::save_page —— 读控件并即时应用
//!   PM_SAVE_SETTINGS      options::save_settings —— 写回 Everything.ini
//!   PM_KILL_OPTIONS_PAGE  options::kill_page —— 无自有资源，空实现
//!
//! **线程约定**：以上消息全部来自主程序主线程。这里绝不能获取
//! `host::HOST_LOCK` —— MCP 工作线程持有该锁自旋等待主线程执行查询时，
//! 主线程若阻塞在 HOST_LOCK 上会造成死锁。

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Controls::{
    CheckDlgButton, IsDlgButtonChecked, BST_CHECKED, BST_UNCHECKED,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, GetDlgItemInt, SetDlgItemInt};

use crate::mcp;
use crate::mcp::protocol::GlobalSearchMode;
use crate::plugin::diag;
use crate::plugin::ffi_types::Utf8Buf;
use crate::plugin::host::Host;

// ============================================================
// 常量：默认值、设置键名、控件 ID、Win32/SDK 尺寸
// ============================================================

/// 默认监听端口。
pub const DEFAULT_PORT: u16 = 8285;

/// 默认监听地址 —— 仅本机，避免无意中把搜索能力暴露到局域网。
pub const DEFAULT_BIND: &str = "127.0.0.1";

/// Everything.ini 里的设置键名（读/写必须对称）。
const KEY_ENABLED: &[u8] = b"mcp_enabled\0";
const KEY_PORT: &[u8] = b"mcp_port\0";
const KEY_BIND: &[u8] = b"mcp_bind\0";
const KEY_GLOBAL_SEARCH: &[u8] = b"mcp_global_search\0";

/// 设置页控件 ID（页面内唯一即可；IDOK/IDCANCEL 在父对话框，不会冲突）。
const ID_ENABLED: i32 = 1;
const ID_BIND_STATIC: i32 = 2;
const ID_BIND_EDIT: i32 = 3;
const ID_PORT_STATIC: i32 = 4;
const ID_PORT_EDIT: i32 = 5;
const ID_RESTORE_DEFAULTS: i32 = 6;
const ID_GLOBAL_STATIC: i32 = 7;
const ID_GLOBAL_DENY: i32 = 8;
const ID_GLOBAL_REVIEW: i32 = 9;
const ID_GLOBAL_ALLOW: i32 = 10;

const WM_COMMAND: u32 = 0x0111;
const EN_CHANGE: u32 = 0x0300;
const WS_GROUP: u32 = 0x0002_0000;
const SS_LEFTNOWORDWRAP: u32 = 0x0C00;

/// everything_plugin.h 里的对话框控件高度/间距常量（逻辑像素）。
const DLG_STATIC_HIGH: i32 = 15;
const DLG_EDIT_HIGH: i32 = 21;
const DLG_CHECKBOX_HIGH: i32 = 15;
const DLG_BUTTON_HIGH: i32 = 23;
const DLG_SEPARATOR: i32 = 6;

/// 选项页最小尺寸（逻辑像素）—— PM_GET_OPTIONS_PAGE_MINMAX 报告给主程序。
const PAGE_MIN_WIDE: i32 = 200;
const PAGE_MIN_HIGH: i32 = 155;

/// Everything 选项对话框里 Apply 按钮的固定 ID（http_server.c 的
/// http_server_enable_options_apply 用的同一个值）。
const OPTIONS_APPLY_BUTTON_ID: i32 = 1001;

/// host 未提供文本宽度测量函数时，标签列的兜底宽度（逻辑像素）。
const FALLBACK_STATIC_WIDE: i32 = 90;

// ============================================================
// 选项页消息结构体 —— 与 everything_plugin.h 字节一致
// ============================================================

/// everything_plugin_load_options_page_t
#[repr(C)]
struct LoadOptionsPage {
    user_data: *mut c_void,
    page_hwnd: HWND,
    tooltip_hwnd: HWND,
}

/// everything_plugin_save_options_page_t
#[repr(C)]
struct SaveOptionsPage {
    user_data: *mut c_void,
    page_hwnd: HWND,
    /// 非 0 表示保持 Apply 启用（应用失败时由插件设置）。
    enable_apply: i32,
}

/// everything_plugin_get_options_page_minmax_t
#[repr(C)]
struct GetOptionsPageMinMax {
    user_data: *mut c_void,
    page_hwnd: HWND,
    wide: i32,
    high: i32,
}

/// everything_plugin_size_options_page_t
#[repr(C)]
struct SizeOptionsPage {
    user_data: *mut c_void,
    page_hwnd: HWND,
}

/// everything_plugin_options_page_proc_t
#[repr(C)]
struct OptionsPageProc {
    user_data: *mut c_void,
    options_hwnd: HWND,
    page_hwnd: HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
    result: isize,
    handled: i32,
}

// ============================================================
// 设置状态
// ============================================================

/// 设置与运行期状态。
struct OptionsState {
    /// 是否启用 MCP 服务。
    enabled: bool,
    /// 监听地址。
    bind: String,
    /// 监听端口。
    port: u16,
    /// 全局搜索档位（search_everywhere 的策略）。
    global_search: GlobalSearchMode,
    /// 当前实际在监听的 (bind, port)；None 表示未监听。
    /// 用于判断 Apply 时是否需要重启服务。
    running: Option<(String, u16)>,
}

static OPTS: OnceLock<Mutex<OptionsState>> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, OptionsState> {
    OPTS.get_or_init(|| Mutex::new(OptionsState::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl Default for OptionsState {
    fn default() -> Self {
        OptionsState {
            enabled: false,
            bind: DEFAULT_BIND.to_string(),
            port: DEFAULT_PORT,
            global_search: GlobalSearchMode::default(),
            running: None,
        }
    }
}

/// 当前全局搜索档位。MCP 工作线程在 tools/list（决定 search_everywhere 的
/// 描述前缀与注解）与 tools/call（Deny 档硬拒）时读取。
pub fn global_search_mode() -> GlobalSearchMode {
    state().global_search
}

// ============================================================
// 对外入口 —— 由 lib.rs 的 everything_plugin_proc 分发
// ============================================================

/// PM_START：载入读到的设置并应用（启用则启动监听，否则保持关闭）。
pub fn init(enabled: bool, bind: String, port: u16, global_search: GlobalSearchMode) {
    {
        let mut st = state();
        st.enabled = enabled;
        st.bind = bind;
        st.port = port;
        st.global_search = global_search;
    }
    apply();
}

/// PM_STOP / PM_KILL 之后服务已被关闭 —— 同步状态，避免下次 Apply 误判为
/// 「配置未变、无需重启」而实际并没有在监听。
pub fn mark_stopped() {
    state().running = None;
}

/// PM_ADD_OPTIONS_PAGES：在 Everything 选项对话框里注册设置页。
pub fn add_page(data: *mut c_void) -> *mut c_void {
    let host = Host::get();
    let add = match host.ui_options_add_plugin_page {
        Some(f) => f,
        None => {
            diag::write("options: ui_options_add_plugin_page unavailable — no settings page");
            return core::ptr::null_mut();
        }
    };
    let name = cstr_bytes(labels().page_name);
    unsafe { add(data, core::ptr::null_mut(), name.as_ptr()) };
    diag::write("options: settings page registered");
    1 as *mut c_void
}

/// PM_LOAD_OPTIONS_PAGE：按当前设置创建页面控件。
pub fn load_page(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let page = unsafe { &*(data as *const LoadOptionsPage) };
    let host = Host::get();

    // 控件工厂函数任一缺失就放弃建页。正常不会发生 —— 能注册页面
    // 说明主程序这一代选项页 API 完整。
    macro_rules! need {
        ($field:ident) => {
            match host.$field {
                Some(f) => f,
                None => {
                    diag::write(concat!(
                        "options: load_page: ",
                        stringify!($field),
                        " unavailable"
                    ));
                    return core::ptr::null_mut();
                }
            }
        };
    }
    let create_checkbox = need!(os_create_checkbox);
    let create_static = need!(os_create_static);
    let create_edit = need!(os_create_edit);
    let create_number_edit = need!(os_create_number_edit);
    let create_button = need!(os_create_button);
    let add_tooltip = need!(os_add_tooltip);

    let page_hwnd = page.page_hwnd;
    let tooltip_hwnd = page.tooltip_hwnd;
    let labels = labels();

    let (enabled, port, bind, global_search) = {
        let st = state();
        (
            st.enabled as i32,
            st.port as i64,
            cstr_bytes(&st.bind),
            st.global_search,
        )
    };

    unsafe {
        // 启用复选框
        create_checkbox(
            page_hwnd,
            ID_ENABLED,
            WS_GROUP,
            enabled,
            cstr_bytes(labels.enable).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_ENABLED,
            cstr_bytes(labels.enable_help).as_ptr(),
        );

        // 绑定地址
        create_static(
            page_hwnd,
            ID_BIND_STATIC,
            SS_LEFTNOWORDWRAP | WS_GROUP,
            cstr_bytes(labels.bind).as_ptr(),
        );
        create_edit(page_hwnd, ID_BIND_EDIT, WS_GROUP, bind.as_ptr());
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_BIND_EDIT,
            cstr_bytes(labels.bind_help).as_ptr(),
        );

        // 端口
        create_static(
            page_hwnd,
            ID_PORT_STATIC,
            SS_LEFTNOWORDWRAP | WS_GROUP,
            cstr_bytes(labels.port).as_ptr(),
        );
        create_number_edit(page_hwnd, ID_PORT_EDIT, WS_GROUP, port);
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_PORT_EDIT,
            cstr_bytes(labels.port_help).as_ptr(),
        );

        // 全局搜索档位：拒绝 / 审核 / 允许 三选一。
        // SDK 没有单选框工厂，用三个互斥复选框呈现 —— page_proc 里点选一个
        // 即自动取消另外两个，行为等同单选组（见 set_global_mode）。
        create_static(
            page_hwnd,
            ID_GLOBAL_STATIC,
            SS_LEFTNOWORDWRAP | WS_GROUP,
            cstr_bytes(labels.global_label).as_ptr(),
        );
        create_checkbox(
            page_hwnd,
            ID_GLOBAL_DENY,
            WS_GROUP,
            (global_search == GlobalSearchMode::Deny) as i32,
            cstr_bytes(labels.global_deny).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_GLOBAL_DENY,
            cstr_bytes(labels.global_deny_help).as_ptr(),
        );
        create_checkbox(
            page_hwnd,
            ID_GLOBAL_REVIEW,
            0,
            (global_search == GlobalSearchMode::Review) as i32,
            cstr_bytes(labels.global_review).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_GLOBAL_REVIEW,
            cstr_bytes(labels.global_review_help).as_ptr(),
        );
        create_checkbox(
            page_hwnd,
            ID_GLOBAL_ALLOW,
            0,
            (global_search == GlobalSearchMode::Allow) as i32,
            cstr_bytes(labels.global_allow).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_GLOBAL_ALLOW,
            cstr_bytes(labels.global_allow_help).as_ptr(),
        );

        // 恢复默认按钮
        create_button(
            page_hwnd,
            ID_RESTORE_DEFAULTS,
            WS_GROUP,
            cstr_bytes(labels.restore).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_RESTORE_DEFAULTS,
            cstr_bytes(labels.restore_help).as_ptr(),
        );

        // 未勾选启用时置灰依赖项（与 http_server_update_options_page 一致）。
        update_page(page_hwnd);
    }

    diag::write("options: page loaded");
    1 as *mut c_void
}

/// PM_SAVE_OPTIONS_PAGE：读控件 → 更新设置 → 即时应用（启用/停用/改地址端口
/// 都会立刻生效，无需重启 Everything）。应用失败时保持 Apply 启用。
pub fn save_page(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let page = unsafe { &mut *(data as *mut SaveOptionsPage) };
    let page_hwnd = page.page_hwnd;

    let enabled = unsafe { is_checked(page_hwnd, ID_ENABLED) };
    let global_search = unsafe { read_global_mode(page_hwnd) };
    let port = unsafe { GetDlgItemInt(page_hwnd, ID_PORT_EDIT, core::ptr::null_mut(), 0) };
    let mut bind = get_dlg_text(page_hwnd, ID_BIND_EDIT);
    if bind.trim().is_empty() {
        // 空地址按「仅本机」处理，避免误暴露到所有网卡。
        bind = DEFAULT_BIND.to_string();
    }

    let ok = if port == 0 || port > 65535 {
        diag::write(&format!("options: save_page: invalid port {}", port));
        Host::debug("everything_mcp: invalid port in settings");
        false
    } else {
        {
            let mut st = state();
            st.enabled = enabled;
            st.bind = bind;
            st.port = port as u16;
            st.global_search = global_search;
        }
        apply()
    };

    if !ok {
        // 应用失败（例如端口被占用）—— 保持 Apply 可点，改完再试。
        page.enable_apply = 1;
    }
    1 as *mut c_void
}

/// PM_GET_OPTIONS_PAGE_MINMAX：报告页面最小尺寸（逻辑像素）。
pub fn minmax(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let p = unsafe { &mut *(data as *mut GetOptionsPageMinMax) };
    p.wide = PAGE_MIN_WIDE;
    p.high = PAGE_MIN_HIGH;
    1 as *mut c_void
}

/// PM_SIZE_OPTIONS_PAGE：按页面客户区尺寸（换算成逻辑像素）摆放控件。
pub fn size_page(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let p = unsafe { &*(data as *const SizeOptionsPage) };
    let page_hwnd = p.page_hwnd;
    let (mut wide, mut high) = client_size_logical(page_hwnd);

    let x = 12;
    let mut y = 12;
    wide -= 24;
    high -= 24;

    let labels = labels();
    // 标签列宽 = 最宽标签 + 6 像素间隔。
    let mut static_wide = expand_min_wide(page_hwnd, labels.bind, 0);
    static_wide = expand_min_wide(page_hwnd, labels.port, static_wide);
    if static_wide == 0 {
        static_wide = FALLBACK_STATIC_WIDE;
    }
    static_wide += 6;

    set_rect(page_hwnd, ID_ENABLED, x, y, wide, DLG_CHECKBOX_HIGH);
    y += DLG_CHECKBOX_HIGH + DLG_SEPARATOR;

    set_rect(
        page_hwnd,
        ID_BIND_STATIC,
        x,
        y + 3,
        static_wide,
        DLG_STATIC_HIGH,
    );
    set_rect(
        page_hwnd,
        ID_BIND_EDIT,
        x + static_wide,
        y,
        wide - static_wide,
        DLG_EDIT_HIGH,
    );
    y += DLG_EDIT_HIGH + DLG_SEPARATOR;

    set_rect(
        page_hwnd,
        ID_PORT_STATIC,
        x,
        y + 3,
        static_wide,
        DLG_STATIC_HIGH,
    );
    set_rect(
        page_hwnd,
        ID_PORT_EDIT,
        x + static_wide,
        y,
        75,
        DLG_EDIT_HIGH,
    );
    y += DLG_EDIT_HIGH + DLG_SEPARATOR;

    // 全局搜索三选一：标签 + 三个互斥复选框横排。
    set_rect(
        page_hwnd,
        ID_GLOBAL_STATIC,
        x,
        y + 3,
        static_wide,
        DLG_STATIC_HIGH,
    );
    let mut cx = x + static_wide;
    for (id, text) in [
        (ID_GLOBAL_DENY, labels.global_deny),
        (ID_GLOBAL_REVIEW, labels.global_review),
        (ID_GLOBAL_ALLOW, labels.global_allow),
    ] {
        // 复选框自带方框与文字间距，宽度 = 文字宽 + 24。
        let wide = expand_min_wide(page_hwnd, text, 30) + 24;
        set_rect(page_hwnd, id, cx, y, wide, DLG_CHECKBOX_HIGH);
        cx += wide + 6;
    }

    // 恢复默认按钮贴在页面底部右角。
    let button_wide = expand_min_wide(page_hwnd, labels.restore, 75 - 24) + 24;
    set_rect(
        page_hwnd,
        ID_RESTORE_DEFAULTS,
        x + wide - button_wide,
        12 + high - DLG_BUTTON_HIGH,
        button_wide,
        DLG_BUTTON_HIGH,
    );

    1 as *mut c_void
}

/// PM_OPTIONS_PAGE_PROC：页面窗口消息（目前只关心 WM_COMMAND）。
pub fn page_proc(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let p = unsafe { &*(data as *const OptionsPageProc) };
    if p.msg != WM_COMMAND {
        // 其它消息不处理（http_server.c 同样直接放过）。
        return 1 as *mut c_void;
    }

    let page_hwnd = p.page_hwnd;
    let id = (p.wparam & 0xffff) as i32;
    let notify = ((p.wparam >> 16) & 0xffff) as u32;

    match id {
        ID_ENABLED => {
            unsafe { update_page(page_hwnd) };
            enable_apply(p.options_hwnd);
        }
        ID_RESTORE_DEFAULTS => {
            restore_defaults(page_hwnd);
            unsafe { update_page(page_hwnd) };
            enable_apply(p.options_hwnd);
        }
        ID_GLOBAL_DENY | ID_GLOBAL_REVIEW | ID_GLOBAL_ALLOW => {
            // 互斥复选框（等同单选组）：点谁谁唯一选中，再通知 Apply。
            unsafe { set_global_mode(page_hwnd, id) };
            enable_apply(p.options_hwnd);
        }
        ID_BIND_EDIT | ID_PORT_EDIT => {
            if notify == EN_CHANGE {
                enable_apply(p.options_hwnd);
            }
        }
        _ => {}
    }

    1 as *mut c_void
}

/// PM_KILL_OPTIONS_PAGE：控件由主程序创建和销毁，插件无自有资源。
pub fn kill_page(_data: *mut c_void) -> *mut c_void {
    1 as *mut c_void
}

/// PM_SAVE_SETTINGS：把当前设置写回主程序的设置输出流（最终落盘到
/// Everything.ini）。set_setting_* 缺失时跳过（设置页仍可即时应用）。
pub fn save_settings(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let host = Host::get();
    let st = state();

    match host.set_setting_int {
        Some(f) => unsafe {
            f(data, KEY_ENABLED.as_ptr(), st.enabled as i32);
            f(data, KEY_PORT.as_ptr(), st.port as i32);
            f(data, KEY_GLOBAL_SEARCH.as_ptr(), st.global_search.as_int());
        },
        None => diag::write("PM_SAVE_SETTINGS: plugin_set_setting_int unavailable"),
    }
    if let Some(f) = host.set_setting_string {
        let bind = cstr_bytes(&st.bind);
        unsafe { f(data, KEY_BIND.as_ptr(), bind.as_ptr()) };
    }

    diag::write(&format!(
        "PM_SAVE_SETTINGS: enabled={} bind={} port={} global_search={}",
        st.enabled as i32,
        st.bind,
        st.port,
        st.global_search.as_str()
    ));
    1 as *mut c_void
}

// ============================================================
// 应用设置（启动 / 停止 / 重启监听）
// ============================================================

/// 把当前设置应用到 MCP 服务。返回 false 表示应用失败（调用方应保持
/// 选项页的 Apply 启用）。必须在主线程调用（PM_START / PM_SAVE_OPTIONS_PAGE）。
fn apply() -> bool {
    let mut st = state();
    if st.enabled {
        let target = (st.bind.clone(), st.port);
        if st.running.as_ref() == Some(&target) {
            // 配置未变且已在监听 —— 不重启，避免打断已有连接。
            return true;
        }
        match mcp::server::start(&st.bind, st.port) {
            Ok(()) => {
                let msg = format!("options: MCP listening on {}:{}", st.bind, st.port);
                diag::write(&msg);
                Host::debug(&msg);
                st.running = Some(target);
                true
            }
            Err(e) => {
                let msg = format!("options: MCP start failed: {}", e);
                diag::write(&msg);
                Host::debug(&msg);
                st.running = None;
                false
            }
        }
    } else {
        if st.running.is_some() {
            mcp::server::stop();
            diag::write("options: MCP stopped (disabled)");
            Host::debug("everything_mcp: MCP stopped");
        }
        st.running = None;
        true
    }
}

// ============================================================
// 页面辅助
// ============================================================

/// 复选框是否处于勾选状态（BST_CHECKED）。
///
/// # Safety
/// `page_hwnd` 必须是有效的页面窗口句柄，且 `id` 对应的复选框已创建。
unsafe fn is_checked(page_hwnd: HWND, id: i32) -> bool {
    let state = unsafe { IsDlgButtonChecked(page_hwnd, id) };
    state == BST_CHECKED
}

/// 读三个互斥复选框 → 全局搜索档位。一个都没勾按最保守的 Deny 处理。
///
/// # Safety
/// `page_hwnd` 必须是有效的页面窗口句柄，且三个复选框已创建。
unsafe fn read_global_mode(page_hwnd: HWND) -> GlobalSearchMode {
    if unsafe { is_checked(page_hwnd, ID_GLOBAL_ALLOW) } {
        GlobalSearchMode::Allow
    } else if unsafe { is_checked(page_hwnd, ID_GLOBAL_REVIEW) } {
        GlobalSearchMode::Review
    } else {
        GlobalSearchMode::Deny
    }
}

/// 把三选一组设置成指定档位：只勾中 `id` 对应的项（等同单选行为）。
/// 复选框默认是点击即翻转，这里强制回写状态 —— 否则再点一次选中项
/// 会把它取消，出现「一个都没选」的中间态。
///
/// # Safety
/// `page_hwnd` 必须是有效的页面窗口句柄，且三个复选框已创建。
unsafe fn set_global_mode(page_hwnd: HWND, id: i32) {
    unsafe {
        CheckDlgButton(
            page_hwnd,
            ID_GLOBAL_DENY,
            if id == ID_GLOBAL_DENY {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            },
        );
        CheckDlgButton(
            page_hwnd,
            ID_GLOBAL_REVIEW,
            if id == ID_GLOBAL_REVIEW {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            },
        );
        CheckDlgButton(
            page_hwnd,
            ID_GLOBAL_ALLOW,
            if id == ID_GLOBAL_ALLOW {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            },
        );
    }
}

/// 按启用复选框置灰/取消置灰依赖控件。
///
/// # Safety
/// `page_hwnd` 必须是有效的页面窗口句柄，且控件已创建。
unsafe fn update_page(page_hwnd: HWND) {
    let enable = unsafe { is_checked(page_hwnd, ID_ENABLED) } as i32;
    if let Some(f) = Host::get().os_enable_or_disable_dlg_item {
        unsafe {
            f(page_hwnd, ID_BIND_STATIC, enable);
            f(page_hwnd, ID_BIND_EDIT, enable);
            f(page_hwnd, ID_PORT_STATIC, enable);
            f(page_hwnd, ID_PORT_EDIT, enable);
            f(page_hwnd, ID_GLOBAL_STATIC, enable);
            f(page_hwnd, ID_GLOBAL_DENY, enable);
            f(page_hwnd, ID_GLOBAL_REVIEW, enable);
            f(page_hwnd, ID_GLOBAL_ALLOW, enable);
        }
    }
}

/// 「恢复默认」按钮：不启用 + 127.0.0.1:8285 + 全局搜索拒绝。
fn restore_defaults(page_hwnd: HWND) {
    unsafe {
        CheckDlgButton(page_hwnd, ID_ENABLED, BST_UNCHECKED);
        SetDlgItemInt(page_hwnd, ID_PORT_EDIT, DEFAULT_PORT as u32, 0);
        set_global_mode(page_hwnd, ID_GLOBAL_DENY);
    }
    if let Some(f) = Host::get().os_set_dlg_text {
        let bind = cstr_bytes(DEFAULT_BIND);
        unsafe { f(page_hwnd, ID_BIND_EDIT, bind.as_ptr()) };
    }
}

/// 通知选项对话框「有未应用的更改」—— 重新启用 Apply 按钮。
fn enable_apply(options_hwnd: HWND) {
    if let Some(f) = Host::get().os_enable_or_disable_dlg_item {
        unsafe { f(options_hwnd, OPTIONS_APPLY_BUTTON_ID, 1) };
    }
}

/// 读编辑框文本（host 的 os_get_dlg_text + utf8_buf）。失败返回空串。
fn get_dlg_text(page_hwnd: HWND, id: i32) -> String {
    let host = Host::get();
    let get = match host.os_get_dlg_text {
        Some(f) => f,
        None => return String::new(),
    };
    let mut cbuf = Utf8Buf::default();
    unsafe {
        if let Some(init) = host.utf8_buf_init {
            init(&mut cbuf);
        }
        get(page_hwnd, id, &mut cbuf);
        let s = cbuf.to_string();
        if let Some(kill) = host.utf8_buf_kill {
            kill(&mut cbuf);
        }
        s
    }
}

/// 页面客户区尺寸（物理像素 → 逻辑像素，DPI 感知）。
fn client_size_logical(page_hwnd: HWND) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetClientRect(page_hwnd, &mut rect) };
    let host = Host::get();
    let logical_wide = host
        .os_get_logical_wide
        .map(|f| unsafe { f() })
        .unwrap_or(96)
        .max(1);
    let logical_high = host
        .os_get_logical_high
        .map(|f| unsafe { f() })
        .unwrap_or(96)
        .max(1);
    (
        ((rect.right - rect.left) * 96) / logical_wide,
        ((rect.bottom - rect.top) * 96) / logical_high,
    )
}

/// 测量文本在页面上占用的逻辑宽度，不小于当前值。
fn expand_min_wide(page_hwnd: HWND, text: &str, current_wide: i32) -> i32 {
    let host = Host::get();
    match host.os_expand_dialog_text_logical_wide_no_prefix {
        Some(f) => {
            let t = cstr_bytes(text);
            let wide = unsafe { f(page_hwnd, t.as_ptr(), current_wide) };
            if wide > current_wide {
                wide
            } else {
                current_wide
            }
        }
        None => current_wide,
    }
}

/// 设置控件矩形（host 封装，DPI 感知）。
fn set_rect(page_hwnd: HWND, id: i32, x: i32, y: i32, wide: i32, high: i32) {
    if let Some(f) = Host::get().os_set_dlg_rect {
        unsafe { f(page_hwnd, id, x, y, wide, high) };
    }
}

// ============================================================
// 界面文案（中/英）
// ============================================================

/// 设置页文案。SDK 只暴露主程序自己的本地化字符串，没有插件自定义文案的
/// 本地化机制，因此按主程序界面语言在中/英之间选择（见 labels()）。
struct Labels {
    page_name: &'static str,
    enable: &'static str,
    enable_help: &'static str,
    bind: &'static str,
    bind_help: &'static str,
    port: &'static str,
    port_help: &'static str,
    global_label: &'static str,
    global_deny: &'static str,
    global_deny_help: &'static str,
    global_review: &'static str,
    global_review_help: &'static str,
    global_allow: &'static str,
    global_allow_help: &'static str,
    restore: &'static str,
    restore_help: &'static str,
}

const LABELS_EN: Labels = Labels {
    page_name: "MCP",
    enable: "Enable MCP server",
    enable_help: "Expose Everything file search to LLM clients over the Model Context Protocol (MCP) HTTP endpoint.",
    bind: "Bind address:",
    bind_help: "IP address to listen on. 127.0.0.1 = this computer only (recommended). 0.0.0.0 = all network interfaces (allow other devices on your network).",
    port: "Port:",
    port_help: "TCP port for the MCP HTTP endpoint. MCP clients connect to http://<address>:<port>/.",
    global_label: "Global search:",
    global_deny: "Deny",
    global_deny_help: "Disable the search_everywhere tool. Calls return GLOBAL_SEARCH_DISABLED. All other tools stay folder-scoped. (Default)",
    global_review: "Review",
    global_review_help: "search_everywhere works, but is marked as needing user confirmation: the MCP client shows a permission prompt before calling it. Nothing is modified on disk either way — the call only reads the index.",
    global_allow: "Allow",
    global_allow_help: "search_everywhere works directly without a confirmation prompt: it searches all indexed locations by file name and returns full paths.",
    restore: "Restore Defaults",
    restore_help: "Reset to 127.0.0.1:8285 with the server disabled and global search set to Deny.",
};

const LABELS_ZH: Labels = Labels {
    page_name: "MCP",
    enable: "启用 MCP 服务",
    enable_help: "通过 MCP（模型上下文协议）HTTP 接口向 LLM 客户端开放 Everything 文件搜索。",
    bind: "绑定地址：",
    bind_help: "监听的 IP 地址。127.0.0.1 表示仅本机访问（推荐）；0.0.0.0 表示监听所有网卡（允许局域网内其他设备访问）。",
    port: "端口：",
    port_help: "MCP HTTP 服务监听的 TCP 端口。MCP 客户端通过 http://<地址>:<端口>/ 连接。",
    global_label: "全局搜索：",
    global_deny: "拒绝",
    global_deny_help: "禁用 search_everywhere 工具，调用一律返回 GLOBAL_SEARCH_DISABLED；其余工具仍限定在文件夹内搜索。（默认）",
    global_review: "审核",
    global_review_help: "search_everywhere 可用，但被标注为「需用户确认」：MCP 客户端在调用前先弹权限确认框。无论是否放行都不会改动磁盘 —— 调用只读索引。",
    global_allow: "允许",
    global_allow_help: "search_everywhere 直接可用，不再弹确认框：按文件名搜索全部已索引位置，返回完整路径。",
    restore: "恢复默认",
    restore_help: "恢复为 127.0.0.1:8285、不启用服务、全局搜索为「拒绝」。",
};

/// 按主程序界面语言取文案（探测一次后缓存）。
fn labels() -> &'static Labels {
    static CHINESE: OnceLock<bool> = OnceLock::new();
    if *CHINESE.get_or_init(probe_chinese_ui) {
        &LABELS_ZH
    } else {
        &LABELS_EN
    }
}

/// 探测主程序界面语言是否为中文：取主程序自己的本地化字符串
/// （101 = "Name"，4 = "OK"），含中文文案即认定中文界面。
fn probe_chinese_ui() -> bool {
    let host = Host::get();
    let get = match host.localization_get_string {
        Some(f) => f,
        None => return false,
    };
    unsafe {
        let name = read_cstr(get(101));
        if name.contains("名称") || name.contains("名稱") {
            return true;
        }
        let ok = read_cstr(get(4));
        ok.contains("确定") || ok.contains("確定")
    }
}

/// 读取 host 返回的 UTF-8 null 结尾字符串（内存归主程序，立即拷贝）。
///
/// # Safety
/// `p` 必须是 host 返回的有效指针或 NULL。
unsafe fn read_cstr(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 256 && unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(p, len) }).into_owned()
}

/// 把 Rust 字符串拷贝成末尾带 \0 的 UTF-8 字节串（host 接口要求 null 结尾）。
fn cstr_bytes(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}
