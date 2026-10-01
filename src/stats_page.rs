//! stats_page.rs — Everything 选项对话框里的 Statistics 只读页。
//!
//! 从 options.rs 拆出：设置页状态机留在 options.rs，本模块只负责统计页的
//! 控件创建、布局、每秒刷新（渲染缓存逐项 diff）与两步确认清空。
//!
//! 与 options.rs 的边界：页注册/回调分流按 `user_data` 哨兵指针判别
//! （[`is_stats_page`]），文案仍走 options.rs 的 Labels（中/英切换的唯一来源，
//! 见该模块说明）；布局辅助（set_rect / client_size_logical / expand_min_wide）
//! 两页共用，留在 options.rs。
//!
//! **线程约定**：本模块全部回调（load / size / page_proc / 定时器）都在
//! Everything 主线程上到达，渲染缓存与武装状态只在主线程访问。

use core::ffi::c_void;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::SetTimer;

use crate::abi_ok;
use crate::options::{
    cstr_bytes, expand_min_wide, labels, set_rect, client_size_logical, LoadOptionsPage,
    OptionsPageProc, DLG_BUTTON_HIGH, DLG_STATIC_HIGH, SS_LEFTNOWORDWRAP, SS_RIGHT, WM_COMMAND,
    WS_GROUP,
};
use crate::plugin::diag;
use crate::plugin::host::Host;

// ============================================================
// 页标识与控件 ID
// ============================================================

/// Statistics 页的 `user_data` 哨兵指针 —— 注册时传入一个静态量地址，
/// 后续 load_page / size_page / page_proc / save_page / minmax / kill_page
/// 按回调结构体里的 `user_data` 字段分流到两套逻辑。`page_hwnd` 在
/// PM_KILL_OPTIONS_PAGE 里拿不到，`user_data` 是唯一可靠的页标识。
static STATS_PAGE_MARKER: u8 = 0;

pub(crate) fn is_stats_page(user_data: *mut c_void) -> bool {
    user_data == core::ptr::addr_of!(STATS_PAGE_MARKER) as *mut c_void
}

/// 注册页面时传给主程序的哨兵指针（options::add_page 使用）。
pub(crate) fn page_marker() -> *mut c_void {
    core::ptr::addr_of!(STATS_PAGE_MARKER) as *mut c_void
}

/// Statistics 页控件 ID（与 MCP 页不重叠，页面内唯一即可）。
const ID_STATS_TOTAL: i32 = 20;
const ID_STATS_CONNECTIONS: i32 = 21;
const ID_STATS_REQUESTS: i32 = 22;
const ID_STATS_CLEAR: i32 = 50;
const ID_STATS_HEADER_BASE: i32 = 60; // 60..=65 六列表头
const ID_STATS_CELL_BASE: i32 = 100; // 100..=153：9 行 × 6 列（行优先）

/// 统计页自动刷新定时器（SetTimer 的窗口级 ID）与周期（毫秒）。
/// 定时器挂在 page_hwnd 上，页面窗口销毁时 user32 自动回收。
const ID_STATS_TIMER: usize = 1;
const STATS_REFRESH_MS: u32 = 1000;

/// 统计表列数：0 = 工具名，1..=5 为五个数值列。
const STATS_COLS: usize = 6;

/// 5 个数值列的固定宽度（逻辑像素，右对齐）—— 列位定死后与字体度量
/// 无关；工具名列吃掉内容区剩余宽度。表头最长的「输出(KiB)」也放得下。
const STATS_NUM_COL_WIDE: [i32; 5] = [46, 40, 40, 54, 62];

/// 清空按钮「两步确认」的武装状态：点一次置位并把按钮文案翻成确认提示，
/// 再点才真正清空。每次重新加载页面时复位 —— 按钮控件是新建的，武装
/// 状态不能跨对话框会话残留（否则下次打开后第一次点击就会直接清空）。
static STATS_CLEAR_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Statistics 页最小尺寸（逻辑像素）。内容区 = 数值列 242 + 工具名列 ~130
/// + 24 边距；比 MCP 页宽 —— 逐列布局要给最长工具名留出不挤的宽度。
pub(crate) const STATS_PAGE_MIN_WIDE: i32 = 400;
pub(crate) const STATS_PAGE_MIN_HIGH: i32 = 240;

// ============================================================
// 渲染缓存与布局
// ============================================================

