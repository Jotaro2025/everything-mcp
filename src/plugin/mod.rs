//! plugin/mod.rs
//!
//! 插件入口与生命周期分发。把「单一导出函数 everything_plugin_proc」
//! 分发到子模块：ffi_types、host、search、state。

// PM_* 消息常量列出全部值便于对照主程序文档，其中部分未使用。
#![allow(dead_code)]

pub mod diag;
pub mod ffi_types;
pub mod host;
pub mod ini_settings;
pub mod main_thread;
pub mod search;
pub mod state;

// PM_* 消息常量 —— 来自 everything_plugin.h。
pub const PM_INIT: u32 = 1;
pub const PM_QUIT: u32 = 2;
pub const PM_GET_NAME: u32 = 3;
pub const PM_GET_VERSION: u32 = 4;
pub const PM_GET_DESCRIPTION: u32 = 5;
pub const PM_GET_AUTHOR: u32 = 6;
pub const PM_GET_LINK: u32 = 7;
pub const PM_START: u32 = 8;
pub const PM_STOP: u32 = 9;
pub const PM_KILL: u32 = 10;
pub const PM_GET_PLUGIN_VERSION: u32 = 11;
/// 注册选项页：data 是 everything_plugin_ui_options_add_custom_page_s*。
pub const PM_ADD_OPTIONS_PAGES: u32 = 12;
/// 加载选项页：data 是 everything_plugin_load_options_page_t*。
pub const PM_LOAD_OPTIONS_PAGE: u32 = 13;
/// 保存选项页：data 是 everything_plugin_save_options_page_t*。
pub const PM_SAVE_OPTIONS_PAGE: u32 = 14;
/// 查询选项页最小尺寸：data 是 everything_plugin_get_options_page_minmax_t*。
pub const PM_GET_OPTIONS_PAGE_MINMAX: u32 = 15;
/// 选项页尺寸变化：data 是 everything_plugin_size_options_page_t*。
pub const PM_SIZE_OPTIONS_PAGE: u32 = 16;
/// 选项页窗口消息转发：data 是 everything_plugin_options_page_proc_t*。
pub const PM_OPTIONS_PAGE_PROC: u32 = 17;
/// 选项页销毁后的清理回调：data 是注册时的 user_data。
pub const PM_KILL_OPTIONS_PAGE: u32 = 18;
/// 保存设置：data 是设置上下文，插件在此写回自己的配置项（见 etp_server.c）。
pub const PM_SAVE_SETTINGS: u32 = 19;
