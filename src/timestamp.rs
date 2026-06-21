use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const NANOS_PER_SECOND: i128 = 1_000_000_000;
const PROBE_SECONDS: i64 = 1_700_000_001;
const PROBE_NANOS: u32 = 123_456_789;

static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 文件时间戳可稳定保存的最小粒度。
///
/// 数值越大代表精度越粗。比较两个端点的 mtime 时使用较粗粒度，
/// 避免目标文件系统无法保存源端亚秒部分时反复触发哈希或元数据更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampPrecision {
    quantum_nanos: u64,
}

impl TimestampPrecision {
    pub const NANOSECOND: Self = Self { quantum_nanos: 1 };
    pub const HUNDRED_NANOSECONDS: Self = Self { quantum_nanos: 100 };
    pub const MICROSECOND: Self = Self {
        quantum_nanos: 1_000,
    };
    pub const MILLISECOND: Self = Self {
        quantum_nanos: 1_000_000,
    };
    pub const TEN_MILLISECONDS: Self = Self {
        quantum_nanos: 10_000_000,
    };
    pub const SECOND: Self = Self {
        quantum_nanos: 1_000_000_000,
    };
    pub const TWO_SECONDS: Self = Self {
        quantum_nanos: 2_000_000_000,
    };

    /// 返回精度粒度，单位为纳秒。
    pub fn quantum_nanos(self) -> u64 {
        self.quantum_nanos
    }

    /// 选择两端中更粗的精度。
    pub fn coarsest(self, other: Self) -> Self {
        Self {
            quantum_nanos: self.quantum_nanos.max(other.quantum_nanos),
        }
    }
}

impl Default for TimestampPrecision {
    fn default() -> Self {
        Self::NANOSECOND
    }
}

/// 探测目标目录所在文件系统的 mtime 保存精度。
///
/// 探测只用于性能优化：失败时回退到纳秒精度，也就是保持旧的精确比较行为。
/// 调用方应只在允许写入目标目录的真实同步流程中使用，dry-run 和源目录扫描不应调用。
pub fn detect_timestamp_precision(root: &Path) -> TimestampPrecision {
    probe_timestamp_precision(root).unwrap_or_default()
}

/// 在指定精度下判断两个可选 mtime 是否等价。
pub fn times_equivalent(
    left: Option<SystemTime>,
    right: Option<SystemTime>,
    precision: TimestampPrecision,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            let left_nanos = system_time_to_unix_nanos(left);
            let right_nanos = system_time_to_unix_nanos(right);
            same_precision_bucket(left_nanos, right_nanos, precision)
        }
        (None, None) => true,
        _ => false,
    }
}

/// 将网络协议中的 Unix 秒/纳秒时间与本地 mtime 按指定精度比较。
pub fn time_matches_unix_parts(
    local: Option<SystemTime>,
    remote_secs: Option<i64>,
    remote_nanos: Option<u32>,
    precision: TimestampPrecision,
) -> bool {
    let Some(remote_secs) = remote_secs else {
        return local.is_none();
    };
    let Some(remote_nanos) = remote_nanos else {
        return local.is_none();
    };
    if remote_nanos >= NANOS_PER_SECOND as u32 {
        return false;
    }
    let Some(local) = local else {
        return false;
    };
    let local_nanos = system_time_to_unix_nanos(local);
    let remote_nanos = unix_parts_to_nanos(remote_secs, remote_nanos);

    same_precision_bucket(local_nanos, remote_nanos, precision)
}

fn probe_timestamp_precision(root: &Path) -> std::io::Result<TimestampPrecision> {
    let probe_path = unique_probe_path(root);
    let root_times = root_file_times(root);
    let result = probe_timestamp_precision_at(&probe_path);

    let _ = fs::remove_file(&probe_path);
    if let Some((atime, mtime)) = root_times {
        let _ = filetime::set_file_times(root, atime, mtime);
    }

    result
}

fn probe_timestamp_precision_at(path: &Path) -> std::io::Result<TimestampPrecision> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    drop(file);

    let requested = unix_parts_to_nanos(PROBE_SECONDS, PROBE_NANOS);
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(PROBE_SECONDS, PROBE_NANOS),
    )?;
    let actual = fs::metadata(path)?.modified()?;
    let actual = system_time_to_unix_nanos(actual);

    Ok(infer_precision_from_roundtrip(requested, actual))
}

