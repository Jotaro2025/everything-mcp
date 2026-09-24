//! stats.rs — 每工具调用统计（原子计数 + 批量刷盘 JSONL）
//!
//! 用途：统计每个 MCP 工具的调用次数/成功/出错/耗时/响应字节数，
//! 以及全局连接数与总请求数，在 Everything 选项对话框的 Statistics 页签
//! 展示（见 options.rs）。
//!
//! 设计要点：
//!   - **纯计数，不碰 host**：本模块完全独立于 host API（同 diag.rs），
//!     纯 std::fs 落盘，避免循环依赖。统计路径绝不能在持 HOST_LOCK 的
//!     调用链上做任何阻塞操作 —— 但因为根本不碰 host，所以无所谓。
//!   - **原子累加 + 批量刷盘**：热路径上每 20 次调用或 60 秒（先到者）
//!     才 append 一行 JSONL 到磁盘，避免 diag.rs 那种每次 open/write/flush
//!     的开销。进程崩溃最多丢最近一小批（约几秒内），这是可接受的取舍。
//!   - **隐私**：统计绝不记录文件路径/搜索词/参数值 —— 只记工具名、
//!     计数、耗时、字节数、时间戳。
//!   - **fire-and-forget**：统计失败绝不能让工具调用失败，所有 I/O
//!     错误都吞掉（内部记到 diag 便于排查）。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;

/// 7 个工具的固定顺序。与 tools::dispatch 的 match 分支一一对应，
/// 顺序变了这里也得跟着改（否则 UI 显示会串行）。
pub const TOOL_NAMES: [&str; 7] = [
    "search_in_folder",
    "list_folder",
    "count",
    "index_changes",
    "read_file",
    "grep",
    "search_everywhere",
];

/// 未知工具名的兜底槽位 —— 不单列一行，防名字空间被撑爆。
/// 未知调用统一记到这个槽位（与 `tool_index` 的兜底逻辑对应）。
const UNKNOWN_TOOL_IDX: usize = 7;
const TOOL_SLOT_COUNT: usize = 8;

/// 批量刷盘阈值：每 N 次调用刷一次。
const FLUSH_CALL_THRESHOLD: u64 = 20;

/// 批量刷盘阈值：距上次刷盘超过 M 秒刷一次。
const FLUSH_SECS_THRESHOLD: u64 = 60;

/// 每工具统计（全部 AtomicU64 + Relaxed —— 纯单调计数，不需要更强内存序）。
struct ToolStat {
    calls: AtomicU64,
    ok: AtomicU64,
    err: AtomicU64,
    total_ms: AtomicU64,
    bytes_out: AtomicU64,
}

impl ToolStat {
    const fn new() -> Self {
        ToolStat {
            calls: AtomicU64::new(0),
            ok: AtomicU64::new(0),
            err: AtomicU64::new(0),
            total_ms: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        self.calls.store(0, Ordering::Relaxed);
        self.ok.store(0, Ordering::Relaxed);
        self.err.store(0, Ordering::Relaxed);
        self.total_ms.store(0, Ordering::Relaxed);
        self.bytes_out.store(0, Ordering::Relaxed);
    }

    fn snapshot(&self) -> ToolSnapshot {
        ToolSnapshot {
            calls: self.calls.load(Ordering::Relaxed),
            ok: self.ok.load(Ordering::Relaxed),
            err: self.err.load(Ordering::Relaxed),
            total_ms: self.total_ms.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
        }
    }
}

/// 单工具快照（给 UI 渲染用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSnapshot {
    pub calls: u64,
    pub ok: u64,
    pub err: u64,
    pub total_ms: u64,
    pub bytes_out: u64,
}

impl ToolSnapshot {
    /// 平均耗时（毫秒，整数除法）。0 次调用返回 0。
    pub fn avg_ms(&self) -> u64 {
        self.total_ms.checked_div(self.calls).unwrap_or(0)
    }
}

/// 全局统计。
struct GlobalStat {
    connections: AtomicU64,
    requests_total: AtomicU64,
    first_seen_unix: AtomicU64,
}

