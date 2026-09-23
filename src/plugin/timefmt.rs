//! timefmt.rs — 文件时间戳格式化
//!
//! Everything 的 `fileinfo_fd_t` 用 Windows FILETIME 表示时间：自 1601-01-01
//! (UTC) 起的 100 纳秒计数。搜索结果直接回给 LLM，原始 u64 既不可读也无法比较，
//! 因此统一转成 ISO 8601 UTC 字符串（`2026-09-23T11:18:31Z`）—— 客户端一眼能读，
//! 字典序比较也等于时间先后。

/// Unix 纪元与 FILETIME 纪元（1601-01-01）之间的秒差。
const FILETIME_UNIX_EPOCH_DIFF_SECS: i64 = 11_644_473_600;

/// Windows FILETIME（100ns 自 1601-01-01 UTC）→ ISO 8601 UTC 字符串。
///
/// 返回 `None` 的情形：`ft == 0`（索引项没有该时间，如未索引的文件夹），
/// 或换算后早于 Unix 纪元（Everything 不会产出这种值，防御性处理）。
pub fn filetime_to_iso(ft: u64) -> Option<String> {
    if ft == 0 {
        return None;
    }
    // 100ns → 秒；先减纪元差再判断符号，避免中间值溢出 i64 的极端情况。
    let secs_since_1601 = (ft / 10_000_000) as i128;
    let unix = secs_since_1601 - FILETIME_UNIX_EPOCH_DIFF_SECS as i128;
    if unix < 0 {
        return None;
    }
    let unix = unix as i64;

    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    // 合理范围之外的年份一律视为「没有该时间」。Everything 对未索引的
    // date_created 用全 1（u64::MAX）填充，直接换算会得到公元六万年这种
    // 垃圾值；文件时间不可能落在 [1601, 9999] 之外，越界即哨兵。
    if !(1601..=9999).contains(&y) {
        return None;
    }
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hh, mm, ss
    ))
}

/// 自 Unix 纪元起的天数 → (年, 月, 日)。Howard Hinnant 的 `civil_from_days`
/// 算法（proleptic Gregorian），只用整数运算，无闰年特判。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 由 ISO 字符串反推的期望值用「已知时刻」硬编码，避免测试里再实现一遍
    /// 日历算法（那样两边一起错就测不出来）。
    #[test]
    fn unix_epoch_is_1970_01_01() {
        // 1970-01-01T00:00:00Z 对应的 FILETIME。
        let ft = (FILETIME_UNIX_EPOCH_DIFF_SECS as u64) * 10_000_000;
        assert_eq!(filetime_to_iso(ft).unwrap(), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_timestamps_round_trip() {
        // 2026-09-23T11:18:31Z。Unix 秒 = 20719 天 * 86400 + 40711。
        let unix_2026_09_23_111831: i64 = 1_790_162_311;
        let ft = ((unix_2026_09_23_111831 + FILETIME_UNIX_EPOCH_DIFF_SECS) as u64) * 10_000_000;
        assert_eq!(filetime_to_iso(ft).unwrap(), "2026-09-23T11:18:31Z");

        // 闰年 2 月 29 日（2024-02-29T00:00:00Z）。Unix 秒 = 1709164800。
        let ft = ((1_709_164_800i64 + FILETIME_UNIX_EPOCH_DIFF_SECS) as u64) * 10_000_000;
        assert_eq!(filetime_to_iso(ft).unwrap(), "2024-02-29T00:00:00Z");

        // 世纪闰年边界 2000-03-01T00:00:00Z。Unix 秒 = 951868800。
        let ft = ((951_868_800i64 + FILETIME_UNIX_EPOCH_DIFF_SECS) as u64) * 10_000_000;
        assert_eq!(filetime_to_iso(ft).unwrap(), "2000-03-01T00:00:00Z");
    }

    #[test]
    fn zero_and_pre_epoch_are_none() {
        assert_eq!(filetime_to_iso(0), None);
        // 1601-01-01 本身（FILETIME 起点）早于 Unix 纪元。
        assert_eq!(filetime_to_iso(1), None);
    }

    #[test]
    fn all_ones_sentinel_is_none() {
        // Everything 对未索引的 date_created 填 u64::MAX（实测 build.rs 等
        // 文件的 created 就是这个值）。必须当成「没有时间」，不能换算成
        // 公元六万年。
        assert_eq!(filetime_to_iso(u64::MAX), None);
        // 接近上限的值同样越界。
        assert_eq!(filetime_to_iso(u64::MAX - 1), None);
    }

    #[test]
    fn sub_second_precision_is_truncated_not_rounded() {
        let ft = (FILETIME_UNIX_EPOCH_DIFF_SECS as u64) * 10_000_000 + 9_999_999; // +0.9999999s
        assert_eq!(filetime_to_iso(ft).unwrap(), "1970-01-01T00:00:00Z");
    }
}
