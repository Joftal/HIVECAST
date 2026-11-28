//! 磁盘守卫：录制会持续吃空间，宁可提前拦住，也不要把系统盘写满。
//!
//! 阈值取 2 GiB / 1 GiB：4 路 1080p 大约十几分钟就能吃满 2 GiB，
//! 留够让用户看到通知、清理磁盘的时间。
//!
//! 剩余空间按「卷」缓存 5 秒：同一块盘上的所有房间共享一次 statvfs，
//! 监控每秒一拍也只会在缓存过期时真正问一次系统。
//!
//! 目录本身是否可用单独按「路径」缓存：外接盘拔掉后 `volume_key` 会往上
//! 退到根盘，如果拿卷键判断可用性，掉线的盘会被当成系统盘，磁盘守卫就失明了（G3）。
//!
//! 是否可用也不能只看 `available_space`：卷已经卸载时这个调用可能失败，
//! 而 `None` 的旧语义是「放行」，等于给掉线的盘开绿灯。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 开录门槛：低于这个值直接拒绝开录。
pub const MIN_START_FREE: u64 = 2 * 1024 * 1024 * 1024;
/// 录制红线：低于这个值主动收尾停录。
pub const MIN_KEEP_FREE: u64 = 1024 * 1024 * 1024;
/// 剩余空间缓存时长。查询本身很便宜（statvfs，不写盘），
/// 但同一卷上多路录制每秒各查一次没有意义。
pub const CACHE_TTL: Duration = Duration::from_secs(5);

struct Cached {
    at: Instant,
    free: Option<u64>,
}

static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);

struct ReadyCached {
    at: Instant,
    ready: bool,
}

/// 目录可用性缓存。键是目录路径本身，不是卷：外接卷掉线后卷号会退化到根盘，
/// 用卷键会让「根盘录制中」和「掉线卷录制中」共享同一份判断。
static READY: Mutex<Option<HashMap<PathBuf, ReadyCached>>> = Mutex::new(None);

/// 一次磁盘判定的结果：目录可用且空间正常 / 空间低于红线 / 目录本身不可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskStatus {
    Ready,
    Low,
    Missing,
}