impl GlobalStat {
    const fn new() -> Self {
        GlobalStat {
            connections: AtomicU64::new(0),
            requests_total: AtomicU64::new(0),
            first_seen_unix: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        self.connections.store(0, Ordering::Relaxed);
        self.requests_total.store(0, Ordering::Relaxed);
        self.first_seen_unix.store(0, Ordering::Relaxed);
    }

    fn snapshot(&self) -> GlobalSnapshot {
        GlobalSnapshot {
            connections: self.connections.load(Ordering::Relaxed),
            requests_total: self.requests_total.load(Ordering::Relaxed),
            first_seen_unix: self.first_seen_unix.load(Ordering::Relaxed),
        }
    }
}

/// 全局快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalSnapshot {
    pub connections: u64,
    pub requests_total: u64,
    pub first_seen_unix: u64,
}

/// 完整快照（工具表 + 全局）。
#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    /// 长度 = TOOL_SLOT_COUNT（前 7 个是真实工具，最后一个是未知工具聚合）。
    pub tools: Vec<ToolSnapshot>,
    pub global: GlobalSnapshot,
}

// ====================================================================
// 全局状态
// ====================================================================

/// 8 个工具槽位（7 真实 + 1 未知兜底）。
static TOOL_STATS: [ToolStat; TOOL_SLOT_COUNT] = [
    ToolStat::new(),
    ToolStat::new(),
    ToolStat::new(),
    ToolStat::new(),
    ToolStat::new(),
    ToolStat::new(),
    ToolStat::new(),
    ToolStat::new(),
];

static GLOBAL: GlobalStat = GlobalStat::new();

/// 自 `record_call` 以来累计的调用次数（用于判断是否触发批量刷盘）。
static CALLS_SINCE_FLUSH: AtomicU64 = AtomicU64::new(0);

/// 上次刷盘时刻（Unix 秒，SystemTime）。0 表示从未刷过。
static LAST_FLUSH_UNIX: AtomicU64 = AtomicU64::new(0);

/// 刷盘文件锁（仿 diag.rs 的 LOG_LOCK）。
static STATS_FILE_LOCK: Mutex<()> = Mutex::new(());

// ====================================================================
// 记录 API
// ====================================================================

/// 工具名 → 槽位索引。未知工具统一映射到 `UNKNOWN_TOOL_IDX`。
pub fn tool_index(name: &str) -> usize {
    TOOL_NAMES
        .iter()
        .position(|&n| n == name)
        .unwrap_or(UNKNOWN_TOOL_IDX)
}

/// 记录一次工具调用（含成功/失败/耗时/响应字节数）。
/// 内部顺带判断是否触发批量刷盘。绝不 panic，绝不返回错误。
pub fn record_call(tool_idx: usize, ok: bool, ms: u64, bytes: u64) {
    let idx = tool_idx.min(TOOL_SLOT_COUNT - 1);
    let t = &TOOL_STATS[idx];
    t.calls.fetch_add(1, Ordering::Relaxed);
    if ok {
        t.ok.fetch_add(1, Ordering::Relaxed);
    } else {
        t.err.fetch_add(1, Ordering::Relaxed);
    }
    t.total_ms.fetch_add(ms, Ordering::Relaxed);
    t.bytes_out.fetch_add(bytes, Ordering::Relaxed);

    // 首次调用时记录 first_seen_unix（仅当尚未设置）。
    let now = now_unix();
    let _ = GLOBAL
        .first_seen_unix
        .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);

    // 批量刷盘判断：调用次数阈值 或 时间阈值，先到者触发。
    let calls = CALLS_SINCE_FLUSH.fetch_add(1, Ordering::Relaxed) + 1;
    let last = LAST_FLUSH_UNIX.load(Ordering::Relaxed);
    let due_by_count = calls >= FLUSH_CALL_THRESHOLD;
    let due_by_time = last != 0 && now.saturating_sub(last) >= FLUSH_SECS_THRESHOLD;
    let due_first = last == 0; // 首次调用立即刷一次，建立文件
    if due_by_count || due_by_time || due_first {
        flush();
    }
}

