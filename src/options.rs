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

use crate::abi_ok;
use crate::mcp;
use crate::mcp::protocol::GlobalSearchMode;
use crate::plugin::diag;
use crate::plugin::ffi_types::Utf8Buf;
use crate::plugin::host::Host;
use crate::stats_page;

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
const ID_CONTENT_INDEXING: i32 = 11;
const ID_CONTENT_NOTE: i32 = 12;
const ID_CONTENT_FORMATS: i32 = 13;

pub(crate) const WM_COMMAND: u32 = 0x0111;
const EN_CHANGE: u32 = 0x0300;
pub(crate) const WS_GROUP: u32 = 0x0002_0000;
// 与 winuser.h 一致（历史上误抄成 0x0C00，低 4 位才是样式位：
// 0x0C00 低 4 位为 0 = SS_LEFT，「不换行」从未生效过）。
pub(crate) const SS_LEFTNOWORDWRAP: u32 = 0x000C;
/// 允许自动换行的静态文本（SS_LEFT，样式位为 0）。
pub(crate) const SS_LEFT: u32 = 0x0000;
pub(crate) const SS_RIGHT: u32 = 0x0002;

/// everything_plugin.h 里的对话框控件高度/间距常量（逻辑像素）。
pub(crate) const DLG_STATIC_HIGH: i32 = 15;
const DLG_EDIT_HIGH: i32 = 21;
const DLG_CHECKBOX_HIGH: i32 = 15;
pub(crate) const DLG_BUTTON_HIGH: i32 = 23;
const DLG_SEPARATOR: i32 = 6;

/// 选项页最小尺寸（逻辑像素）—— PM_GET_OPTIONS_PAGE_MINMAX 报告给主程序。
const PAGE_MIN_WIDE: i32 = 200;
// 底部多出「内容索引」复选框 + 说明行 + 两行格式列表，最小高度相应加 65。
const PAGE_MIN_HIGH: i32 = 220;

/// Everything 选项对话框里 Apply 按钮的固定 ID（http_server.c 的
/// http_server_enable_options_apply 用的同一个值）。
const OPTIONS_APPLY_BUTTON_ID: i32 = 1001;

// ============================================================
// 双页：MCP 设置页 + Statistics 只读页（后者在 stats_page.rs）
// ============================================================

/// host 未提供文本宽度测量函数时，标签列的兜底宽度（逻辑像素）。
const FALLBACK_STATIC_WIDE: i32 = 90;

// ============================================================
// 选项页消息结构体 —— 与 everything_plugin.h 字节一致
// ============================================================