/// 卷标识：Unix 用设备号，Windows 用盘符；取不到时退回目录本身。
///
/// 目标目录可能还不存在（首次开录前），所以往上找第一个存在的祖先再取设备号，
/// 否则新建的媒体目录会拿到一个「只属于自己」的假卷，缓存也就共享不起来。
pub fn volume_key(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let existing = path.ancestors().find(|candidate| candidate.exists());
        if let Some(found) = existing {
            if let Ok(meta) = std::fs::metadata(found) {
                return PathBuf::from(format!("dev:{}", meta.dev()));
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(first) = path.components().next() {
            return PathBuf::from(first.as_os_str());
        }
    }
    path.to_path_buf()
}

/// 现查一次剩余空间，不走缓存。
pub fn free_bytes(path: &Path) -> Option<u64> {
    fs4::available_space(path).ok()
}

/// 带缓存的剩余空间：同一卷 5 秒内只查一次。
pub fn cached_free_bytes(path: &Path) -> Option<u64> {
    let key = volume_key(path);
    let now = Instant::now();
    {
        let mut guard = CACHE.lock().unwrap_or_else(|err| err.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(hit) = map.get(&key) {
            if now.duration_since(hit.at) < CACHE_TTL {
                return hit.free;
            }
        }
        let free = free_bytes(path);
        map.insert(key, Cached { at: now, free });
        free
    }
}

/// 目录现在能不能写：能建出来、是目录、不是只读挂载。
///
/// 只做目录项操作，不写文件；外接盘拔掉时 `create_dir_all` 会直接失败。
pub fn root_ready(path: &Path) -> bool {
    if std::fs::create_dir_all(path).is_err() {
        return false;
    }
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_dir() && !meta.permissions().readonly(),
        Err(_) => false,
    }
}

/// 带缓存的目录可用性：同一路径 5 秒内只判定一次。
///
/// 录制中每秒一拍都要问，而 `create_dir_all` 是真实系统调用；
/// 5 秒足够发现拔盘，又不会让监控循环每秒多做几次目录操作。
pub fn cached_root_ready(path: &Path) -> bool {
    let now = Instant::now();
    let (previous, ready) = {
        let mut guard = READY.lock().unwrap_or_else(|err| err.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(hit) = map.get(path) {
            if now.duration_since(hit.at) < CACHE_TTL {
                return hit.ready;
            }
        }
        let previous = map.get(path).map(|hit| hit.ready);
        let ready = root_ready(path);
        map.insert(path.to_path_buf(), ReadyCached { at: now, ready });
        (previous, ready)
    };
    // 目录可用性每秒都会被问一次，只在状态真的翻转时记一条，避免刷屏。
    match (previous, ready) {
        (None, false) | (Some(true), false) => {
            tracing::warn!(path = %path.display(), "录制目录不可用（掉盘或权限变化）");
        }
        (Some(false), true) => {
            tracing::info!(path = %path.display(), "录制目录已恢复");
        }
        _ => {}
    }
    ready
}

/// 一次判定：目录不可用优先于空间不足——盘都掉线了，容量数字没有意义。
pub fn status(path: &Path) -> DiskStatus {
    if !cached_root_ready(path) {
        return DiskStatus::Missing;
    }
    match cached_free_bytes(path) {
        Some(free) if free < MIN_KEEP_FREE => DiskStatus::Low,
        _ => DiskStatus::Ready,
    }
}

/// 清空缓存（测试用，避免跨用例串味）。
#[cfg(test)]
pub fn clear_cache() {
    let mut guard = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    guard.get_or_insert_with(HashMap::new).clear();
    let mut ready = READY.lock().unwrap_or_else(|err| err.into_inner());
    ready.get_or_insert_with(HashMap::new).clear();
}

pub fn human(free: u64) -> String {
    format!("{:.1} GB", free as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// 够不够开录。取不到容量时放行：网络盘 / 特殊文件系统不该阻塞录制。
pub fn can_start(path: &Path) -> bool {
    cached_root_ready(path)
        && cached_free_bytes(path)
            .map(|free| free >= MIN_START_FREE)
            .unwrap_or(true)
}

/// 是否已经踩到红线（目录不可用不算红线，单独由 `status` 报 Missing）。
pub fn is_low(path: &Path) -> bool {
    matches!(status(path), DiskStatus::Low)
}

/// 开录被拦时的说明文案（直接进 UI 行内错误）。
pub fn start_blocked_message(path: &Path) -> String {
    if !cached_root_ready(path) {
        return if crate::i18n::is_english() {
            format!(
                "Recording folder is unavailable; the external drive may be disconnected ({}). Recording was blocked.",
                path.display()
            )
        } else {
            format!(
                "录制目录不可用，外接盘可能已断开（{}），已拒绝开录",
                path.display()
            )
        };
    }
    match cached_free_bytes(path) {
        Some(free) if crate::i18n::is_english() => format!(
            "Not enough disk space ({}); at least {} is required to start recording.",
            human(free),
            human(MIN_START_FREE)
        ),
        Some(free) => format!(
            "磁盘剩余空间不足（{}），至少需要 {} 才能开录",
            human(free),
            human(MIN_START_FREE)
        ),
        None => crate::i18n::text(
            "无法读取磁盘剩余空间，已拒绝开录",
            "Could not read free disk space; recording was blocked.",
        )
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // 两个阈值都是常量，断言的是编译期常量关系，不是运行时输入。
    #[allow(clippy::assertions_on_constants)]
    fn thresholds_are_ordered() {
        assert!(MIN_START_FREE > MIN_KEEP_FREE);
    }

    #[test]
    fn human_formats_gigabytes() {
        assert_eq!(human(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    /// 缓存是进程级全局状态，两个用例分开写会互相 clear 造成偶发失败，
    /// 所以合成一个：先验证按卷共享，再验证与直查量级一致，最后验证掉线目录。
    #[test]
    fn cache_is_shared_per_volume_and_agrees_with_direct() {
        clear_cache();
        let dir = std::env::temp_dir();
        // 还不存在的子目录同样落在同一个卷上（首次开录前媒体目录尚未创建）。
        let missing = dir.join("hivecast-disk-not-created");
        assert_eq!(volume_key(&dir), volume_key(&missing));
        let first = cached_free_bytes(&dir);
        let second = cached_free_bytes(&missing);
        assert_eq!(first, second, "同一卷第二次查询必须命中缓存");
        // 两次调用之间别的进程也可能在写盘，只要求量级一致。
        let cached = first.expect("缓存值");
        let direct = free_bytes(&dir).expect("直查值");
        let diff = cached.abs_diff(direct);
        assert!(diff < 1024 * 1024 * 1024, "缓存值与直查值偏差过大：{diff}");

        // 可写目录：状态 Ready（临时目录空间足够跑测试）。
        assert!(root_ready(&dir));
        assert_eq!(status(&dir), DiskStatus::Ready);
        assert!(can_start(&dir));
        assert!(!is_low(&dir));

        // 目录路径上压着一个普通文件：`create_dir_all` 必然失败，模拟外接盘拔掉。
        let blocker = dir.join(format!("hivecast-disk-blocker-{}", std::process::id()));
        std::fs::write(&blocker, b"x").expect("建阻塞文件失败");
        let lost = blocker.join("Recording");
        assert!(!root_ready(&lost));
        assert_eq!(status(&lost), DiskStatus::Missing);
        assert!(!can_start(&lost), "目录不可用时绝不能放行开录");
        assert!(start_blocked_message(&lost).contains("录制目录不可用"));
        let _ = std::fs::remove_file(&blocker);
        clear_cache();
    }
}
