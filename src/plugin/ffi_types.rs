//! ffi_types.rs
//!
//! 与 Everything 1.5 主程序共享的 C ABI 类型定义。
//! 所有 `#[repr(C)]` 结构体必须与 everything_plugin.h 中的定义保持字节一致。
//!
//! 安全约定：本文件中的类型只用于跨 FFI 边界，Rust 代码不应直接读写其字段，
//! 除了少数明确标注的「值类型」（Utf8Buf 在调用前后用 init/kill 包裹）。

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use core::ffi::c_void;

/// 主程序分配的 UTF-8 字符串缓冲区。
///
/// 与 everything_plugin.h 中 everything_plugin_utf8_buf_t 字段一一对应：
///   buf   : 指向 UTF-8 字符串的指针
///   len   : 已写入字节数（不含 null）
///   size  : 缓冲区容量（为 0 表示 buf 指向外部 const 缓冲区）
///   stack : 内联栈缓冲区（MAX_PATH = 260 字节）
///
/// 关键：必须保留 stack[260] 字段，否则 utf8_buf_init 会写到结构体之外的栈内存。
/// 字段顺序和大小经过与 everything_plugin.h 的 byte-level 比对验证。
///
/// 生命周期：必须由 `utf8_buf_init` 初始化、`utf8_buf_kill` 销毁。
/// buf 指针归主程序所有，禁止 Rust 端 free。
#[repr(C)]
pub struct Utf8Buf {
    pub buf: *mut u8,
    pub len: usize,
    pub size: usize,
    pub stack: [u8; EVERYTHING_PLUGIN_UTF8_BUF_STACK_SIZE],
}

/// 与 everything_plugin.h 的 #define 一致：MAX_PATH。
const EVERYTHING_PLUGIN_UTF8_BUF_STACK_SIZE: usize = 260;

impl Default for Utf8Buf {
    fn default() -> Self {
        // 安全的初始零值，对应 C 端未显式 init 前的状态。
        // stack 数组必须清零，避免被误读为有效 UTF-8。
        Self {
            buf: core::ptr::null_mut(),
            len: 0,
            size: 0,
            stack: [0; EVERYTHING_PLUGIN_UTF8_BUF_STACK_SIZE],
        }
    }
}

impl Utf8Buf {
    /// 取出 buf 内容并复制为 Rust String。如果 buf 为空或不是合法 UTF-8，
    /// 返回空串而不报错（搜索结果中文件名几乎总是合法 UTF-8）。
    ///
    /// # Safety
    /// 调用者必须保证此时 buf..buf+len 是有效的可读内存。
    pub unsafe fn to_string(&self) -> String {
        if self.buf.is_null() || self.len == 0 {
            return String::new();
        }
        let slice = core::slice::from_raw_parts(self.buf, self.len);
        String::from_utf8_lossy(slice).into_owned()
    }
}

/// 主程序中的数据库句柄。不透明指针，仅作为参数透传给主程序函数。
pub type DbHandle = *mut c_void;

/// 数据库查询句柄。一次 `db_query_create` 对应一个。
pub type DbQueryHandle = *mut c_void;

/// `db_find_*` 系列返回的查找句柄（文件夹列举）。
pub type DbFindHandle = *mut c_void;

/// 文件信息 fd 结构 —— 与 everything_plugin.h 中
/// everything_plugin_fileinfo_fd_t 字段顺序与类型字节级一致：
///   size           : u64（完整文件大小）
///   date_created   : u64（FILETIME）
///   date_modified  : u64（FILETIME）
///   date_accessed  : u64（FILETIME）
///   attributes     : u32（Windows 文件属性位）
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct FileInfoFd {
    pub size: u64,
    pub date_created: u64,
    pub date_modified: u64,
    pub date_accessed: u64,
    pub attributes: u32,
}

impl FileInfoFd {
    pub fn file_size(&self) -> u64 {
        self.size
    }
    pub fn is_directory(&self) -> bool {
        (self.attributes & 0x10) != 0 // FILE_ATTRIBUTE_DIRECTORY
    }
}

/// `property_t` 不透明指针。用于 sort 列类型；
/// 我们暂时用 NULL（不排序）或主程序返回的内置类型指针。
pub type PropertyHandle = *const c_void;

/// 数据库事件类型常量（与 everything_plugin.h 对齐）。
///
/// 主程序在查询/排序完成时通过 `event_proc` 回调这些值。
pub mod db_event {
    pub const QUERY_COMPLETE: i32 = 5;
    pub const SORT_COMPLETE: i32 = 6;
}

/// 文件夹搜索默认排序常量（PropertyType = 0 表示 NAME）。
/// 主程序中通过 `property_get_builtin_type(0)` 可取得对应的 property_t 指针，
/// 但我们这里只用空指针跳过排序，让主程序按默认返回。
pub const PROPERTY_TYPE_NAME: i32 = 0;

/// 主程序配置大小显示风格常量（来自 everything_plugin.h）。
/// SIZE_STANDARD_JEDEC = 0 是默认二进制风格（KB/MB/GB）。
pub const SIZE_STANDARD_JEDEC: i32 = 0;
