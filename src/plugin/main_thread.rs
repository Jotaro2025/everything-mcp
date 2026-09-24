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
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentThreadId, Sleep};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyWindow, IsWindow, PostMessageW};

use super::host::{Host, HCURSOR, HICON, HINSTANCE, HMENU, HWND as HostHwnd};

/// 自定义消息：把一个工作线程的 invocation 投递到主线程上执行。
/// 用 WM_USER+4 —— 与 etp_server 同一消息段（WM_USER..WM_USER+3 已被它占用）。
pub const WM_INVOKE: u32 = 0x0400 + 4; // WM_USER+4

/// 进程级主线程窗口句柄 —— 一旦 PM_START 注册成功就有效。
/// 0 表示未安装。用原子值而非 OnceLock：stop/start 周期里要能销毁并重建。
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// install_on_main_thread 调用时的线程 ID —— 用于诊断 wnd_proc 是否真的
/// 在主线程上被调度（PostMessage 投递的消息由创建窗口的线程的消息泵处理）。
static INSTALL_TID: AtomicU32 = AtomicU32::new(0);

/// invoke_c 等待主线程完成的超时（毫秒）。
///
/// 超时不等于可以释放 ctx —— 主线程可能正拿着它执行。能否安全返回错误
/// 由 try_reclaim 判断（见其文档）；超时值本身只是把卡死变成可诊断的错误。
const INVOKE_TIMEOUT_MS: u32 = 30_000;

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
    let h = MAIN_HWND.load(Ordering::Acquire);
    if h == 0 {
        None
    } else {
        Some(h)
    }
}

/// 尝试取回槽位里的任务。
///
/// 只有当能**证明**主线程不会（再）执行我们的任务时，才取回并清空槽位：
///
///  1. 槽位里仍然是我们这次提交的任务 —— 主线程还没取走它，取回即安全；
///  2. 槽位已空、且 `hwnd` 指向的窗口已经销毁 —— 消息永远到不了主线程，
///     任务不可能再被执行（销毁窗口发生在主线程，因此主线程不可能正卡在
///     某个 wndproc 里执行我们的任务）。
///
/// 其余情况（槽位空但窗口仍在）表示主线程已经取走任务、正在执行，此时
/// 返回 None：调用方必须继续等 `done`，绝不能就地释放 ctx。
///
/// 返回 Some(()) 表示调用方已重新独占该任务，可以安全释放自己的 ctx。
fn try_reclaim(done: &Arc<AtomicBool>, hwnd: isize) -> Option<()> {
    let mut slot = PENDING_TASK.lock().unwrap_or_else(|p| p.into_inner());
    match slot.as_ref() {
        // 还在槽位里 —— 主线程尚未取走。
        Some(t) if Arc::ptr_eq(&t.done, done) => {
            *slot = None;
            Some(())
        }
        // 槽位里是别的任务 —— 说明我们那次已经被取走在执行。
        Some(_) => None,
        // 槽位为空。只在我们自己的窗口已销毁时才能断定任务已被丢弃；
        // 窗口还活着就意味着主线程刚 take() 完、正在跑。
        None => {
            let alive = unsafe { IsWindow(hwnd as HWND) } != 0;
            if alive {
                None
            } else {
                Some(())
            }
        }
    }
}

