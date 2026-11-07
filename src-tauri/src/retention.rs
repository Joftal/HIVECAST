//! 媒体库保留策略：按天数 / 总容量清理旧录像。
//!
//! 只在三个时机跑：应用启动、每次录制收尾、保存设置。都不做定时器——
//! 桌面端绝大多数时间没有新文件，定时扫盘只是耗电。
//!
//! 删除是「文件 + 同名弹幕 + 数据库行」一起删：媒体库按数据库行渲染，
//! 只删文件会留下永远打不开的卡片，只删行会留下永远看不见的孤儿文件。

use std::path::Path;

use crate::db::Db;

/// 一条待判定的录像（来自 record_session 左连房间后的最小字段集）。
pub struct Entry {
    pub id: String,
    pub path: String,
    pub size: i64,
    pub started_at: i64,
}

/// 清理结果，用于日志与测试。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub deleted: usize,
    pub freed: u64,
}

/// 读设置并执行清理；两个阈值都是 0 表示「不自动删」，直接返回。
pub fn enforce_from_settings(db: &Db) -> Cleaned {
    let days = read_u32(db, crate::settings::KEY_RETENTION_DAYS).min(3650);
    let max_gb = read_u32(db, crate::settings::KEY_MAX_TOTAL_GB).min(100_000);
    if days == 0 && max_gb == 0 {
        return Cleaned::default();
    }
    let rows = match db.retention_rows() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(%err, "读取媒体库失败，跳过保留策略");
            return Cleaned::default();
        }
    };
    let entries: Vec<Entry> = rows
        .into_iter()
        .map(|(id, path, size, started_at)| Entry {
            id,
            path,
            size,
            started_at,
        })
        .collect();
    let victims = select(
        &entries,
        crate::db::now_ts(),
        days,
        max_gb as u64 * 1024 * 1024 * 1024,
    );
    if victims.is_empty() {
        return Cleaned::default();
    }

    let mut cleaned = Cleaned::default();
    for entry in entries.iter().filter(|entry| victims.contains(&entry.id)) {
        let path = Path::new(&entry.path);
        let mut freed = 0u64;
        // 只删 mp4 与 .jsonl 会留下 .ass / .concat.txt / 残留分片（I4）：
        // 这些文件不进数据库，媒体库里看不见，但会一直占盘。
        for target in artifacts(path) {
            if let Ok(meta) = std::fs::metadata(&target) {
                if std::fs::remove_file(&target).is_ok() {
                    freed += meta.len();
                }
            }
        }
        if let Err(err) = db.delete_session(&entry.id) {
            tracing::warn!(%err, id = %entry.id, "删除历史记录失败");
            continue;
        }
        cleaned.deleted += 1;
        cleaned.freed += freed;
    }
    if cleaned.deleted > 0 {
        tracing::info!(
            deleted = cleaned.deleted,
            freed = cleaned.freed,
            "媒体库保留策略：已清理旧录像"
        );
    }
    cleaned
}

/// 一条录像在磁盘上的全部产物：成品、弹幕、字幕、合并清单、残留分片（I4）。
fn artifacts(path: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![
        path.to_path_buf(),
        path.with_extension("jsonl"),
        path.with_extension("ass"),
        path.with_extension("concat.txt"),
    ];
    out.extend(crate::segment::collect_parts(path));
    out
}

/// 纯判定：返回要删除的 id。
///
/// 规则按顺序叠加：先按天淘汰过期，再按容量从最旧开始淘汰，
/// 一旦容量溢出，后面更旧的即使很小也一并删除——否则会出现
/// 「删了新的、留下更旧的」这种反直觉结果。
pub fn select(entries: &[Entry], now: i64, days: u32, max_bytes: u64) -> Vec<String> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    let cutoff = if days == 0 {
        i64::MIN
    } else {
        now - days as i64 * 86_400
    };
    let mut victims = Vec::new();
    let mut kept_bytes: u64 = 0;
    let mut overflow = false;
    for entry in sorted {
        let size = entry.size.max(0) as u64;
        let expired = days > 0 && entry.started_at < cutoff;
        if !overflow && max_bytes > 0 && kept_bytes + size > max_bytes {
            overflow = true;
        }
        if expired || overflow {
            victims.push(entry.id.clone());
        } else {
            kept_bytes += size;
        }
    }
    victims
}

