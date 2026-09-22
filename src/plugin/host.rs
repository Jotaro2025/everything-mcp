//! host.rs
//!
//! 主程序函数指针表。
//!
//! Everything 1.5 插件协议在 PM_INIT 时传入一个 `get_proc_address` 回调，
//! 插件通过它按 UTF-8 名称字符串索取主程序内部函数指针。
//!
//! 本模块定义：
//!   1. 所有需要的 host 函数的 Rust 函数指针类型签名；
//!   2. `Host` 结构 —— 一个集中存放所有函数指针的全局容器；
//!   3. `Host::install(...)` —— 在 PM_INIT 时根据 get_proc_address 填充；
//!   4. 一个进程级 `OnceLock<Host>`，作为整个插件的 host 接口入口。
//!
//! 安全约定：
//!   - 所有指针在 PM_INIT 之后才可用，在此之前调用 host 函数会 panic；
//!   - 主程序的这些函数**不是**线程安全的（共享 Everything 主线程状态），
//!     调用前必须持有 `HOST_LOCK`（一个进程级 Mutex）。

#![allow(non_snake_case)]
#![allow(dead_code)]

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use super::ffi_types::*;

// ====================================================================
// 类型别名：每个 host 函数的精确 C 签名
// 与 http_server.c / etp_server.c 中的 typedef 字节一致。
// ====================================================================

/// 主程序内存分配。
pub type MemAllocFn = unsafe extern "system" fn(size: usize) -> *mut c_void;
pub type MemFreeFn = unsafe extern "system" fn(ptr: *mut c_void);

/// UTF-8 缓冲区生命周期管理。
pub type Utf8BufInitFn = unsafe extern "system" fn(cbuf: *mut Utf8Buf);
pub type Utf8BufKillFn = unsafe extern "system" fn(cbuf: *mut Utf8Buf);

/// 事件 / 线程（用于查询完成同步）。
pub type OsEventCreateFn = unsafe extern "system" fn() -> *mut c_void;
/// Windows HANDLE 语义：传给 WaitForSingleObject。
pub type OsEventSetFn = unsafe extern "system" fn(handle: *mut c_void) -> i32;

/// 设置读取（PM_START 时从 data 取配置）。
pub type GetSettingIntFn =
    unsafe extern "system" fn(data: *mut c_void, name: *const u8, current: i32) -> i32;
pub type GetSettingStringFn =
    unsafe extern "system" fn(data: *mut c_void, name: *const u8, current: *mut u8) -> *mut u8;

/// 设置写回（PM_SAVE_SETTINGS 时把配置写进主程序的设置输出流）。
/// data 是 output_stream_t；键名与读取时一致（如 "mcp_port"）。
pub type SetSettingIntFn = unsafe extern "system" fn(data: *mut c_void, name: *const u8, value: i32);
pub type SetSettingStringFn =
    unsafe extern "system" fn(data: *mut c_void, name: *const u8, value: *const u8);

/// 取主程序本地化字符串（返回 UTF-8 null 结尾指针，主程序所有）。
/// 选项页 UI 文本用它探测界面语言（见 options 模块）。
pub type LocalizationGetStringFn = unsafe extern "system" fn(id: i32) -> *const u8;

/// 数据库引用计数管理。
pub type DbAddLocalRefFn = unsafe extern "system" fn() -> DbHandle;
pub type DbReleaseFn = unsafe extern "system" fn(db: DbHandle);

/// 数据库查询对象生命周期。
pub type DbQueryEventProcFn =
    unsafe extern "system" fn(user_data: *mut c_void, evtype: i32);
pub type DbQueryCreateFn =
    unsafe extern "system" fn(db: DbHandle, proc_: DbQueryEventProcFn, user_data: *mut c_void)
        -> DbQueryHandle;
pub type DbQueryDestroyFn = unsafe extern "system" fn(q: DbQueryHandle);
pub type DbCancelQueryFn = unsafe extern "system" fn(q: DbQueryHandle) -> i32;

