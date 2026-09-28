//! diag.rs — 诊断日志（写入磁盘）
//!
//! 用途：插件加载阶段的诊断信息（PM_INIT 时哪些 host 函数解析失败、
//! PM_START 时 db/query 是否创建成功等）需要立即知晓，
//! 但 OutputDebugString 需要外部 DebugView 才能看见 ——
//! 因此同时写一份到 %LOCALAPPDATA%\everything-mcp\plugin.log。
//!
//! 实现完全独立于 host API，纯 std::fs，避免对 host 的循环依赖。
//!
//! 两条通道：
//!   - [`write`]：生命周期与异常日志，始终落盘。每次插件启停只有几条，
//!     排查「插件为什么没起来」靠它。
//!   - [`verbose`]：热路径日志（每次 MCP 调用 / 每次搜索都写几条），
//!     默认丢弃。每条都是一次「打开-写入-落盘」，还落在 Everything 的
//!     UI 线程上；排查崩溃或主程序消息序列时，设环境变量
//!     `EVERYTHING_MCP_DIAG=1`（任何非 0 非空值）即可恢复全量落盘，
//!     进程内不刷新，需重启 Everything 生效。
//!
//! 文件超过 [`LOG_MAX_BYTES`] 时就地清空重来（每 128 次写检查一次大小，
//! 不逐条 stat）：诊断日志只关心最近发生了什么，不做轮转。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// plugin.log 的就地重置阈值。
const LOG_MAX_BYTES: u64 = 1 << 20; // 1 MiB

/// 热路径开关。OnceLock 保证环境变量只读一次。
static VERBOSE: OnceLock<bool> = OnceLock::new();

fn verbose_enabled() -> bool {
    *VERBOSE.get_or_init(|| {
        std::env::var("EVERYTHING_MCP_DIAG")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

/// 热路径日志：默认丢弃，`EVERYTHING_MCP_DIAG=1` 时与 [`write`] 等价。
pub fn verbose(line: &str) {
    if verbose_enabled() {
        write(line);
    }
}

fn log_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        let dir = format!("{}\\everything-mcp", base);
        let _ = std::fs::create_dir_all(&dir);
        format!("{}\\plugin.log", dir)
    })
    .as_str()
}

static WRITES: AtomicU64 = AtomicU64::new(0);

pub fn write(line: &str) {
    let _g = LOG_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = log_path();
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    if n.is_multiple_of(128)
        && std::fs::metadata(path)
            .map(|m| m.len() > LOG_MAX_BYTES)
            .unwrap_or(false)
    {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
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