/// 上一次实际推送到统计页的数值（渲染缓存）。刷新时逐项 diff：没变的
/// 汇总行/数据行不再重复 SetDlgItemText —— 典型一次工具调用只动一行
/// 计数，其余 40+ 格连格式化都跳过。仅在主线程访问（load/定时器回调/
/// WM_COMMAND 都在主线程），锁无竞争。名字列每会话固定，不入缓存；
/// 按钮文案不走缓存（两步确认绕过这里直接改按钮）。
#[derive(Clone, Copy, PartialEq)]
struct StatsRenderCache {
    total_calls: u64,
    connections: u64,
    requests_total: u64,
    /// 每行数值列 [calls, ok, err, total_ms, bytes_out]。
    rows: [[u64; 5]; crate::plugin::stats::TOOL_SLOT_COUNT],
}

static LAST_STATS_RENDER: Mutex<Option<StatsRenderCache>> = Mutex::new(None);

/// 从快照算出渲染缓存。load 播种与刷新 diff 共用同一映射，防止两处
/// 手写映射漂移。
fn render_cache_from(snap: &crate::plugin::stats::StatsSnapshot) -> StatsRenderCache {
    let mut cache = StatsRenderCache {
        total_calls: snap.tools.iter().map(|t| t.calls).sum(),
        connections: snap.global.connections,
        requests_total: snap.global.requests_total,
        rows: [[0; 5]; crate::plugin::stats::TOOL_SLOT_COUNT],
    };
    for (i, t) in snap.tools.iter().enumerate() {
        cache.rows[i] = [t.calls, t.ok, t.err, t.total_ms, t.bytes_out];
    }
    cache
}

/// 行优先单元格 ID：`100 + row*6 + col`（col 0 = 工具名，1..=5 数值列）。
fn stats_cell_id(row: usize, col: usize) -> i32 {
    ID_STATS_CELL_BASE + (row * STATS_COLS + col) as i32
}

/// 一行的 6 个显示值：[工具名, 调用, 成功, 出错, 平均 ms, KiB]。
/// 只出「值」，对齐交给控件 rect + SS_RIGHT —— 不再用空格填充凑列。
fn stats_row_cells(s: &crate::plugin::stats::ToolSnapshot, name: &str) -> [String; STATS_COLS] {
    [
        name.to_string(),
        s.calls.to_string(),
        s.ok.to_string(),
        s.err.to_string(),
        s.avg_ms().to_string(),
        (s.bytes_out / 1024).to_string(),
    ]
}

/// 6 列的 `(x, wide)`：工具名列吃剩余宽度，5 个数值列固定宽。
/// 表头与 8 行数据共用同一组 rect，列自然对齐。
fn stats_column_rects(x: i32, content_wide: i32) -> [(i32, i32); STATS_COLS] {
    let num_total: i32 = STATS_NUM_COL_WIDE.iter().sum();
    let name_wide = (content_wide - num_total).max(60);
    let mut rects = [(0, 0); STATS_COLS];
    rects[0] = (x, name_wide);
    let mut cx = x + name_wide;
    for (i, w) in STATS_NUM_COL_WIDE.iter().enumerate() {
        rects[i + 1] = (cx, *w);
        cx += w;
    }
    rects
}

// ============================================================
// 页面生命周期回调（options.rs 的分流终点）
// ============================================================

