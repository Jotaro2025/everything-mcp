//! Apply Everything core settings after its options dialog has closed.
//!
//! Never wait for a second Everything process on the host UI thread. The
//! forwarder needs that thread to receive its command. Keep its input file
//! alive until it exits, and never treat our own INI write as host confirmation.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

#[derive(Default)]
struct ApplyState {
    pending: Option<(bool, isize)>,
    active: Option<bool>,
    error: Option<String>,
    worker: Option<JoinHandle<()>>,
    stop: Option<Arc<AtomicBool>>,
}

static STATE: Mutex<ApplyState> = Mutex::new(ApplyState {
    pending: None,
    active: None,
    error: None,
    worker: None,
    stop: None,
});

pub fn status() -> (Option<bool>, Option<String>) {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    (
        state.pending.map(|p| p.0).or(state.active),
        state.error.clone(),
    )
}

pub fn schedule(enabled: bool, options_window: isize) -> Result<(), String> {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.worker.is_none() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        state.worker = Some(
            std::thread::Builder::new()
                .name("everything-content-setting".into())
                .spawn(move || worker(worker_stop))
                .map_err(|e| format!("start content-setting worker: {e}"))?,
        );
        state.stop = Some(stop);
    }
    state.pending = Some((enabled, options_window));
    state.error = None;
    Ok(())
}

pub fn stop() {
    let worker = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stop) = state.stop.take() {
            stop.store(true, Ordering::Release);
        }
        state.pending = None;
        state.worker.take()
    };
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    STATE.lock().unwrap_or_else(|e| e.into_inner()).active = None;
}

fn worker(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        let request = {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((enabled, window)) = state.pending {
                if unsafe { IsWindow(window as _) } == 0 {
                    state.pending = None;
                    state.active = Some(enabled);
                    Some(enabled)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(enabled) = request {
            let result = apply(enabled, &stop);
            match &result {
                Ok(()) => crate::plugin::diag::write(&format!(
                    "options: content_indexing set to {} (forwarder completed; stable readback)",
                    enabled as i32
                )),
                Err(error) => crate::plugin::diag::write(&format!(
                    "options: content_indexing apply failed: {error}"
                )),
            }
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.active = None;
            state.error = result.err();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

unsafe extern "system" fn release_query(_: *mut core::ffi::c_void) {
    // The worker holds HOST_LOCK, so no search owns these resources.
    unsafe { crate::plugin::state::release_query() };
}

fn apply(enabled: bool, stop: &AtomicBool) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let _guard = loop {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err("content-setting application cancelled or busy".into());
        }
        match crate::plugin::host::HOST_LOCK.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    // A core-index change may rebuild the DB. Drop the idle query/ref first,
    // while keeping subsequent search submissions serialized behind this apply.
    crate::plugin::main_thread::invoke_until(release_query, core::ptr::null_mut(), deadline)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = std::env::temp_dir().join(format!(
        "everything_mcp_content_indexing_{}.ini",
        std::process::id()
    ));
    let body = format!(
        "[Everything]\r\ncontent_indexing_enabled={}\r\n",
        enabled as i32
    );
    run_forwarder(&exe, file, body.as_bytes(), stop)?;
    wait_for_readback(enabled, stop, crate::options::content_indexing_enabled)
}

fn run_forwarder(
    exe: &std::path::Path,
    file: PathBuf,
    body: &[u8],
    stop: &AtomicBool,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(exe);
    crate::host_context::HostContext::current().target_command(&mut command);
    command
        .arg("-add-config")
        .arg(&file)
        .creation_flags(0x0800_0000);
    run_with_input(command, file, body, stop, Duration::from_secs(15))
}

fn run_with_input(
    mut command: Command,
    file: PathBuf,
    body: &[u8],
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<(), String> {
    std::fs::write(&file, body).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut child = command
            .spawn()
            .map_err(|e| format!("spawn forwarder: {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(format!("configuration forwarder failed: {status}"))
                    }
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.to_string());
                }
                Ok(None) => {}
            }
            if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("configuration forwarder cancelled or timed out".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    let _ = std::fs::remove_file(file);
    result
}

fn wait_for_readback(
    enabled: bool,
    stop: &AtomicBool,
    mut read: impl FnMut() -> Option<bool>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stable = None;
    loop {
        if stop.load(Ordering::Acquire) {
            return Err("content-setting verification cancelled".into());
        }
        if read() == Some(enabled) {
            let since = stable.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_millis(500) {
                return Ok(());
            }
        } else {
            stable = None;
        }
        if Instant::now() >= deadline {
            return Err(
                "content-indexing value did not persist; retry from Everything Advanced settings"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_rejects_a_transient_match() {
        let mut calls = 0;
        let result = wait_for_readback(false, &AtomicBool::new(false), || {
            calls += 1;
            Some(calls > 2)
        });
        assert!(result.is_err());
    }

    #[test]
    fn forwarder_input_survives_until_child_exit() {
        use std::os::windows::process::CommandExt;
        if let Some(input) = std::env::var_os("EMCP_TEST_INPUT") {
            std::thread::sleep(Duration::from_millis(1200));
            assert!(std::path::Path::new(&input).is_file());
            return;
        }
        let file =
            std::env::temp_dir().join(format!("emcp-forwarder-test-{}.ini", std::process::id()));
        // Run this native test executable as the delayed reader. PowerShell's
        // first startup on a fresh CI runner can exceed the forwarder timeout.
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "content_indexing::tests::forwarder_input_survives_until_child_exit",
            ])
            .env("EMCP_TEST_INPUT", &file)
            .creation_flags(0x0800_0000);
        let result = run_with_input(
            command,
            file.clone(),
            b"fixture",
            &AtomicBool::new(false),
            Duration::from_secs(30),
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(!file.exists());
    }
}
