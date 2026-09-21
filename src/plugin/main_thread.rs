//! main_thread.rs — 把工作线程的任务 marshal 到 Everything 主线程上执行。
//!
//! 背景：Everything 1.5 的 db_query_search2 必须从主线程（运行窗口消息循环的
//! 那个线程）调用 —— 既不能用 std::thread::spawn 创建的线程，也不能用 host
//! 的 os_thread_create 创建的线程。etp_server.c 通过 WSAAsyncSelect 把
//! socket 事件转换成窗口消息，让一切处理都发生在主线程的 wndproc 里。
//!
//! 关键约束（来自 etp_server.c 行 2822-2828）：
//! 注册窗口类和创建窗口**必须**通过主程序提供的 `os_register_class` 和
//! `os_create_window` 来做。主程序的消息泵只处理它自己创建过的窗口的消息 ——
//! 直接调用 Win32 `RegisterClassExW` / `CreateWindowExW` 创建出来的窗口
//! 要么不会被主消息泵分发（消息卡死），要么触发主程序内部窗口表异常（崩溃）。
//!
//! 我们的实现：在 PM_START 时（运行在主线程）通过 host 的 os_register_class +
//! os_create_window 创建一个隐藏窗口。工作线程把任务（一个 C 风格函数指针 +
//! 上下文裸指针）存进 `PENDING_TASK`，再 PostMessageW 投递 WM_INVOKE；
//! 主线程 wndproc 取出任务直接调用，完成后置位 done 标志，工作线程自旋等待。
//!
//! **刻意不使用 Rust 闭包 / Box<dyn FnOnce>**：wndproc 是 host 消息泵回调的
//! 栈帧，里面跑 Rust 闭包间接分发曾在实测中触发崩溃。改成纯 C 函数指针后
//! 调用路径与 etp_server.c 完全一致。

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::host::{Host, HCURSOR, HICON, HINSTANCE, HMENU, HWND as HostHwnd};

/// 自定义消息：把一个工作线程的 invocation 投递到主线程上执行。
/// 用 WM_USER+4 —— 与 etp_server 同一消息段（WM_USER..WM_USER+3 已被它占用）。
pub const WM_INVOKE: u32 = 0x0400 + 4; // WM_USER+4

/// 进程级主线程窗口句柄 —— 一旦 PM_START 注册成功就有效。
static MAIN_HWND: OnceLock<isize> = OnceLock::new();

/// install_on_main_thread 调用时的线程 ID —— 用于诊断 wnd_proc 是否真的
/// 在主线程上被调度（PostMessage 投递的消息由创建窗口的线程的消息泵处理）。
static INSTALL_TID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 我们注册的窗口类名（UTF-8，null 结尾的字节串）。
const CLASS_NAME: &[u8] = b"EverythingMcpMain\0";

/// 任务入口：纯 C 风格函数指针，参数是上下文裸指针。
///
/// 实现方自行保证 ctx 指向的对象在调用期间有效，
/// 并在返回前把 done 标志置位（由 invoke_c 传入的 Arc 负责）。
pub type TaskFn = unsafe extern "system" fn(*mut c_void);

/// 待主线程执行的任务。由工作线程填充，主线程 wndproc 取出执行。
struct PendingTask {
    func: TaskFn,
    ctx: *mut c_void,
    done: Arc<AtomicBool>,
}

// SAFETY：任务只在 invoke_c 的同步窗口内跨线程传递：
// 工作线程写入 → PostMessageW（内核屏障）→ 主线程读取执行 → 置位 done。
// 同一时刻只有一个 invoke_c 在飞行（HOST_LOCK 串行化），不存在并发覆写。
unsafe impl Send for PendingTask {}

/// 任务槽。用 Mutex<Option<_>> 而非 OnceLock —— 一次插件生命周期内
/// 会有多次 search / read_results 调用，每次都需要重新装填。
static PENDING_TASK: Mutex<Option<PendingTask>> = Mutex::new(None);

/// 取主窗口句柄；注册成功后才有，否则返回 None。
pub fn main_hwnd() -> Option<isize> {
    MAIN_HWND.get().copied()
}