/// 查询提交（异步，完成后调用 event_proc）。
///
/// 注意：真实导出名是 `db_query_search2`（不是 `db_query_search`），
/// 参数列表与 everything_plugin.h / etp_server.c 的实际签名字节一致。
/// 我们只用其中几个开关，其余传 0 / NULL 以匹配 etp_server.c 的默认调用。
///
/// **sort_property_type 不能传 NULL** —— etp_server.c 传的是
/// `property_get_builtin_type(PROPERTY_TYPE_NAME)` 返回的内置属性指针；
/// 传 NULL 会让主程序在排序阶段解引用空指针崩溃（0xc0000005）。
#[allow(clippy::too_many_arguments)]
pub type DbQuerySearchFn = unsafe extern "system" fn(
    q: DbQueryHandle,
    match_case: i32,
    match_whole_word: i32,
    match_path: i32,
    match_diacritics: i32,
    match_prefix: i32,
    match_suffix: i32,
    ignore_punctuation: i32,
    ignore_whitespace: i32,
    match_regex: i32,
    hide_empty_search_results: i32,
    clear_selection: i32,
    clear_item_refs: i32,
    search_string: *const u8,
    filter_flags: u32,
    filter: *const u8,
    filter_columns: *const u8,
    filter_sort: PropertyHandle,
    filter_sort_ascending: i32,
    filter_view: i32,
    fast_sort_only: i32,
    sort_property_type: PropertyHandle,
    sort_ascending: i32,
    sort_property_type2: PropertyHandle,
    sort_ascending2: i32,
    sort_property_type3: PropertyHandle,
    sort_ascending3: i32,
    folders_first: i32,
    dialog_center_x: i32,
    dialog_center_y: i32,
    track_selected_and_total_file_size: i32,
    track_selected_folder_size: i32,
    force: i32,
    allow_query_access: i32,
    allow_read_access: i32,
    allow_disk_access: i32,
    hide_omit_results: i32,
    size_standard: i32,
    match_treeview: i32,
    treeview_subfolders: i32,
    sort_mix: i32,
);

/// 查询结果读取。
pub type DbQueryGetResultCountFn = unsafe extern "system" fn(q: DbQueryHandle) -> usize;
pub type DbQueryGetResultNameFn =
    unsafe extern "system" fn(q: DbQueryHandle, index: usize, cbuf: *mut Utf8Buf);
pub type DbQueryGetResultPathFn =
    unsafe extern "system" fn(q: DbQueryHandle, index: usize, cbuf: *mut Utf8Buf);
pub type DbQueryGetResultIndexedFdFn =
    unsafe extern "system" fn(q: DbQueryHandle, index: usize, fd: *mut FileInfoFd);
pub type DbQueryIsFolderResultFn = unsafe extern "system" fn(q: DbQueryHandle, index: usize) -> i32;

/// 取内置属性（如 NAME）指针，用于 db_query_search2 的 sort_property_type。
pub type PropertyGetBuiltinTypeFn = unsafe extern "system" fn(type_: i32) -> PropertyHandle;

/// 文件夹列举（不通过 query 对象，直接对 db）。
pub type DbFindFirstFileFn = unsafe extern "system" fn(
    db: DbHandle,
    path: *const u8,
    filename_cbuf: *mut Utf8Buf,
    fd: *mut FileInfoFd,
) -> DbFindHandle;
pub type DbFindNextFileFn =
    unsafe extern "system" fn(fh: DbFindHandle, filename_cbuf: *mut Utf8Buf, fd: *mut FileInfoFd) -> i32;
pub type DbFindCloseFn = unsafe extern "system" fn(fh: DbFindHandle);
pub type DbFindGetCountFn = unsafe extern "system" fn(fh: DbFindHandle) -> usize;

/// 文件 / 文件夹存在性检查。
pub type DbFolderExistsFn = unsafe extern "system" fn(db: DbHandle, filename: *const u8) -> i32;
pub type DbFileExistsFn = unsafe extern "system" fn(db: DbHandle, filename: *const u8) -> i32;

/// Debug 输出（可选；不存在不影响功能）。
pub type DebugPrintfFn = unsafe extern "system" fn(fmt: *const u8, ...) -> i32;

/// 由主程序创建的工作线程。
///
/// 注意：主程序在创建线程时会设置它需要的 TLS（线程本地存储），
/// 因此 db_query_search2 等接口必须从这种线程上发起调用 ——
/// 不能用 std::thread::spawn 创建的线程。
pub type OsThreadCreateFn = unsafe extern "system" fn(
    thread_proc: unsafe extern "system" fn(*mut c_void) -> u32,
    param: *mut c_void,
) -> *mut c_void;
pub type OsThreadWaitAndCloseFn = unsafe extern "system" fn(thread: *mut c_void) -> u32;

