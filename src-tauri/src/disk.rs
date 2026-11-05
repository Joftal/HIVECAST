//! 磁盘守卫：录制会持续吃空间，宁可提前拦住，也不要把系统盘写满。
//!
//! 阈值取 2 GiB / 1 GiB：4 路 1080p 大约十几分钟就能吃满 2 GiB，
//! 留够让用户看到通知、清理磁盘的时间。

use std::path::Path;

/// 开录门槛：低于这个值直接拒绝开录。
pub const MIN_START_FREE: u64 = 2 * 1024 * 1024 * 1024;
/// 录制红线：低于这个值主动收尾停录。
pub const MIN_KEEP_FREE: u64 = 1024 * 1024 * 1024;

pub fn free_bytes(path: &Path) -> Option<u64> {
    fs4::available_space(path).ok()
}

pub fn human(free: u64) -> String {
    format!("{:.1} GB", free as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// 够不够开录。取不到容量时放行：网络盘 / 特殊文件系统不该阻塞录制。
pub fn can_start(path: &Path) -> bool {
    free_bytes(path).map(|free| free >= MIN_START_FREE).unwrap_or(true)
}

/// 是否已经踩到红线。
pub fn is_low(path: &Path) -> bool {
    free_bytes(path).map(|free| free < MIN_KEEP_FREE).unwrap_or(false)
}

/// 开录被拦时的说明文案（中文，直接进 UI 行内错误）。
pub fn start_blocked_message(path: &Path) -> String {
    match free_bytes(path) {
        Some(free) => format!(
            "磁盘剩余空间不足（{}），至少需要 {} 才能开录",
            human(free),
            human(MIN_START_FREE)
        ),
        None => "无法读取磁盘剩余空间，已拒绝开录".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_ordered() {
        assert!(MIN_START_FREE > MIN_KEEP_FREE);
    }

    #[test]
    fn human_formats_gigabytes() {
        assert_eq!(human(2 * 1024 * 1024 * 1024), "2.0 GB");
    }
}