/// everything_plugin_load_options_page_t
#[repr(C)]
pub(crate) struct LoadOptionsPage {
    pub(crate) user_data: *mut c_void,
    pub(crate) page_hwnd: HWND,
    pub(crate) tooltip_hwnd: HWND,
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
pub(crate) struct OptionsPageProc {
    pub(crate) user_data: *mut c_void,
    pub(crate) options_hwnd: HWND,
    pub(crate) page_hwnd: HWND,
    pub(crate) msg: u32,
    pub(crate) wparam: usize,
    pub(crate) lparam: isize,
    pub(crate) result: isize,
    pub(crate) handled: i32,
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

/// 当前生效的设置快照 `(enabled, bind, port)` —— server_diagnostics 工具用。
/// 这是「配置值」，不代表服务真的在监听（那要看 server::bound_port）。
pub fn settings_snapshot() -> (bool, String, u16) {
    let st = state();
    (st.enabled, st.bind.clone(), st.port)
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

/// PM_ADD_OPTIONS_PAGES：在 Everything 选项对话框里注册两页 ——
/// MCP 设置页（`user_data=null`）+ Statistics 只读页（`user_data=STATS_PAGE_MARKER`）。
/// 多页注册 SDK 未验证过，这里直接做，实机验证；不行退到「MCP 页底部加统计区块」。
pub fn add_page(data: *mut c_void) -> *mut c_void {
    let host = Host::get();
    let add = match host.ui_options_add_plugin_page {
        Some(f) => f,
        None => {
            diag::write("options: ui_options_add_plugin_page unavailable — no settings page");
            return core::ptr::null_mut();
        }
    };
    let name_mcp = cstr_bytes(labels().page_name);
    unsafe { add(data, core::ptr::null_mut(), name_mcp.as_ptr()) };
    diag::write("options: MCP settings page registered");

    let name_stats = cstr_bytes(labels().stats_page_name);
    unsafe { add(data, stats_page::page_marker(), name_stats.as_ptr()) };
    diag::write("options: Statistics page registered");
    abi_ok()
}

/// PM_LOAD_OPTIONS_PAGE：按当前设置创建页面控件。
pub fn load_page(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let page = unsafe { &*(data as *const LoadOptionsPage) };
    if stats_page::is_stats_page(page.user_data) {
        return stats_page::load_stats_page(page);
    }
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
    // 内容索引是 Everything 的核心设置（不在 Plugins.ini），从 Everything.ini
    // 读当前值；读不到按未勾选显示（与官方默认一致）。
    let content_indexing = read_content_indexing().unwrap_or(false) as i32;

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

        // 内容索引（Everything 高级设置 content_indexing_enabled 的直达开关）。
        create_checkbox(
            page_hwnd,
            ID_CONTENT_INDEXING,
            0,
            content_indexing,
            cstr_bytes(labels.content_indexing).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_CONTENT_INDEXING,
            cstr_bytes(labels.content_indexing_help).as_ptr(),
        );
        create_static(
            page_hwnd,
            ID_CONTENT_NOTE,
            SS_LEFTNOWORDWRAP,
            cstr_bytes(labels.content_indexing_note).as_ptr(),
        );
        // 只读展示 Everything 实际索引了哪些文件类型 —— 让「content: 为什么
        // 搜不到我的代码」这个问题在页面上就能自答（要用 grep）。
        // 用 SS_LEFT 而非 NOWORDWRAP：格式列表放一行必然被右边裁掉。
        let formats = content_formats_text(labels);
        create_static(
            page_hwnd,
            ID_CONTENT_FORMATS,
            SS_LEFT,
            cstr_bytes(&formats).as_ptr(),
        );
        add_tooltip(
            tooltip_hwnd,
            page_hwnd,
            ID_CONTENT_FORMATS,
            cstr_bytes(labels.content_formats_help).as_ptr(),
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
    abi_ok()
}

/// PM_SAVE_OPTIONS_PAGE：读控件 → 更新设置 → 即时应用（启用/停用/改地址端口
/// 都会立刻生效，无需重启 Everything）。应用失败时保持 Apply 启用。
pub fn save_page(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let page = unsafe { &mut *(data as *mut SaveOptionsPage) };
    if stats_page::is_stats_page(page.user_data) {
        // Statistics 页只读，无任何可保存内容 —— 空转返回，不碰 Apply。
        return abi_ok();
    }
    let page_hwnd = page.page_hwnd;

    let enabled = unsafe { is_checked(page_hwnd, ID_ENABLED) };
    let global_search = unsafe { read_global_mode(page_hwnd) };
    let port = unsafe { GetDlgItemInt(page_hwnd, ID_PORT_EDIT, core::ptr::null_mut(), 0) };
    let mut bind = get_dlg_text(page_hwnd, ID_BIND_EDIT);
    if bind.trim().is_empty() {
        // 空地址按「仅本机」处理，避免误暴露到所有网卡。
        bind = DEFAULT_BIND.to_string();
    }

    // 内容索引与 MCP 服务无关，独立应用（先于端口校验，端口写错也不该
    // 拦下这个开关）。只在值真的变化时才写 ini / 调 -add-config。
    let content_indexing = unsafe { is_checked(page_hwnd, ID_CONTENT_INDEXING) };
    let mut content_indexing_failed = false;
    if read_content_indexing() != Some(content_indexing) {
        match apply_content_indexing(content_indexing) {
            // Ok 表示已回读校验生效，不只是「命令发出去了」。
            Ok(()) => diag::write(&format!(
                "options: content_indexing set to {} (verified)",
                content_indexing as i32
            )),
            Err(e) => {
                diag::write(&format!("options: content_indexing apply failed: {}", e));
                Host::debug("everything_mcp: content indexing apply failed");
                // 没生效就留住 Apply，让用户改完能再试一次。
                content_indexing_failed = true;
            }
        }
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

    if !ok || content_indexing_failed {
        // 应用失败（例如端口被占用，或内容索引开关没生效）——
        // 保持 Apply 可点，改完再试。
        page.enable_apply = 1;
    }
    abi_ok()
}

/// PM_GET_OPTIONS_PAGE_MINMAX：报告页面最小尺寸（逻辑像素）。
pub fn minmax(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let p = unsafe { &mut *(data as *mut GetOptionsPageMinMax) };
    if stats_page::is_stats_page(p.user_data) {
        p.wide = stats_page::STATS_PAGE_MIN_WIDE;
        p.high = stats_page::STATS_PAGE_MIN_HIGH;
    } else {
        p.wide = PAGE_MIN_WIDE;
        p.high = PAGE_MIN_HIGH;
    }
    abi_ok()
}

/// PM_SIZE_OPTIONS_PAGE：按页面客户区尺寸（换算成逻辑像素）摆放控件。
pub fn size_page(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let p = unsafe { &*(data as *const SizeOptionsPage) };
    if stats_page::is_stats_page(p.user_data) {
        return stats_page::size_stats_page(p.page_hwnd);
    }
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

    // 内容索引开关 + 两行说明（紧跟全局搜索行下方）。
    y += DLG_CHECKBOX_HIGH + DLG_SEPARATOR;
    set_rect(page_hwnd, ID_CONTENT_INDEXING, x, y, wide, DLG_CHECKBOX_HIGH);
    y += DLG_CHECKBOX_HIGH + 3;
    set_rect(page_hwnd, ID_CONTENT_NOTE, x, y, wide, DLG_STATIC_HIGH);
    y += DLG_STATIC_HIGH;
    // 格式列表会换行，预留两行高度（窄对话框下默认那串扩展名就占两行）。
    set_rect(
        page_hwnd,
        ID_CONTENT_FORMATS,
        x,
        y,
        wide,
        DLG_STATIC_HIGH * 2,
    );

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

    abi_ok()
}

/// PM_OPTIONS_PAGE_PROC：页面窗口消息（目前只关心 WM_COMMAND）。
pub fn page_proc(data: *mut c_void) -> *mut c_void {
    if data.is_null() {
        return core::ptr::null_mut();
    }
    let p = unsafe { &*(data as *const OptionsPageProc) };
    if stats_page::is_stats_page(p.user_data) {
        return stats_page::stats_page_proc(p);
    }
    if p.msg != WM_COMMAND {
        // 其它消息不处理（http_server.c 同样直接放过）。
        return abi_ok();
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
        ID_CONTENT_INDEXING => {
            // 内容索引是独立于 MCP 服务的核心设置，勾选变化也要 Apply。
            enable_apply(p.options_hwnd);
        }
        ID_GLOBAL_DENY | ID_GLOBAL_REVIEW | ID_GLOBAL_ALLOW => {
            // 互斥复选框（等同单选组）：点谁谁唯一选中，再通知 Apply。
            unsafe { set_global_mode(page_hwnd, id) };
            enable_apply(p.options_hwnd);
        }
        ID_BIND_EDIT | ID_PORT_EDIT if notify == EN_CHANGE => {
            enable_apply(p.options_hwnd);
        }
        _ => {}
    }

    abi_ok()
}

/// PM_KILL_OPTIONS_PAGE：控件由主程序创建和销毁，插件无自有资源。
pub fn kill_page(_data: *mut c_void) -> *mut c_void {
    abi_ok()
}

// ============================================================
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
    abi_ok()
}

// ============================================================
// 应用设置（启动 / 停止 / 重启监听）
// ============================================================

/// 把当前设置应用到 MCP 服务。返回 false 表示应用失败（调用方应保持
/// 选项页的 Apply 启用）。必须在主线程调用（PM_START / PM_SAVE_OPTIONS_PAGE）。
///
/// 锁纪律（2026-10 审计修复）：start/stop 是耗时操作（join 监听与连接
/// 线程），绝不能持有 OPTS 锁去做 —— MCP 工作线程的 global_search_mode()
/// / settings_snapshot() 都在等这把锁，在途请求会被卡到停机结束。
/// 模式是「快照 → 释放锁 → 应用 → 短暂持锁写回 running」。
fn apply() -> bool {
    let (enabled, target, was_running) = {
        let st = state();
        (st.enabled, (st.bind.clone(), st.port), st.running.clone())
    };

    if enabled {
        if was_running.as_ref() == Some(&target) {
            // 配置未变且已在监听 —— 不重启，避免打断已有连接。
            return true;
        }
        match mcp::server::start(&target.0, target.1) {
            Ok(()) => {
                let msg = format!("options: MCP listening on {}:{}", target.0, target.1);
                diag::write(&msg);
                Host::debug(&msg);
                state().running = Some(target);
                true
            }
            Err(e) => {
                let msg = format!("options: MCP start failed: {}", e);
                diag::write(&msg);
                Host::debug(&msg);
                state().running = None;
                false
            }
        }
    } else {
        if was_running.is_some() {
            mcp::server::stop();
            diag::write("options: MCP stopped (disabled)");
            Host::debug("everything_mcp: MCP stopped");
        }
        state().running = None;
        true
    }
}

// ============================================================
// 内容索引（Everything 核心设置 content_indexing_enabled）
//
// 新版 Everything 把这个开关从「索引 → 文件夹」挪到了「高级」页，位置不
// 直观。本页提供一个直达开关：
//   - 读：解析 Everything.ini 的 [Everything] 段；
//   - 写：原位改键（保留编码与行尾），并调官方命令行 `-add-config`
//     合并进运行中实例（1.5 实机验证：改 window_title_format 后窗口
//     标题立刻变化）；配合 db_save_on_exit=1，退出时落盘。
// 不用推测出来的 config_set_int_value：签名未公开，调错会崩宿主。
// ============================================================

/// Everything 核心 ini 键：内容索引总开关。
const CORE_KEY_CONTENT_INDEXING: &str = "content_indexing_enabled";

/// Everything 核心 ini 键：参与内容索引的文件类型白名单。
///
/// 本页**只读展示**这个键，不提供编辑。理由：它是全局设置，新增扩展名会让
/// Everything 对那类文件做内容提取（PDF/DOCX 走 IFilter），索引体积与 CPU
/// 都会涨；写错时 Everything 不报错、只是静默不索引。这个半径不该由一个
/// 插件页承担 —— 要改请到 Everything 高级页。
const CORE_KEY_CONTENT_FORMATS: &str = "content_indexing_include_only_files";

#[derive(Clone, Copy, PartialEq, Debug)]
enum IniEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Everything.ini 的候选路径：app_data 安装（默认）与便携安装（exe 旁）。
fn core_ini_candidates() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        paths.push(
            std::path::PathBuf::from(appdata)
                .join("Everything")
                .join("Everything.ini"),
        );
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("Everything.ini"));
        }
    }
    paths
}