/// 创建 Statistics 页控件：汇总行 + 表头 + 9 行工具明细（含未知兜底行）+ 清空按钮。
pub(crate) fn load_stats_page(page: &LoadOptionsPage) -> *mut c_void {
    // 重进页面 = 新按钮新文案，确认状态从「未武装」开始。
    STATS_CLEAR_ARMED.store(false, std::sync::atomic::Ordering::Relaxed);
    let host = Host::get();
    let create_static = match host.os_create_static {
        Some(f) => f,
        None => return core::ptr::null_mut(),
    };
    let create_button = match host.os_create_button {
        Some(f) => f,
        None => return core::ptr::null_mut(),
    };
    let add_tooltip = host.os_add_tooltip;

    let labels = labels();
    let snap = crate::plugin::stats::snapshot();

    let total_calls: u64 = snap.tools.iter().map(|t| t.calls).sum();
    let total_text = format!("{} {}", labels.stats_total, total_calls);
    let conn_text = format!("{} {}", labels.stats_connections, snap.global.connections);
    let req_text = format!("{} {}", labels.stats_requests, snap.global.requests_total);

    let page_hwnd = page.page_hwnd;
    unsafe {
        create_static(
            page_hwnd,
            ID_STATS_TOTAL,
            SS_LEFTNOWORDWRAP | WS_GROUP,
            cstr_bytes(&total_text).as_ptr(),
        );
        create_static(
            page_hwnd,
            ID_STATS_CONNECTIONS,
            SS_LEFTNOWORDWRAP,
            cstr_bytes(&conn_text).as_ptr(),
        );
        create_static(
            page_hwnd,
            ID_STATS_REQUESTS,
            SS_LEFTNOWORDWRAP,
            cstr_bytes(&req_text).as_ptr(),
        );
        // 表头与数据同列同对齐（数值列右对齐）。列 x 由 size_stats_page
        // 按固定像素摆位 —— 比例字体下空格填充的列宽每行不同，会参差。
        let heads = [
            labels.stats_col_tool,
            labels.stats_col_calls,
            labels.stats_col_ok,
            labels.stats_col_err,
            labels.stats_col_avg,
            labels.stats_col_out,
        ];
        for (c, text) in heads.iter().enumerate() {
            let style = if c == 0 {
                SS_LEFTNOWORDWRAP | WS_GROUP
            } else {
                SS_RIGHT
            };
            create_static(
                page_hwnd,
                ID_STATS_HEADER_BASE + c as i32,
                style,
                cstr_bytes(text).as_ptr(),
            );
        }
        // 9 行 = 8 个真实工具 + 1 个未知工具兜底行。兜底行也要显示：
        // 总调用次数对所有 9 个槽位求和，少画一行总数就对不上。
        for i in 0..crate::plugin::stats::TOOL_SLOT_COUNT {
            let name = crate::plugin::stats::TOOL_NAMES
                .get(i)
                .copied()
                .unwrap_or(labels.stats_unknown_tool);
            let cells = stats_row_cells(&snap.tools[i], name);
            for (c, text) in cells.iter().enumerate() {
                let style = if c == 0 {
                    SS_LEFTNOWORDWRAP
                } else {
                    SS_RIGHT
                };
                create_static(
                    page_hwnd,
                    stats_cell_id(i, c),
                    style,
                    cstr_bytes(text).as_ptr(),
                );
            }
        }
        create_button(
            page_hwnd,
            ID_STATS_CLEAR,
            WS_GROUP,
            cstr_bytes(labels.stats_clear).as_ptr(),
        );
        if let Some(tt) = add_tooltip {
            tt(
                page.tooltip_hwnd,
                page_hwnd,
                ID_STATS_CLEAR,
                cstr_bytes(labels.stats_clear_help).as_ptr(),
            );
        }
    }
    // 播种渲染缓存：控件初值就来自这份快照，第一轮定时器刷新无需重推。
    *LAST_STATS_RENDER.lock().unwrap_or_else(|p| p.into_inner()) = Some(render_cache_from(&snap));
    // 页面打开期间每秒刷一次文本（page_proc 只收到 WM_COMMAND，WM_TIMER
    // 走 TIMERPROC 回调直达，不依赖主程序转发）。窗口销毁时定时器自动回收。
    unsafe {
        SetTimer(page_hwnd, ID_STATS_TIMER, STATS_REFRESH_MS, Some(stats_timer_proc));
    }
    diag::write("options: stats page loaded");
    abi_ok()
}

/// 统计页的定时器回调：只刷新数值文本，不动清空按钮 —— 按钮可能正处于
/// 「再点一次确认」的两步确认文案，被定时器盖掉会跟武装状态不一致。
/// 计数代次没变就整轮跳过，空闲时不做无谓的 SetDlgItemText。
unsafe extern "system" fn stats_timer_proc(hwnd: HWND, _msg: u32, _id: usize, _dwtime: u32) {
    static LAST_REFRESH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let gen = crate::plugin::stats::generation();
    if gen == LAST_REFRESH_GEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    LAST_REFRESH_GEN.store(gen, std::sync::atomic::Ordering::Relaxed);
    refresh_stats_page_text(hwnd, false);
}