/// 记录一次新连接。
pub fn record_connection() {
    GLOBAL.connections.fetch_add(1, Ordering::Relaxed);
    // 首次连接时也建立 first_seen_unix。
    let now = now_unix();
    let _ = GLOBAL
        .first_seen_unix
        .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
}

/// 记录一次 JSON-RPC 请求（含 parse 失败的）。
pub fn record_request() {
    GLOBAL.requests_total.fetch_add(1, Ordering::Relaxed);
}

/// 取一份快照（用于 UI 渲染）。
pub fn snapshot() -> StatsSnapshot {
    StatsSnapshot {
        tools: TOOL_STATS.iter().map(|t| t.snapshot()).collect(),
        global: GLOBAL.snapshot(),
    }
}

/// 清零全部计数 + 删除/截断 stats.jsonl。
pub fn reset() {
    for t in &TOOL_STATS {
        t.reset();
    }
    GLOBAL.reset();
    CALLS_SINCE_FLUSH.store(0, Ordering::Relaxed);
    LAST_FLUSH_UNIX.store(0, Ordering::Relaxed);
    let _g = STATS_FILE_LOCK.lock();
    let _ = std::fs::remove_file(stats_path());
}

/// 判断 JSON-RPC 响应里这次 tools/call 是否算「成功」。
///
/// 三类结果：`Ok((_,false))` → result 无 isError 字段 → 成功；
/// `Ok((_,true))` → result.isError == true → 失败；
/// `Err(_)` → 响应是 error 对象（不是 result）→ 失败。
/// 无法判定时返回 None（调用方保守按失败记）。
pub fn dispatch_ok_flag(resp: &crate::mcp::protocol::JsonRpcMessage) -> Option<bool> {
    if resp.error.is_some() {
        return Some(false);
    }
    let result = resp.result.as_ref()?;
    match result.get("isError") {
        Some(v) => Some(!v.as_bool().unwrap_or(true)),
        None => Some(true),
    }
}

// ====================================================================
// 落盘（JSONL）
// ====================================================================

/// 真正刷盘（持文件锁 append 一行 JSONL）。任何错误都吞掉。
///
/// 这里用「快照全部计数器一次性 append」而不是「攒一批 record 后 append」：
/// 后者需要在内存里维护一个待写队列，复杂度高且容易漏刷。快照式刷盘更简单 ——
/// 每次把当前累计值写一行，UI 读历史时按 `tool` 聚合最后一条即可（同名多条时
/// 取最大 `ts` 那条代表最新累计）。批量阈值的作用是控制刷盘频率，不是控制
/// 单次写入量。
fn flush() {
    let _g = STATS_FILE_LOCK.lock();
    let path = stats_path();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let now = now_unix();
        for (idx, t) in TOOL_STATS.iter().enumerate() {
            let s = t.snapshot();
            if s.calls == 0 {
                continue;
            }
            let name = TOOL_NAMES.get(idx).copied().unwrap_or("_unknown");
            let line = json!({
                "ts": now,
                "tool": name,
                "calls": s.calls,
                "ok": s.ok,
                "err": s.err,
                "ms": s.total_ms,
                "bytes": s.bytes_out,
            });
            let _ = writeln!(f, "{}", line);
        }
        let _ = f.flush();
    }
    CALLS_SINCE_FLUSH.store(0, Ordering::Relaxed);
    LAST_FLUSH_UNIX.store(now_unix(), Ordering::Relaxed);
}

/// `%LOCALAPPDATA%\everything-mcp\stats.jsonl`。
fn stats_path() -> String {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let dir = format!("{}\\everything-mcp", base);
    let _ = std::fs::create_dir_all(&dir);
    format!("{}\\stats.jsonl", dir)
}

/// 当前 Unix 秒。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ====================================================================
// 启动时读历史（PM_START 调用，把 stats.jsonl 累计进内存原子量）
// ====================================================================