// ====================================================================
// 主程序的窗口管理封装
//
// 重要：插件需要把工作 marshal 到 Everything 主线程的消息循环里执行时，
// 注册窗口类和创建窗口都**必须**通过这两个 host 函数做，而不是直接调用
// Win32 的 RegisterClassExW / CreateWindowExW。
//
// 主程序在自己的消息循环里只会分发它自己创建过的窗口的消息 —— 直接
// CreateWindowExW 出来的窗口不会被主消息泵处理，PostMessage 进去的消息
// 永远不会被取出（造成死锁），或者更糟，触发主程序内部的窗口表越界（崩溃）。
//
// 这正是 etp_server.c 的做法（行 2822-2828）。
// ====================================================================

#[allow(non_camel_case_types)]
pub type UINT = u32;
#[allow(non_camel_case_types)]
pub type DWORD = u32;
#[allow(non_camel_case_types)]
pub type HWND = *mut c_void;
#[allow(non_camel_case_types)]
pub type HMENU = *mut c_void;
#[allow(non_camel_case_types)]
pub type HINSTANCE = *mut c_void;
#[allow(non_camel_case_types)]
pub type HICON = *mut c_void;
#[allow(non_camel_case_types)]
pub type HCURSOR = *mut c_void;
#[allow(non_camel_case_types)]
pub type LPVOID = *mut c_void;
#[allow(non_camel_case_types)]
/// Win32 窗口过程函数指针 —— 与 user32 一致。
pub type WNDPROC = Option<
    unsafe extern "system" fn(
        hwnd: HWND,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize,
>;

pub type OsRegisterClassFn = unsafe extern "system" fn(
    style: UINT,
    lpszClassName: *const u8,
    lpfnWndProc: WNDPROC,
    window_extra: usize,
    hIcon: HICON,
    hIconSm: HICON,
    hcursor: HCURSOR,
);

pub type OsCreateWindowFn = unsafe extern "system" fn(
    dwExStyle: DWORD,
    lpClassName: *const u8,
    lpWindowName: *const u8,
    dwStyle: DWORD,
    x: i32,
    y: i32,
    nWidth: i32,
    nHeight: i32,
    hWndParent: HWND,
    hMenu: HMENU,
    hInstance: HINSTANCE,
    lpParam: LPVOID,
) -> HWND;

// ====================================================================
// 选项页 UI 工厂函数
//
// Everything 选项对话框里的插件页由主程序提供的这些工厂函数搭建控件，
// 以保持与主程序一致的视觉风格、DPI 缩放与本地化。
// 签名与 http_server.c 中的 typedef 字节一致；控件 ID 由插件自定义，
// 之后即可按标准 Win32 对话框方式操作（如 IsDlgButtonChecked）。
//
// 这些函数全部是**可选**依赖：主程序未暴露时插件照常加载，
// 只是没有设置页（见 options 模块的降级处理）。
// ====================================================================

/// 在 Everything 选项对话框中注册一个插件设置页（PM_ADD_OPTIONS_PAGES 时调用）。
/// 第一个参数是 PM_ADD_OPTIONS_PAGES 的 data（不透明，直接透传）。
pub type UiOptionsAddPluginPageFn =
    unsafe extern "system" fn(
        add_custom_page: *mut c_void,
        user_data: *mut c_void,
        name: *const u8,
    ) -> *mut c_void;

pub type OsCreateCheckboxFn = unsafe extern "system" fn(
    parent: HWND,
    id: i32,
    extra_style: DWORD,
    checked: i32,
    text: *const u8,
) -> HWND;
pub type OsCreateStaticFn = unsafe extern "system" fn(
    parent: HWND,
    id: i32,
    extra_style: DWORD,
    text: *const u8,
) -> HWND;
pub type OsCreateEditFn = unsafe extern "system" fn(
    parent: HWND,
    id: i32,
    extra_style: DWORD,
    text: *const u8,
) -> HWND;
/// 数字编辑框 —— `number` 是 __int64。
pub type OsCreateNumberEditFn =
    unsafe extern "system" fn(parent: HWND, id: i32, extra_style: DWORD, number: i64) -> HWND;
pub type OsCreateButtonFn = unsafe extern "system" fn(
    parent: HWND,
    id: i32,
    extra_style: DWORD,
    text: *const u8,
) -> HWND;
pub type OsAddTooltipFn =
    unsafe extern "system" fn(tooltip: HWND, parent: HWND, id: i32, text: *const u8);
pub type OsSetDlgRectFn =
    unsafe extern "system" fn(parent: HWND, id: i32, x: i32, y: i32, wide: i32, high: i32);
pub type OsSetDlgTextFn = unsafe extern "system" fn(hwnd: HWND, id: i32, s: *const u8) -> i32;
pub type OsGetDlgTextFn = unsafe extern "system" fn(hwnd: HWND, id: i32, cbuf: *mut Utf8Buf);
pub type OsEnableOrDisableDlgItemFn = unsafe extern "system" fn(parent: HWND, id: i32, enable: i32);
/// 逻辑像素尺寸（DPI 感知）—— 布局时把物理像素换算成逻辑像素用。
pub type OsGetLogicalWideFn = unsafe extern "system" fn() -> i32;
pub type OsGetLogicalHighFn = unsafe extern "system" fn() -> i32;
/// 计算文本在页面上占用的逻辑宽度（展开 && 助记符），用于静态标签列宽。
pub type OsExpandDialogTextLogicalWideNoPrefixFn =
    unsafe extern "system" fn(parent: HWND, text: *const u8, wide: i32) -> i32;

// ====================================================================
// 函数指针表
// ====================================================================

#[derive(Default)]
pub struct Host {
    pub mem_alloc: Option<MemAllocFn>,
    pub mem_free: Option<MemFreeFn>,
    pub utf8_buf_init: Option<Utf8BufInitFn>,
    pub utf8_buf_kill: Option<Utf8BufKillFn>,
    pub os_event_create: Option<OsEventCreateFn>,
    pub os_event_set: Option<OsEventSetFn>,
    pub get_setting_int: Option<GetSettingIntFn>,
    pub get_setting_string: Option<GetSettingStringFn>,
    pub set_setting_int: Option<SetSettingIntFn>,
    pub set_setting_string: Option<SetSettingStringFn>,
    pub localization_get_string: Option<LocalizationGetStringFn>,
    pub db_add_local_ref: Option<DbAddLocalRefFn>,
    pub db_release: Option<DbReleaseFn>,
    pub db_query_create: Option<DbQueryCreateFn>,
    pub db_query_destroy: Option<DbQueryDestroyFn>,
    pub db_query_cancel: Option<DbCancelQueryFn>,
    pub db_query_search: Option<DbQuerySearchFn>,
    pub db_query_get_result_count: Option<DbQueryGetResultCountFn>,
    pub db_query_get_result_name: Option<DbQueryGetResultNameFn>,
    pub db_query_get_result_path: Option<DbQueryGetResultPathFn>,
    pub db_query_get_result_indexed_fd: Option<DbQueryGetResultIndexedFdFn>,
    pub db_query_is_folder_result: Option<DbQueryIsFolderResultFn>,
    /// 取内置属性（排序列类型）指针。db_query_search2 的 sort_property_type
    /// 必须是它返回的有效指针，不能是 NULL。
    pub property_get_builtin_type: Option<PropertyGetBuiltinTypeFn>,
    pub db_find_first_file: Option<DbFindFirstFileFn>,
    pub db_find_next_file: Option<DbFindNextFileFn>,
    pub db_find_close: Option<DbFindCloseFn>,
    pub db_find_get_count: Option<DbFindGetCountFn>,
    pub db_folder_exists: Option<DbFolderExistsFn>,
    pub db_file_exists: Option<DbFileExistsFn>,
    pub debug_printf: Option<DebugPrintfFn>,
    /// 主程序创建的工作线程。`db_query_search2` 必须从这种线程上发起。
    pub os_thread_create: Option<OsThreadCreateFn>,
    pub os_thread_wait_and_close: Option<OsThreadWaitAndCloseFn>,
    /// 主程序封装的窗口管理：注册窗口类、创建窗口。
    /// 必须用这两个函数 —— 主程序的消息泵只处理它自己创建的窗口的消息。
    pub os_register_class: Option<OsRegisterClassFn>,
    pub os_create_window: Option<OsCreateWindowFn>,
    /// 选项页注册 + 控件工厂（设置页 UI）。全部可选：
    /// 主程序未暴露时插件无设置页，仅失去图形配置入口。
    pub ui_options_add_plugin_page: Option<UiOptionsAddPluginPageFn>,
    pub os_create_checkbox: Option<OsCreateCheckboxFn>,
    pub os_create_static: Option<OsCreateStaticFn>,
    pub os_create_edit: Option<OsCreateEditFn>,
    pub os_create_number_edit: Option<OsCreateNumberEditFn>,
    pub os_create_button: Option<OsCreateButtonFn>,
    pub os_add_tooltip: Option<OsAddTooltipFn>,
    pub os_set_dlg_rect: Option<OsSetDlgRectFn>,
    pub os_set_dlg_text: Option<OsSetDlgTextFn>,
    pub os_get_dlg_text: Option<OsGetDlgTextFn>,
    pub os_enable_or_disable_dlg_item: Option<OsEnableOrDisableDlgItemFn>,
    pub os_get_logical_wide: Option<OsGetLogicalWideFn>,
    pub os_get_logical_high: Option<OsGetLogicalHighFn>,
    pub os_expand_dialog_text_logical_wide_no_prefix: Option<OsExpandDialogTextLogicalWideNoPrefixFn>,
}

/// `get_proc_address` 回调的类型。
pub type GetProcAddressFn = unsafe extern "system" fn(name: *const u8) -> *mut c_void;

/// 进程级 host 表 —— 在 PM_INIT 时填充一次后只读访问。
pub static HOST: OnceLock<Host> = OnceLock::new();

/// 所有 host 函数调用前的全局互斥锁。
///
/// 主程序的数据库 / 查询接口假定单线程访问（来自 Everything UI 主线程）。
/// 我们的 MCP HTTP 工作线程在调用任何 host 函数前必须持有此锁，
/// 否则可能观察到部分写入或竞态。
pub static HOST_LOCK: Mutex<()> = Mutex::new(());

impl Host {
    /// 在 PM_INIT 时调用，按名称索取所有必需的函数指针。
    ///
    /// 返回 `Ok(())` 表示所有**强制**函数都获取成功；
    /// 任一强制函数不存在则返回 `Err(name)`，主程序应当拒绝加载本插件。
    pub fn install(get_proc_address: GetProcAddressFn) -> Result<(), &'static str> {
        let mut host = Host::default();
        super::diag::write("PM_INIT begin");

        // 强制函数 —— 任一缺失即失败。
        // 使用闭包简化和统一错误处理。
        macro_rules! req {
            ($field:ident, $name:literal) => {{
                // concat! 要求字面量参数 —— 因此 $name 必须是字符串字面量。
                let p = unsafe { get_proc_address(concat!($name, "\0").as_ptr()) };
                super::diag::write(&format!("  resolve {}: {}", $name, if p.is_null() { "MISSING" } else { "ok" }));
                if p.is_null() {
                    return Err($name);
                }
                host.$field = Some(unsafe { core::mem::transmute::<*mut c_void, _>(p) });
            }};
        }
        // 可选函数 —— 缺失不影响核心功能。
        macro_rules! opt {
            ($field:ident, $name:literal) => {{
                let p = unsafe { get_proc_address(concat!($name, "\0").as_ptr()) };
                if !p.is_null() {
                    host.$field = Some(unsafe { core::mem::transmute::<*mut c_void, _>(p) });
                }
            }};
        }

        // 内存管理 —— 强制
        req!(mem_alloc, "mem_alloc");
        req!(mem_free, "mem_free");
        req!(utf8_buf_init, "utf8_buf_init");
        req!(utf8_buf_kill, "utf8_buf_kill");

        // 同步原语 —— 强制（用于等待异步查询完成）
        req!(os_event_create, "os_event_create");
        // os_event_set 在头文件中未列名 —— 若不可用，回退到 Win32 SetEvent 系统调用，
        // 见 sync 模块。这里尝试获取，失败不致命。
        opt!(os_event_set, "os_event_set");

        // 设置 —— 可选（PM_START 时使用，缺失则全部使用编译期默认值）。
        // 真实导出名是 plugin_get_setting_int / plugin_get_setting_string
        // （已对 Everything 1.5.0.1422b 的字符串表核对：裸名 get_setting_*
        // 在该可执行文件里根本不存在）。
        opt!(get_setting_int, "plugin_get_setting_int");
        opt!(get_setting_string, "plugin_get_setting_string");
        // 设置写回（PM_SAVE_SETTINGS / 选项页持久化）。同样按可选处理：
        // 缺失时设置页仍可即时应用，只是无法写进 Everything.ini。
        opt!(set_setting_int, "plugin_set_setting_int");
        opt!(set_setting_string, "plugin_set_setting_string");
        // 本地化字符串 —— 仅用于探测界面语言选择设置页文案（见 options 模块）。
        opt!(localization_get_string, "localization_get_string");

        // 数据库 —— 强制
        req!(db_add_local_ref, "db_add_local_ref");
        req!(db_release, "db_release");
        req!(db_query_create, "db_query_create");
        req!(db_query_destroy, "db_query_destroy");
        // 数据库查询 —— 全部可选。1.5.0.1422b 上实测某些函数尚未对外暴露；
        // 具体哪个可用属于版本差异。
        // 调用方（search.rs）一律使用 .ok_or(...)？ —— 任一缺失会在用户实际
        // 触发对应搜索时给出明确错误，而不会阻断插件加载。
        // 真实导出名是 db_query_cancel（不是 db_cancel_query）。
        opt!(db_query_cancel, "db_query_cancel");
        // 真实函数名是 db_query_search2 —— 不是 db_query_search。
        // 参数表与 etp_server.c 中的 everything_plugin_db_query_search2 字节一致。
        opt!(db_query_search, "db_query_search2");
        opt!(db_query_get_result_count, "db_query_get_result_count");
        opt!(db_query_get_result_name, "db_query_get_result_name");
        opt!(db_query_get_result_path, "db_query_get_result_path");
        opt!(db_query_get_result_indexed_fd, "db_query_get_result_indexed_fd");
        opt!(db_query_is_folder_result, "db_query_is_folder_result");
        // 内置属性指针 —— db_query_search2 的 sort_property_type 必须用它
        // （传 NULL 会崩）。按强制项解析：拿不到就不该执行搜索。
        req!(property_get_builtin_type, "property_get_builtin_type");
        opt!(db_find_first_file, "db_find_first_file");
        opt!(db_find_next_file, "db_find_next_file");
        opt!(db_find_close, "db_find_close");
        opt!(db_find_get_count, "db_find_get_count");
        opt!(db_folder_exists, "db_folder_exists");
        opt!(db_file_exists, "db_file_exists");

        // Debug —— 可选
        opt!(debug_printf, "debug_printf");
        // 主程序创建的工作线程 —— 用于在不破坏主程序 TLS 的前提下调用 db_query_search2。
        opt!(os_thread_create, "os_thread_create");
        opt!(os_thread_wait_and_close, "os_thread_wait_and_close");

        // 主程序封装的窗口管理 —— 主消息泵只处理它自己创建过的窗口的消息。
        // 这是主线程 marshaling 的关键基础设施。
        opt!(os_register_class, "os_register_class");
        opt!(os_create_window, "os_create_window");

        // 选项页 UI（设置页）—— 全部可选。任一项缺失时设置页降级：
        // 插件功能不受影响，仅没有图形配置入口（继续支持手改 Everything.ini）。
        opt!(ui_options_add_plugin_page, "ui_options_add_plugin_page");
        opt!(os_create_checkbox, "os_create_checkbox");
        opt!(os_create_static, "os_create_static");
        opt!(os_create_edit, "os_create_edit");
        opt!(os_create_number_edit, "os_create_number_edit");
        opt!(os_create_button, "os_create_button");
        opt!(os_add_tooltip, "os_add_tooltip");
        opt!(os_set_dlg_rect, "os_set_dlg_rect");
        opt!(os_set_dlg_text, "os_set_dlg_text");
        opt!(os_get_dlg_text, "os_get_dlg_text");
        opt!(os_enable_or_disable_dlg_item, "os_enable_or_disable_dlg_item");
        opt!(os_get_logical_wide, "os_get_logical_wide");
        opt!(os_get_logical_high, "os_get_logical_high");
        opt!(
            os_expand_dialog_text_logical_wide_no_prefix,
            "os_expand_dialog_text_logical_wide_no_prefix"
        );

        // 安装到全局表。如果已经被填充（理论上不应发生），保留旧值。
        let _ = HOST.set(host);
        Ok(())
    }

    /// 取全局 host 表的引用。必须在 PM_INIT 之后调用。
    pub fn get() -> &'static Host {
        HOST.get().expect("HOST not initialized; PM_INIT not received")
    }

    /// 输出一行调试日志到主程序的调试输出窗口。
    /// 通过主程序提供的 `debug_printf("%s", msg)` 接口。
    /// 如果 host 表尚未初始化或 debug_printf 不可用，则静默忽略。
    pub fn debug(msg: &str) {
        let host = match HOST.get() {
            Some(h) => h,
            None => return,
        };
        if let Some(dp) = host.debug_printf {
            // 用 "%s\0" 作为格式串，msg 作为参数。
            // 注意：%s 期望的是 null 结尾的 UTF-8 字符串。
            let mut bytes = Vec::with_capacity(msg.len() + 1);
            bytes.extend_from_slice(msg.as_bytes());
            bytes.push(0);
            unsafe { dp(b"%s\0".as_ptr(), bytes.as_ptr()) };
        }
    }
}
