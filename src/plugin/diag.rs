//! diag.rs — 诊断日志（写入磁盘）
//!
//! 用途：插件加载阶段的诊断信息（PM_INIT 时哪些 host 函数解析失败、
//! PM_START 时 db/query 是否创建成功等）需要立即知晓，
//! 但 OutputDebugString 需要外部 DebugView 才能看见 ——
//! 因此同时写一份到 %LOCALAPPDATA%\everything-mcp\plugin.log。
//!
//! 实现完全独立于 host API，纯 std::fs，避免对 host 的循环依赖。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

static LOG_LOCK: Mutex<()> = Mutex::new(());

fn log_path() -> String {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let dir = format!("{}\\everything-mcp", base);
    let _ = std::fs::create_dir_all(&dir);
    format!("{}\\plugin.log", dir)
}

pub fn write(line: &str) {
    let _g = LOG_LOCK.lock();
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", now, line);
        let _ = f.flush();
    }
}

/// 与 write 相同，但显式 flush —— 用于紧贴潜在崩溃点的诊断输出，
/// 确保即使后续崩溃，已写入的内容也落盘。
pub fn write_flush(line: &str) {
    write(line);
}