/// Statistics 页布局：顶部汇总三行 + 表头 + 9 行工具（含兜底行）+ 底部清空按钮。
pub(crate) fn size_stats_page(page_hwnd: HWND) -> *mut c_void {
    let (mut wide, mut high) = client_size_logical(page_hwnd);
    let x = 12;
    let mut y = 12;
    wide -= 24;
    high -= 24;

    let row_high = DLG_STATIC_HIGH;
    let sep = 3;

    set_rect(page_hwnd, ID_STATS_TOTAL, x, y, wide, row_high);
    y += row_high + sep;
    set_rect(page_hwnd, ID_STATS_CONNECTIONS, x, y, wide, row_high);
    y += row_high + sep;
    set_rect(page_hwnd, ID_STATS_REQUESTS, x, y, wide, row_high);
    y += row_high + sep + 3;

    // 6 列的 x/宽一次性算好，表头与 7 行共用 —— 与字体度量无关。
    let cols = stats_column_rects(x, wide);
    for (c, (cx, cw)) in cols.iter().enumerate() {
        set_rect(
            page_hwnd,
            ID_STATS_HEADER_BASE + c as i32,
            *cx,
            y,
            *cw,
            row_high,
        );
    }
    y += row_high + sep;
    for i in 0..crate::plugin::stats::TOOL_SLOT_COUNT {
        for (c, (cx, cw)) in cols.iter().enumerate() {
            set_rect(page_hwnd, stats_cell_id(i, c), *cx, y, *cw, row_high);
        }
        y += row_high + sep;
    }

    let labels = labels();
    let button_wide = expand_min_wide(page_hwnd, labels.stats_clear, 75 - 24) + 24;
    set_rect(
        page_hwnd,
        ID_STATS_CLEAR,
        x + wide - button_wide,
        12 + high - DLG_BUTTON_HIGH,
        button_wide,
        DLG_BUTTON_HIGH,
    );
    abi_ok()
}

/// Statistics 页 WM_COMMAND：只处理清空按钮。
pub(crate) fn stats_page_proc(p: &OptionsPageProc) -> *mut c_void {
    if p.msg != WM_COMMAND {
        return abi_ok();
    }
    let id = (p.wparam & 0xffff) as i32;
    if id == ID_STATS_CLEAR {
        // 两步确认：点一次变「再点一次确认」，再点才真正清空。
        // SDK 的 ui_task_dialog_show 是 varargs，Rust FFI 不便调用，
        // 用按钮文案自翻转做轻量确认。
        let armed = STATS_CLEAR_ARMED.load(std::sync::atomic::Ordering::Relaxed);
        if !armed {
            STATS_CLEAR_ARMED.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(f) = Host::get().os_set_dlg_text {
                // 确认提示必须走 Labels —— Everything 界面语言可选，不能硬编码。
                let t = cstr_bytes(labels().stats_clear_confirm);
                unsafe { f(p.page_hwnd, ID_STATS_CLEAR, t.as_ptr()) };
            }
        } else {
            STATS_CLEAR_ARMED.store(false, std::sync::atomic::Ordering::Relaxed);
            crate::plugin::stats::reset();
            refresh_stats_page_text(p.page_hwnd, true);
        }
    }
    abi_ok()
}