/// 在主线程上执行一次 C 风格任务，阻塞直到完成。
///
/// `func(ctx)` 在主线程 wndproc 里被直接调用；`ctx` 指向的对象必须
/// 在本函数返回前保持有效（调用方持有）。
pub fn invoke_c(func: TaskFn, ctx: *mut c_void) -> Result<(), String> {
    let caller_tid = unsafe { GetCurrentThreadId() };
    let hwnd = main_hwnd().ok_or_else(|| "main window not ready".to_string())?;
    let done = Arc::new(AtomicBool::new(false));

    {
        let mut slot = PENDING_TASK.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            return Err("PENDING_TASK busy — a previous invocation has not completed".into());
        }
        *slot = Some(PendingTask {
            func,
            ctx,
            done: done.clone(),
        });
    }

    super::diag::write(&format!(
        "invoke_c: caller_tid={} hwnd=0x{:x}",
        caller_tid, hwnd
    ));

    // PostMessageW 是内核调用，天然构成内存屏障：主线程一定能看到槽位写入。
    let posted = unsafe { PostMessageW(hwnd as HWND, WM_INVOKE, 0, 0) };
    if posted == 0 {
        // 投递失败：清空槽位，避免残留任务被下一次调用误执行。
        let mut slot = PENDING_TASK.lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
        return Err("PostMessageW failed".to_string());
    }

    // 忙等主线程完成。db_query_search2 是异步提交（很快返回），
    // read_results 读取本地内存也很快；给足上界防死锁。
    let mut spins: u64 = 0;
    while !done.load(Ordering::SeqCst) {
        spins += 1;
        if spins > 2_000_000_000 {
            return Err("invoke_c timeout".to_string());
        }
        std::hint::spin_loop();
    }
    Ok(())
}

/// 主线程窗口的 wndproc。
///
/// 注意签名：host 注册的 wndproc 期望标准 Win32 4-参数签名。
/// 即便我们是通过 host 的 os_register_class 注册的，wndproc 本身仍然是
/// 一个普通的 Win32 WNDPROC。
pub unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INVOKE {
        let cur_tid = unsafe { GetCurrentThreadId() };
        let install_tid = INSTALL_TID.load(Ordering::SeqCst);
        super::diag::write(&format!(
            "wnd_proc: WM_INVOKE cur_tid={} install_tid={}",
            cur_tid, install_tid
        ));

        // 取出任务（Take 语义：取出即清空槽位）。
        let task = {
            let mut slot = PENDING_TASK.lock().unwrap_or_else(|p| p.into_inner());
            slot.take()
        };

        if let Some(t) = task {
            super::diag::write_flush("wnd_proc: executing task on main thread...");
            unsafe { (t.func)(t.ctx) };
            super::diag::write_flush("wnd_proc: task completed");
            // 先置位完成标志，再让调用方解除阻塞 —— 顺序不能反，
            // 否则调用方可能提前返回并释放 ctx 指向的内存。
            t.done.store(true, Ordering::SeqCst);
        } else {
            super::diag::write("wnd_proc: PENDING_TASK empty");
        }
        return 1;
    }
    // 其它消息交给 DefWindowProcW。我们只关心 WM_INVOKE。
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(
            hwnd, msg, wparam, lparam,
        )
    }
}

/// 在 PM_START 时调用一次 —— 必须在 Everything 主线程上！
///
/// 通过主程序提供的 `os_register_class` + `os_create_window` 注册窗口类并
/// 创建窗口。主消息泵只会处理它自己创建过的窗口的消息。
pub fn install_on_main_thread() -> Result<(), String> {
    let install_tid = unsafe { GetCurrentThreadId() };
    INSTALL_TID.store(install_tid, Ordering::SeqCst);

    let host = Host::get();

    let register = host
        .os_register_class
        .ok_or("os_register_class not available")?;
    let create_window = host
        .os_create_window
        .ok_or("os_create_window not available")?;

    // 注册窗口类。
    // 注意：os_register_class 期望 UTF-8 字符串类名（内部自行转 UTF-16）。
    unsafe {
        register(
            0, // style
            CLASS_NAME.as_ptr(),
            // host 的 WNDPROC 类型与我们的 wnd_proc 函数签名一致 —— 直接传 Some。
            Some(wnd_proc),
            0,                  // window_extra
            0 as HICON,         // hIcon
            0 as HICON,         // hIconSm
            0 as HCURSOR,       // hcursor
        );
    }

    // 创建隐藏窗口。
    // dwStyle=0, hWndParent=0 创建一个不可见窗口（不指定 WS_OVERLAPPED 等
    // 可见样式即可）。etp_server.c 行 2824-2828 用的也是相同模式。
    let hwnd: HostHwnd = unsafe {
        create_window(
            0, // dwExStyle
            CLASS_NAME.as_ptr(),
            b"\0".as_ptr(), // lpWindowName —— 空字符串
            0,              // dwStyle
            0,              // x
            0,              // y
            0,              // nWidth
            0,              // nHeight
            0 as HostHwnd,  // hWndParent
            0 as HMENU,     // hMenu
            0 as HINSTANCE, // hInstance
            core::ptr::null_mut(), // lpParam
        )
    };
    if hwnd.is_null() {
        return Err("os_create_window returned NULL".to_string());
    }
    let _ = MAIN_HWND.set(hwnd as isize);
    super::diag::write(&format!(
        "main_thread: host-window installed hwnd=0x{:x} tid={}",
        hwnd as usize, install_tid
    ));
    Ok(())
}