/// 把 stats.jsonl 里的历史记录累计进内存原子量。
///
/// 文件格式是「快照式」—— 每行是某工具在某个时刻的**累计**值
/// （`{ts, tool, calls, ok, err, ms, bytes}`），同名工具会有多行，
/// 取最后一条（最新累计）作为该工具的历史值。
/// 文件损坏/不存在就跳过（从 0 开始），不报错。
pub fn load_history() {
    let path = stats_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return,
    };
    // 每工具取最后一条（最新累计）；first_seen_unix 取全文件最小 ts。
    let mut latest: Vec<Option<serde_json::Value>> = vec![None; TOOL_SLOT_COUNT];
    let mut min_ts: u64 = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let name = v["tool"].as_str().unwrap_or("");
        let idx = tool_index(name);
        latest[idx] = Some(v.clone());
        if let Some(ts) = v["ts"].as_u64() {
            if ts != 0 && (min_ts == 0 || ts < min_ts) {
                min_ts = ts;
            }
        }
    }
    for (idx, v) in latest.into_iter().enumerate() {
        let v = match v {
            Some(v) => v,
            None => continue,
        };
        let t = &TOOL_STATS[idx];
        t.calls.store(v["calls"].as_u64().unwrap_or(0), Ordering::Relaxed);
        t.ok.store(v["ok"].as_u64().unwrap_or(0), Ordering::Relaxed);
        t.err.store(v["err"].as_u64().unwrap_or(0), Ordering::Relaxed);
        t.total_ms.store(v["ms"].as_u64().unwrap_or(0), Ordering::Relaxed);
        t.bytes_out
            .store(v["bytes"].as_u64().unwrap_or(0), Ordering::Relaxed);
    }
    if min_ts != 0 {
        GLOBAL.first_seen_unix.store(min_ts, Ordering::Relaxed);
    }
}

// ====================================================================
// 测试
// ====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_index_maps_known_and_unknown() {
        assert_eq!(tool_index("search_in_folder"), 0);
        assert_eq!(tool_index("list_folder"), 1);
        assert_eq!(tool_index("count"), 2);
        assert_eq!(tool_index("index_changes"), 3);
        assert_eq!(tool_index("read_file"), 4);
        assert_eq!(tool_index("grep"), 5);
        assert_eq!(tool_index("search_everywhere"), 6);
        assert_eq!(tool_index("bogus"), UNKNOWN_TOOL_IDX);
    }

    #[test]
    fn record_call_accumulates() {
        // 隔离：测试间不依赖全局状态的绝对值，只验证相对增量。
        let before = snapshot();
        record_call(0, true, 10, 100);
        record_call(0, false, 20, 200);
        let after = snapshot();
        assert_eq!(after.tools[0].calls, before.tools[0].calls + 2);
        assert_eq!(after.tools[0].ok, before.tools[0].ok + 1);
        assert_eq!(after.tools[0].err, before.tools[0].err + 1);
        assert_eq!(after.tools[0].total_ms, before.tools[0].total_ms + 30);
        assert_eq!(after.tools[0].bytes_out, before.tools[0].bytes_out + 300);
    }

    #[test]
    fn avg_ms_zero_calls_is_zero() {
        let s = ToolSnapshot {
            calls: 0,
            ok: 0,
            err: 0,
            total_ms: 0,
            bytes_out: 0,
        };
        assert_eq!(s.avg_ms(), 0);
    }

    #[test]
    fn avg_ms_divides() {
        let s = ToolSnapshot {
            calls: 4,
            ok: 4,
            err: 0,
            total_ms: 100,
            bytes_out: 0,
        };
        assert_eq!(s.avg_ms(), 25);
    }

    #[test]
    fn record_connection_and_request() {
        let before = snapshot();
        record_connection();
        record_request();
        record_request();
        let after = snapshot();
        assert_eq!(
            after.global.connections,
            before.global.connections + 1
        );
        assert_eq!(
            after.global.requests_total,
            before.global.requests_total + 2
        );
    }

    #[test]
    fn unknown_tool_goes_to_fallback_slot() {
        // 不能断言真实工具的计数不变——cargo test 多线程并行会同时
        // 累加真实工具槽位。只断言未知工具落到了 fallback 槽。
        let before = snapshot();
        record_call(tool_index("bogus"), true, 5, 50);
        let after = snapshot();
        assert_eq!(
            after.tools[UNKNOWN_TOOL_IDX].calls,
            before.tools[UNKNOWN_TOOL_IDX].calls + 1
        );
    }
}