fn read_u32(db: &Db, key: &str) -> u32 {
    db.get_setting(key)
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, started_at: i64, size: i64) -> Entry {
        Entry {
            id: id.to_string(),
            path: format!("/tmp/{id}.mp4"),
            size,
            started_at,
        }
    }

    #[test]
    fn zero_thresholds_keep_everything() {
        let entries = vec![entry("a", 0, 100)];
        assert!(select(&entries, 2_000_000_000, 0, 0).is_empty());
    }

    #[test]
    fn days_drop_only_expired() {
        let now = 2_000_000_000;
        let entries = vec![
            entry("new", now - 3_600, 10),
            entry("old", now - 40 * 86_400, 10),
        ];
        assert_eq!(select(&entries, now, 30, 0), vec!["old".to_string()]);
    }

    #[test]
    fn capacity_drops_from_oldest_and_takes_the_rest() {
        // 容量 250：new(100) 留，mid(100) 留，old(100) 溢出 → old 删；
        // 更旧的 ancient(1) 虽然很小，也一起删，避免「新删旧留」。
        let now = 2_000_000_000;
        let entries = vec![
            entry("new", now - 10, 100),
            entry("mid", now - 20, 100),
            entry("old", now - 30, 100),
            entry("ancient", now - 40, 1),
        ];
        assert_eq!(
            select(&entries, now, 0, 250),
            vec!["old".to_string(), "ancient".to_string()]
        );
    }

    #[test]
    fn enforce_removes_every_artifact_of_a_session() {
        use std::time::{Duration, UNIX_EPOCH};
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        let dir = std::env::temp_dir().join(format!("hivecast-ret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录失败");
        let final_path = dir.join("旧录像.mp4");
        let started = crate::db::now_ts() - 3 * 86_400;
        std::fs::write(&final_path, vec![0u8; 1_048_576]).expect("写成品");
        for (suffix, bytes) in [("jsonl", 56usize), ("ass", 14), ("concat.txt", 84)] {
            std::fs::write(final_path.with_extension(suffix), vec![b'x'; bytes]).expect("写旁路文件");
        }
        let part = crate::segment::part_name(&final_path, 0);
        std::fs::write(&part, vec![b'x'; 32_768]).expect("写残留分片");
        let _ = std::fs::File::options()
            .write(true)
            .open(&final_path)
            .and_then(|file| file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)));

        db.set_setting(crate::settings::KEY_RETENTION_DAYS, "1")
            .expect("设置保留天数失败");
        db.exec_batch_for_test(&format!(
                "INSERT INTO room (id, platform, room_key, canonical_url, host_name, title, created_at, updated_at)
                 VALUES ('rm-r', 'bilibili', '8', 'https://live.bilibili.com/8', '主播', '房间', 0, 0);
                 INSERT INTO record_session (id, room_id, started_at, status, file_path, file_size, duration_ms)
                 VALUES ('s-r', 'rm-r', {started}, 'finished', '{}', 1048576, 60000);",
                final_path.display()
            ))
            .expect("播种失败");

        let cleaned = enforce_from_settings(&db);
        assert_eq!(cleaned.deleted, 1);
        assert_eq!(cleaned.freed, 1_048_576 + 56 + 14 + 84 + 32_768);
        for suffix in ["mp4", "jsonl", "ass", "concat.txt"] {
            assert!(
                !final_path.with_extension(suffix).exists(),
                "{suffix} 应被清掉"
            );
        }
        assert!(!std::path::Path::new(&part).exists(), "残留分片应被清掉");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enforce_is_noop_without_settings() {
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        assert_eq!(enforce_from_settings(&db), Cleaned::default());
    }
}