/// 在主线程上执行一次 C 风格任务，阻塞直到完成。
///
/// `func(ctx)` 在主线程 wndproc 里被直接调用；`ctx` 指向的对象必须
/// 在本函数返回前保持有效（调用方持有）。
pub fn invoke_c(func: TaskFn, ctx: *mut c_void) -> Result<(), String> {
    let caller_tid = unsafe { GetCurrentThreadId() };
    let done = Arc::new(AtomicBool::new(false));

    // 已经在主线程上：直接执行。否则 PostMessage 到自己的窗口要等消息泵
    // 取走这条消息才返回 —— 而消息泵此刻正卡在本调用里，必死锁。
    // （PM_START 期间注册窗口就是在主线程上跑的，这条路径可达。）
    if caller_tid == INSTALL_TID.load(Ordering::Acquire) {
        super::diag::write("invoke_c: already on main thread — running inline");
        unsafe { func(ctx) };
        return Ok(());
    }

    let hwnd = main_hwnd().ok_or_else(|| "main window not ready".to_string())?;

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
    // 每轮自旋后让出一次 CPU —— 纯自旋会把主线程饿死（本线程与主线程
    // 可能同优先级），反而更容易撞上超时。
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(INVOKE_TIMEOUT_MS as u64);
    let mut spins: u64 = 0;
    while !done.load(Ordering::SeqCst) {
        if start.elapsed() >= deadline {
            // 超时。关键：不能直接返回让调用方释放 ctx —— 主线程可能已经
            // 取走任务、此刻正在解引用 ctx。只有确认槽位里还是我们的任务
            // （主线程尚未取走）时才清槽位并返回错误。
            if try_reclaim(&done, hwnd).is_some() {
                super::diag::write("invoke_c: timeout, task reclaimed");
                return Err("invoke_c timeout".to_string());
            }
            // 主线程已经接手 —— 只能继续等它置位 done。此时返回会让调用方
            // 释放正在被执行的 ctx，比多等一会儿危险得多。
            super::diag::write("invoke_c: timeout but task may be running — waiting");
            while !done.load(Ordering::SeqCst) {
                // 槽位空 + 窗口已销毁 ⇒ 任务被丢弃、永远不会执行，
                // 这时才可以安全返回错误让调用方回收 ctx。
                if try_reclaim(&done, hwnd).is_some() {
                    super::diag::write("invoke_c: window gone, task discarded");
                    return Err("invoke_c aborted — main window destroyed".to_string());
                }
                unsafe { Sleep(1) };
            }
            return Ok(());
        }
        spins += 1;
        std::hint::spin_loop();
        if spins.is_multiple_of(4096) {
            unsafe { Sleep(0) };
        }
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
        windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

/// 在 PM_START 时调用一次 —— 必须在 Everything 主线程上！
///
/// 通过主程序提供的 `os_register_class` + `os_create_window` 注册窗口类并
/// 创建窗口。主消息泵只会处理它自己创建过的窗口的消息。
///
/// **幂等**：已安装过就直接返回 Ok，不会重复注册窗口类 / 重复建窗口
/// （选项对话框里停用再启用插件会走到这里两次）。
pub fn install_on_main_thread() -> Result<(), String> {
    if main_hwnd().is_some() {
        super::diag::write("install_on_main_thread: already installed");
        return Ok(());
    }

    let install_tid = unsafe { GetCurrentThreadId() };
    INSTALL_TID.store(install_tid, Ordering::Release);

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
            0,            // window_extra
            0 as HICON,   // hIcon
            0 as HICON,   // hIconSm
            0 as HCURSOR, // hcursor
        );
    }

    // 创建隐藏窗口。
    // dwStyle=0, hWndParent=0 创建一个不可见窗口（不指定 WS_OVERLAPPED 等
    // 可见样式即可）。etp_server.c 行 2824-2828 用的也是相同模式。
    //
    // hInstance 传 GetModuleHandle(0)（本 DLL 实例），与 etp_server.c:2828 /
    // http_server.c:3830 完全一致 —— 传 NULL 会让主程序在窗口类实例匹配时
    // 拿不到模块基址。
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(core::ptr::null()) as HINSTANCE };
    let hwnd: HostHwnd = unsafe {
        create_window(
            0, // dwExStyle
            CLASS_NAME.as_ptr(),
            c"".to_bytes().as_ptr(), // lpWindowName —— 空字符串（含结尾 NUL）
            0,                     // dwStyle
            0,                     // x
            0,                     // y
            0,                     // nWidth
            0,                     // nHeight
            0 as HostHwnd,         // hWndParent
            0 as HMENU,            // hMenu
            hinstance,             // hInstance
            core::ptr::null_mut(), // lpParam
        )
    };
    if hwnd.is_null() {
        return Err("os_create_window returned NULL".to_string());
    }
    MAIN_HWND.store(hwnd as isize, Ordering::Release);
    super::diag::write(&format!(
        "main_thread: host-window installed hwnd=0x{:x} tid={}",
        hwnd as usize, install_tid
    ));
    Ok(())
}

/// 在 PM_STOP/PM_KILL 时调用：销毁主线程窗口并清空句柄。
///
/// 与 etp_server.c:1171 / http_server.c:3944 一致 —— 官方插件在关闭时
/// 直接对本插件创建的窗口调 DestroyWindow。销毁后 invoke_c 会以
/// 「main window not ready」失败，直到下一次 PM_START 重建。
///
/// 幂等：句柄已为 0 时直接返回。
///
/// **刻意不清空 PENDING_TASK**：可能有 MCP 工作线程正带着任务在等。
/// 抢清槽位会让它误以为「任务已被主线程取走」而永久等 done。留着不动，
/// 它的 invoke_c 超时后会发现窗口已销毁、自行回收（见 try_reclaim）。
pub fn destroy_main_window() {
    let h = MAIN_HWND.swap(0, Ordering::AcqRel);
    if h == 0 {
        return;
    }
    super::diag::write(&format!("main_thread: destroying hwnd=0x{:x}", h));
    unsafe { DestroyWindow(h as HWND) };
}