/// 解码 ini 并保留编码 —— 写回时用同一种编码，避免把 UTF-16 的 ini 变成
/// UTF-8（Everything 自己写得出来 UTF-16，journal.rs 也为此专门处理过）。
///
/// 非 UTF-8/UTF-16 的 ANSI ini 返回 None：那种文件里可能含本机代码页的
/// 非 ASCII 路径，重写有损坏风险，宁可只走 `-add-config` 不落盘。
fn decode_core_ini(bytes: &[u8]) -> Option<(String, IniEncoding)> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return Some((String::from_utf16_lossy(&units), IniEncoding::Utf16Le));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return Some((String::from_utf16_lossy(&units), IniEncoding::Utf16Be));
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    core::str::from_utf8(body)
        .ok()
        .map(|s| (s.to_string(), IniEncoding::Utf8))
}

fn encode_core_ini(content: &str, enc: IniEncoding) -> Vec<u8> {
    match enc {
        IniEncoding::Utf8 => content.as_bytes().to_vec(),
        IniEncoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            for unit in content.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out
        }
        IniEncoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            for unit in content.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            out
        }
    }
}

/// 在 [Everything] 段里找键（大小写不敏感，跳过其它段）。
fn find_core_value(content: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            in_section = rest
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("Everything");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// 当前内容索引开关（供 server_diagnostics 回显；ini 不可读时为 None）。
pub(crate) fn content_indexing_enabled() -> Option<bool> {
    read_content_indexing()
}

/// 当前内容索引覆盖的文件类型（供设置页只读展示与 server_diagnostics 回显）。
pub(crate) fn content_indexing_formats() -> Option<String> {
    read_core_value(CORE_KEY_CONTENT_FORMATS).filter(|v| !v.trim().is_empty())
}

/// 读 `[Everything]` 段里某个键的原始值；ini 不存在或键缺失时返回 None。
fn read_core_value(key: &str) -> Option<String> {
    for path in core_ini_candidates() {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some((content, _)) = decode_core_ini(&bytes) {
                if let Some(v) = find_core_value(&content, key) {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// 读取内容索引开关的当前值；ini 不存在或键缺失时返回 None。
fn read_content_indexing() -> Option<bool> {
    read_core_value(CORE_KEY_CONTENT_INDEXING)
        .map(|v| v.parse::<i64>().map(|n| n != 0).unwrap_or(false))
}

/// 把 [Everything] 段里的键改成 `value`；键不存在时插到段头后（没有段头
/// 就先补 `[Everything]`）。其余行、行尾风格（CRLF/LF）原样保留。
fn set_core_value(content: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(content.len() + 40);
    let mut in_section = false;
    let mut replaced = false;
    for line in content.split_inclusive('\n') {
        let no_nl = line.strip_suffix('\n').unwrap_or(line);
        let raw = no_nl.strip_suffix('\r').unwrap_or(no_nl);
        let ending = &line[raw.len()..];
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            in_section = rest
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("Everything");
            out.push_str(line);
            continue;
        }
        if in_section {
            if let Some((k, _)) = raw.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    out.push_str(k.trim());
                    out.push('=');
                    out.push_str(value);
                    out.push_str(ending);
                    replaced = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    if replaced {
        return out;
    }

    // 键不在段里：插到 [Everything] 段头后面。
    let mut inserted = false;
    let mut final_out = String::with_capacity(out.len() + key.len() + value.len() + 16);
    for line in out.split_inclusive('\n') {
        final_out.push_str(line);
        let raw = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = raw.trim().strip_prefix('[') {
            let is_everything = rest
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("Everything");
            if is_everything && !inserted {
                // 段头在文件末尾且没有换行时，先把行断开再插键。
                let nl = if content.contains("\r\n") { "\r\n" } else { "\n" };
                if !line.ends_with('\n') {
                    final_out.push_str(nl);
                }
                final_out.push_str(key);
                final_out.push('=');
                final_out.push_str(value);
                final_out.push_str(nl);
                inserted = true;
            }
        }
    }
    if !inserted {
        final_out.insert_str(0, &format!("[Everything]\r\n{}={}\r\n", key, value));
    }
    final_out
}

/// 持久化内容索引开关到 Everything.ini（原子替换）。
fn write_content_indexing(enabled: bool) -> Result<(), String> {
    let path = core_ini_candidates()
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| "Everything.ini not found".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let (content, enc) = decode_core_ini(&bytes)
        .ok_or_else(|| format!("{} is not UTF-8/UTF-16; not rewriting it", path.display()))?;
    let updated = set_core_value(
        &content,
        CORE_KEY_CONTENT_INDEXING,
        &(enabled as i32).to_string(),
    );
    if updated == content {
        return Ok(());
    }
    let out = encode_core_ini(&updated, enc);
    let tmp = path.with_extension("ini.emcp.tmp");
    std::fs::write(&tmp, &out).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replace {}: {}", path.display(), e)
    })
}

/// 写临时配置并调官方 `-add-config` 把开关合并进运行中的实例。
///
/// 转发进程通常毫秒级退出；等待上限见 [`ADD_CONFIG_WAIT_STEPS`]。
fn run_add_config(exe: &std::path::Path, tmp: &std::path::Path, enabled: bool) -> Result<(), String> {
    std::fs::write(
        tmp,
        format!(
            "[Everything]\r\n{}={}\r\n",
            CORE_KEY_CONTENT_INDEXING, enabled as i32
        ),
    )
    .map_err(|e| format!("write temp config: {}", e))?;

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = std::process::Command::new(exe)
        .arg("-add-config")
        .arg(tmp)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("spawn -add-config: {}", e))?;

    for _ in 0..ADD_CONFIG_WAIT_STEPS {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(ADD_CONFIG_POLL_MS)),
            Err(_) => break,
        }
    }
    let _ = std::fs::remove_file(tmp);
    Ok(())
}

/// 应用开关的最大尝试次数。首次通常毫秒级成功，重试只覆盖实例未就绪的情况。
const APPLY_ATTEMPTS: u32 = 3;

/// 两次尝试之间的退避。Everything 刚重启时初始化可能持续数秒。
const APPLY_BACKOFF_MS: u64 = 250;

/// 等待 `-add-config` 转发进程退出的轮询间隔与次数上限（100 × 10ms = 1s）。
///
/// 它正常是毫秒级退出，这个上限只防子进程卡死。**整个 apply 跑在
/// Everything 的 UI 线程上**，所以单次等待必须短 —— 真正的冻结时长是
/// [`APPLY_ATTEMPTS`] 次叠加，最坏 3 × (1s + 250ms) 约 3.8 秒。
const ADD_CONFIG_POLL_MS: u64 = 10;
const ADD_CONFIG_WAIT_STEPS: u32 = 100;

/// 应用内容索引开关：先 `-add-config` 合并进运行中实例，再回读校验。
///
/// 顺序与校验都是修过的，两点都关键：
///
/// 1. **不要预先写 ini。** 转发进程退出时会用它自己那份状态覆盖 ini，
///    把插件刚写好的键又改回去。实机复现：安装插件重启 Everything 后
///    23 秒内勾选此开关，ini 先被写成 1、同一秒又被覆盖回 0，而插件
///    只看到「命令发出去了」就报了成功。单靠 `-add-config` 已足够——
///    它自己会把合并结果落盘（实测 ini 与内存同时变成新值）。
/// 2. **必须回读。** 合并可能因实例尚未就绪而静默失败，此时 ini 停留在
///    旧值。只有发命令不校验，就会把失败报成成功，用户以为开了其实没开。
///
/// 合并没生效时补写 ini 兜底（至少下次启动生效），仍不一致则退避重试；
/// 插件拿不到该设置的活动值（`config_set_int_value` 签名未公开，不敢调），
/// 回读 ini 是唯一可用的校验手段。
fn apply_content_indexing(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
    let tmp = std::env::temp_dir().join(format!(
        "everything_mcp_content_indexing_{}.ini",
        std::process::id()
    ));

    let mut last_err = String::new();
    for attempt in 1..=APPLY_ATTEMPTS {
        if let Err(e) = run_add_config(&exe, &tmp, enabled) {
            last_err = e;
            diag::write(&format!(
                "options: content_indexing -add-config failed (attempt {}/{}): {}",
                attempt, APPLY_ATTEMPTS, last_err
            ));
        } else if read_content_indexing() == Some(enabled) {
            return Ok(());
        }

        // 合并没生效 —— 直接补写 ini，至少保证下次启动时是新值。
        if let Err(e) = write_content_indexing(enabled) {
            diag::write(&format!(
                "options: content_indexing ini write failed (attempt {}): {}",
                attempt, e
            ));
        } else if read_content_indexing() == Some(enabled) {
            return Ok(());
        }

        // 仍不一致：实例可能还在初始化，退避后重试。
        if attempt < APPLY_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(APPLY_BACKOFF_MS));
        }
    }

    Err(format!(
        "content_indexing_enabled did not take effect after {} attempts{}",
        APPLY_ATTEMPTS,
        if last_err.is_empty() {
            String::new()
        } else {
            format!(" (last error: {})", last_err)
        }
    ))
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
pub(crate) fn client_size_logical(page_hwnd: HWND) -> (i32, i32) {
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
pub(crate) fn expand_min_wide(page_hwnd: HWND, text: &str, current_wide: i32) -> i32 {
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
pub(crate) fn set_rect(page_hwnd: HWND, id: i32, x: i32, y: i32, wide: i32, high: i32) {
    if let Some(f) = Host::get().os_set_dlg_rect {
        unsafe { f(page_hwnd, id, x, y, wide, high) };
    }
}

// ============================================================
// 界面文案（中/英）
// ============================================================

/// 设置页文案。SDK 只暴露主程序自己的本地化字符串，没有插件自定义文案的
/// 本地化机制，因此按主程序界面语言在中/英之间选择（见 labels()）。
/// 统计页（stats_page.rs）也从这里取文案 —— Labels 是全部界面文案的唯一来源。
pub(crate) struct Labels {
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
    content_indexing: &'static str,
    content_indexing_help: &'static str,
    content_indexing_note: &'static str,
    content_formats: &'static str,
    content_formats_help: &'static str,
    content_formats_unset: &'static str,
    restore: &'static str,
    restore_help: &'static str,
    // ---- Statistics 页 ----
    pub(crate) stats_page_name: &'static str,
    pub(crate) stats_total: &'static str,
    pub(crate) stats_connections: &'static str,
    pub(crate) stats_requests: &'static str,
    pub(crate) stats_col_tool: &'static str,
    pub(crate) stats_unknown_tool: &'static str,
    pub(crate) stats_col_calls: &'static str,
    pub(crate) stats_col_ok: &'static str,
    pub(crate) stats_col_err: &'static str,
    pub(crate) stats_col_avg: &'static str,
    pub(crate) stats_col_out: &'static str,
    pub(crate) stats_clear: &'static str,
    pub(crate) stats_clear_confirm: &'static str,
    pub(crate) stats_clear_help: &'static str,
}

const LABELS_EN: Labels = Labels {
    page_name: "MCP Server",
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
    content_indexing: "Enable content indexing",
    content_indexing_help: "Everything 1.5 moved this switch to the Advanced page (Tools > Options > Advanced: content_indexing_enabled). Ticking it applies immediately, no restart needed. It only widens what plain content: can search; utf8content:/ansicontent: read on demand and are not limited by it, and grep reads the files itself without depending on it.",
    content_indexing_note: "Advanced setting: content_indexing_enabled",
    content_formats: "Content-indexed formats: ",
    content_formats_help: "Read-only. This is Everything's content_indexing_include_only_files (Advanced page), which decides what content: can find. Edit it there, not here: changing it affects the size and CPU cost of the global content index.",
    content_formats_unset: "Content-indexed formats: not set (Advanced: content_indexing_include_only_files)",
    restore: "Restore Defaults",
    restore_help: "Reset to 127.0.0.1:8285 with the server disabled and global search set to Deny.",
    stats_page_name: "MCP Server Statistics",
    stats_total: "Total calls:",
    stats_connections: "Connections:",
    stats_requests: "Requests:",
    stats_col_tool: "Tool",
    stats_unknown_tool: "(other)",
    stats_col_calls: "Calls",
    stats_col_ok: "OK",
    stats_col_err: "Err",
    stats_col_avg: "Avg(ms)",
    stats_col_out: "Out(KiB)",
    stats_clear: "Clear Statistics",
    stats_clear_confirm: "Click again to confirm",
    stats_clear_help: "Reset all counters to zero and delete the local stats file.",
};

const LABELS_ZH: Labels = Labels {
    page_name: "MCP 服务器",
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
    content_indexing: "启用内容索引",
    content_indexing_help: "新版 Everything 把这个开关移到了「高级」页（工具 → 选项 → 高级：content_indexing_enabled）。勾选后本页直接应用，无需重启。它只影响纯 content: 能搜到哪些文件类型；utf8content:/ansicontent: 是按需读取、不受此列表限制，grep 更是自己读文件、不依赖这个开关。",
    content_indexing_note: "对应高级页设置 content_indexing_enabled",
    content_formats: "内容索引格式：",
    content_formats_help: "只读展示。这是 Everything 高级页的 content_indexing_include_only_files，决定 content: 能搜到哪些文件类型。请到 Everything 高级页修改——改动会影响全局内容索引的体积与 CPU 开销。",
    content_formats_unset: "内容索引格式：未设置（高级页 content_indexing_include_only_files）",
    restore: "恢复默认",
    restore_help: "恢复为 127.0.0.1:8285、不启用服务、全局搜索为「拒绝」。",
    stats_page_name: "MCP 服务器统计",
    stats_total: "总调用次数：",
    stats_connections: "连接数：",
    stats_requests: "请求数：",
    stats_col_tool: "工具",
    stats_unknown_tool: "（其他）",
    stats_col_calls: "调用",
    stats_col_ok: "成功",
    stats_col_err: "出错",
    stats_col_avg: "平均(ms)",
    stats_col_out: "输出(KiB)",
    stats_clear: "清空统计",
    stats_clear_confirm: "再点一次确认",
    stats_clear_help: "清零所有计数并删除本地统计文件。",
};

/// 「内容索引格式」那一行的文案 —— 只读展示 Everything 当前配置的覆盖范围。
///
/// 展示而不提供编辑，是为了让用户一眼看到 `content:` 能搜哪些格式、
/// 代码为什么搜不到（要用 grep），同时不把改写全局索引配置的能力放进插件页。
fn content_formats_text(labels: &Labels) -> String {
    match content_indexing_formats() {
        Some(list) => format!("{}{}", labels.content_formats, list),
        None => labels.content_formats_unset.to_string(),
    }
}

/// 按主程序界面语言取文案（探测一次后缓存）。
pub(crate) fn labels() -> &'static Labels {
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
pub(crate) fn cstr_bytes(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    // Statistics 页的测试随实现迁到了 stats_page.rs。

    use super::*;

    #[test]
    fn core_ini_finds_value_only_inside_everything_section() {
        let content = "[Everything]\r\ncontent_indexing_enabled=1\r\nother=2\r\n[Plugins]\r\ncontent_indexing_enabled=0\r\n";
        assert_eq!(
            find_core_value(content, "content_indexing_enabled"),
            Some("1".into())
        );
        // 大小写不敏感
        assert_eq!(
            find_core_value(content, "CONTENT_INDEXING_ENABLED"),
            Some("1".into())
        );
        // 段外的同名键不算
        let without = "[Other]\r\ncontent_indexing_enabled=1\r\n";
        assert_eq!(find_core_value(without, "content_indexing_enabled"), None);
    }

    #[test]
    fn core_ini_replaces_value_preserving_rest() {
        let content =
            "; comment\r\n[Everything]\r\nfoo=1\r\ncontent_indexing_enabled=0\r\nbar=2\r\n";
        let out = set_core_value(content, "content_indexing_enabled", "1");
        assert!(out.contains("content_indexing_enabled=1"));
        assert!(out.contains("foo=1") && out.contains("bar=2"));
        assert!(out.contains("; comment"));
        assert!(!out.contains("content_indexing_enabled=0"));
    }

    #[test]
    fn core_ini_inserts_after_section_header_when_missing() {
        let content = "[Everything]\r\nfoo=1\r\n[Other]\r\nbar=2\r\n";
        let out = set_core_value(content, "content_indexing_enabled", "1");
        let pos_key = out.find("content_indexing_enabled=1").expect("key 应被插入");
        let pos_other = out.find("[Other]").unwrap();
        assert!(pos_key < pos_other, "必须插在 [Everything] 段内: {out}");
        assert!(out.contains("foo=1"));
        // 没有 [Everything] 段时补一个
        let out2 = set_core_value("foo=1\r\n", "content_indexing_enabled", "0");
        assert!(out2.starts_with("[Everything]\r\ncontent_indexing_enabled=0"));
        assert!(out2.contains("foo=1"));
    }

    #[test]
    fn core_ini_roundtrips_utf16le() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "[Everything]\r\ncontent_indexing_enabled=0\r\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let (content, enc) = decode_core_ini(&bytes).unwrap();
        assert_eq!(enc, IniEncoding::Utf16Le);
        let updated = set_core_value(&content, "content_indexing_enabled", "1");
        let out = encode_core_ini(&updated, enc);
        assert_eq!(&out[..2], &[0xFF, 0xFE], "保留 BOM");
        let (again, enc2) = decode_core_ini(&out).unwrap();
        assert_eq!(enc2, IniEncoding::Utf16Le);
        assert_eq!(
            find_core_value(&again, "content_indexing_enabled"),
            Some("1".into())
        );
    }
}