fn root_file_times(root: &Path) -> Option<(filetime::FileTime, filetime::FileTime)> {
    let metadata = fs::metadata(root).ok()?;
    Some((
        filetime::FileTime::from_last_access_time(&metadata),
        filetime::FileTime::from_last_modification_time(&metadata),
    ))
}

fn unique_probe_path(root: &Path) -> PathBuf {
    let counter = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
    root.join(format!(
        ".fastsync.time-probe.{}.{}",
        std::process::id(),
        counter
    ))
}

fn infer_precision_from_roundtrip(requested: i128, actual: i128) -> TimestampPrecision {
    const CANDIDATES: [TimestampPrecision; 7] = [
        TimestampPrecision::NANOSECOND,
        TimestampPrecision::HUNDRED_NANOSECONDS,
        TimestampPrecision::MICROSECOND,
        TimestampPrecision::MILLISECOND,
        TimestampPrecision::TEN_MILLISECONDS,
        TimestampPrecision::SECOND,
        TimestampPrecision::TWO_SECONDS,
    ];

    CANDIDATES
        .into_iter()
        .find(|precision| same_precision_bucket(requested, actual, *precision))
        .unwrap_or_default()
}

fn same_precision_bucket(left: i128, right: i128, precision: TimestampPrecision) -> bool {
    let quantum = precision.quantum_nanos() as i128;
    left.div_euclid(quantum) == right.div_euclid(quantum)
}

fn system_time_to_unix_nanos(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration_to_nanos(duration),
        Err(error) => -duration_to_nanos(error.duration()),
    }
}

fn duration_to_nanos(duration: std::time::Duration) -> i128 {
    i128::from(duration.as_secs()) * NANOS_PER_SECOND + i128::from(duration.subsec_nanos())
}

fn unix_parts_to_nanos(seconds: i64, nanos: u32) -> i128 {
    i128::from(seconds) * NANOS_PER_SECOND + i128::from(nanos)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn after_epoch(seconds: u64, nanos: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(seconds, nanos)
    }

    #[test]
    fn millisecond_precision_ignores_sub_millisecond_differences() {
        let left = after_epoch(1_700_000_000, 123_456_789);
        let right = after_epoch(1_700_000_000, 123_000_000);

        assert!(times_equivalent(
            Some(left),
            Some(right),
            TimestampPrecision::MILLISECOND
        ));
        assert!(!times_equivalent(
            Some(left),
            Some(right),
            TimestampPrecision::MICROSECOND
        ));
    }

    #[test]
    fn second_precision_ignores_subsecond_differences() {
        let left = after_epoch(1_700_000_000, 999_999_999);
        let right = after_epoch(1_700_000_000, 0);

        assert!(times_equivalent(
            Some(left),
            Some(right),
            TimestampPrecision::SECOND
        ));
        assert!(!times_equivalent(
            Some(left),
            Some(right),
            TimestampPrecision::MILLISECOND
        ));
    }

    #[test]
    fn unix_parts_comparison_uses_requested_precision() {
        let local = after_epoch(1_700_000_000, 123_000_000);

        assert!(time_matches_unix_parts(
            Some(local),
            Some(1_700_000_000),
            Some(123_456_789),
            TimestampPrecision::MILLISECOND
        ));
        assert!(!time_matches_unix_parts(
            Some(local),
            Some(1_700_000_000),
            Some(123_456_789),
            TimestampPrecision::MICROSECOND
        ));
    }

    #[test]
    fn roundtrip_inference_selects_coarsest_needed_precision() {
        let requested = unix_parts_to_nanos(1_700_000_001, 123_456_789);
        let stored_as_millis = unix_parts_to_nanos(1_700_000_001, 123_000_000);
        let stored_as_seconds = unix_parts_to_nanos(1_700_000_001, 0);

        assert_eq!(
            infer_precision_from_roundtrip(requested, stored_as_millis),
            TimestampPrecision::MILLISECOND
        );
        assert_eq!(
            infer_precision_from_roundtrip(requested, stored_as_seconds),
            TimestampPrecision::SECOND
        );
    }

    #[test]
    fn coarsest_precision_keeps_the_larger_quantum() {
        assert_eq!(
            TimestampPrecision::MICROSECOND.coarsest(TimestampPrecision::MILLISECOND),
            TimestampPrecision::MILLISECOND
        );
    }
}