/// 把 Statistics 页文本刷成当前快照。定时器每秒调一次（restore_button =
/// false）；清空统计后也调（restore_button = true，把确认文案翻回「清空统计」）。
///
/// 逐项 diff：只把相对上次实推有变化的汇总行/数据行推给控件（见
/// LAST_STATS_RENDER），没变的行连格式化都跳过 —— 典型一次工具调用只
/// 动一行计数 + 汇总行，其余 40+ 格保持沉默。
fn refresh_stats_page_text(page_hwnd: HWND, restore_button: bool) {
    let host = Host::get();
    let set_text = match host.os_set_dlg_text {
        Some(f) => f,
        None => return,
    };
    let labels = labels();
    let snap = crate::plugin::stats::snapshot();
    let cache = render_cache_from(&snap);

    let mut last = LAST_STATS_RENDER.lock().unwrap_or_else(|p| p.into_inner());
    let prev = *last;
    unsafe {
        // 汇总三行：逐行 diff。
        if prev.is_none_or(|p| p.total_calls != cache.total_calls) {
            let t = format!("{} {}", labels.stats_total, cache.total_calls);
            let b = cstr_bytes(&t);
            set_text(page_hwnd, ID_STATS_TOTAL, b.as_ptr());
        }
        if prev.is_none_or(|p| p.connections != cache.connections) {
            let t = format!("{} {}", labels.stats_connections, cache.connections);
            let b = cstr_bytes(&t);
            set_text(page_hwnd, ID_STATS_CONNECTIONS, b.as_ptr());
        }
        if prev.is_none_or(|p| p.requests_total != cache.requests_total) {
            let t = format!("{} {}", labels.stats_requests, cache.requests_total);
            let b = cstr_bytes(&t);
            set_text(page_hwnd, ID_STATS_REQUESTS, b.as_ptr());
        }
        // 9 行 = 8 真实工具 + 兜底行，与 load_stats_page 一一对应。
        // 整行 diff：数值没变的行跳过（名字列只在创建时写一次，永不变化）。
        for i in 0..crate::plugin::stats::TOOL_SLOT_COUNT {
            if prev.is_some_and(|p| p.rows[i] == cache.rows[i]) {
                continue;
            }
            let name = crate::plugin::stats::TOOL_NAMES
                .get(i)
                .copied()
                .unwrap_or(labels.stats_unknown_tool);
            let cells = stats_row_cells(&snap.tools[i], name);
            for (c, text) in cells.iter().enumerate() {
                let b = cstr_bytes(text);
                set_text(page_hwnd, stats_cell_id(i, c), b.as_ptr());
            }
        }
        // 按钮文案恢复（定时器刷新路径不动按钮，见 stats_timer_proc）。
        if restore_button {
            let b = cstr_bytes(labels.stats_clear);
            set_text(page_hwnd, ID_STATS_CLEAR, b.as_ptr());
        }
    }
    *last = Some(cache);
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_columns_are_contiguous_and_fit_content() {
        let content = 376; // 400 最小宽 - 24 边距
        let cols = stats_column_rects(12, content);
        // 名字列吃满剩余（376 - 242 数值列 = 134），数值列逐列相接、不重叠。
        assert_eq!(cols[0], (12, content - STATS_NUM_COL_WIDE.iter().sum::<i32>()));
        let mut cx = cols[0].0 + cols[0].1;
        for (i, w) in STATS_NUM_COL_WIDE.iter().enumerate() {
            assert_eq!(cols[i + 1], (cx, *w));
            cx += w;
        }
        assert_eq!(cx - 12, content, "列总宽必须恰好铺满内容区");
    }

    #[test]
    fn stats_cell_ids_are_unique_and_clear_of_other_controls() {
        let rows = crate::plugin::stats::TOOL_NAMES.len();
        let mut seen = std::collections::HashSet::new();
        for row in 0..rows {
            for col in 0..STATS_COLS {
                assert!(seen.insert(stats_cell_id(row, col)));
            }
        }
        // 与汇总行、清空按钮、表头的 ID 不相撞。
        for id in [ID_STATS_TOTAL, ID_STATS_CONNECTIONS, ID_STATS_REQUESTS, ID_STATS_CLEAR] {
            assert!(!seen.contains(&id));
        }
        for c in 0..STATS_COLS {
            assert!(!seen.contains(&(ID_STATS_HEADER_BASE + c as i32)));
        }
    }

    #[test]
    fn render_cache_maps_snapshot_values() {
        let zero = crate::plugin::stats::ToolSnapshot {
            calls: 0,
            ok: 0,
            err: 0,
            total_ms: 0,
            bytes_out: 0,
        };
        let mut tools = vec![zero; crate::plugin::stats::TOOL_SLOT_COUNT];
        tools[0] = crate::plugin::stats::ToolSnapshot {
            calls: 3,
            ok: 2,
            err: 1,
            total_ms: 30,
            bytes_out: 300,
        };
        tools[crate::plugin::stats::TOOL_SLOT_COUNT - 1] = crate::plugin::stats::ToolSnapshot {
            calls: 5,
            ok: 4,
            err: 1,
            total_ms: 50,
            bytes_out: 500,
        };
        let snap = crate::plugin::stats::StatsSnapshot {
            tools,
            global: crate::plugin::stats::GlobalSnapshot {
                connections: 7,
                requests_total: 9,
                first_seen_unix: 0,
            },
        };
        let cache = render_cache_from(&snap);
        // 总调用 = 全部槽位之和（含兜底行）。
        assert_eq!(cache.total_calls, 8);
        assert_eq!(cache.connections, 7);
        assert_eq!(cache.requests_total, 9);
        assert_eq!(cache.rows[0], [3, 2, 1, 30, 300]);
        assert_eq!(cache.rows[crate::plugin::stats::TOOL_SLOT_COUNT - 1], [5, 4, 1, 50, 500]);
        // 中间行保持零 —— diff 依赖「没动的行 rows 相等」来跳过推送。
        assert_eq!(cache.rows[1], [0, 0, 0, 0, 0]);
    }
}
