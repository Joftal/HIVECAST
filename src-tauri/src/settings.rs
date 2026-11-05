//! 全局设置：持久化在 app_setting 表，启动时读一次并应用到各子系统。
//!
//! v1 只有一个作用域（全局）。docs/19 §4.4 规划的四级覆盖要等
//! 「同一房间不同会话不同画质」真正出现时再落地，现在拆表只会增加复杂度。

use std::path::PathBuf;

use hivecast_ipc::Settings;

use crate::db::Db;
use crate::error::AppError;
use crate::recorder::Recorder;
use crate::state::default_media_root;

const KEY_RECORD_QN: &str = "record.default_qn";
const KEY_VIEW_QN: &str = "view.default_qn";
const KEY_MEDIA_ROOT: &str = "media.root";
const KEY_MAX_CONCURRENT: &str = "record.max_concurrent";
const KEY_NOTIFY: &str = "ui.notify";
const KEY_LANGUAGE: &str = "ui.language";
/// 保留策略：多少天后自动删除；0 表示不按天删。
pub const KEY_RETENTION_DAYS: &str = "media.retention_days";
/// 保留策略：媒体库总容量上限（GB）；0 表示不按容量删。
pub const KEY_MAX_TOTAL_GB: &str = "media.max_total_gb";

pub const MAX_CONCURRENT_LIMIT: u32 = 8;
pub const MAX_RETENTION_DAYS: u32 = 3650;
pub const MAX_TOTAL_GB: u32 = 100_000;

/// 首次启动的默认值。
pub fn defaults() -> Settings {
    Settings {
            default_record_qn: 10000,
            default_view_qn: 0,
            media_root: default_media_root().display().to_string(),
            max_concurrent_records: 4,
            notify_enabled: true,
        language: "system".into(),
        retention_days: 0,
        max_total_gb: 0,
    }
}

/// 从库里读出设置；缺失或损坏的项回落到默认值。
pub fn load(db: &Db) -> Settings {
    let mut settings = defaults();
    let read = |key: &str| db.get_setting(key).ok().flatten();

    if let Some(value) = read(KEY_RECORD_QN).and_then(|v| v.parse::<u32>().ok()) {
        settings.default_record_qn = value;
    }
    if let Some(value) = read(KEY_VIEW_QN).and_then(|v| v.parse::<u32>().ok()) {
        settings.default_view_qn = value;
    }
    if let Some(value) = read(KEY_MEDIA_ROOT).filter(|v| !v.trim().is_empty()) {
        settings.media_root = value;
    }
    if let Some(value) = read(KEY_MAX_CONCURRENT).and_then(|v| v.parse::<u32>().ok()) {
        settings.max_concurrent_records = value.clamp(1, MAX_CONCURRENT_LIMIT);
    }
    if let Some(value) = read(KEY_NOTIFY).and_then(|v| v.parse::<bool>().ok()) {
        settings.notify_enabled = value;
    }
    if let Some(value) = read(KEY_LANGUAGE).filter(|v| matches!(v.as_str(), "zh-CN" | "en-US" | "system")) {
        settings.language = value;
    }
    if let Some(value) = read(KEY_RETENTION_DAYS).and_then(|v| v.parse::<u32>().ok()) {
        settings.retention_days = value.min(MAX_RETENTION_DAYS);
    }
    if let Some(value) = read(KEY_MAX_TOTAL_GB).and_then(|v| v.parse::<u32>().ok()) {
        settings.max_total_gb = value.min(MAX_TOTAL_GB);
    }
    settings
}

/// 校验并落库；返回纠正过的值（例如并发数被夹到 1..=8），前端据此回填输入框。
pub fn save(db: &Db, input: &Settings) -> Result<Settings, AppError> {
    let mut settings = input.clone();
    settings.default_record_qn = normalize_qn(settings.default_record_qn);
    settings.default_view_qn = normalize_qn(settings.default_view_qn);
    settings.max_concurrent_records = settings
        .max_concurrent_records
        .clamp(1, MAX_CONCURRENT_LIMIT);
    if !matches!(settings.language.as_str(), "zh-CN" | "en-US" | "system") {
        settings.language = "system".into();
    }
    settings.retention_days = settings.retention_days.min(MAX_RETENTION_DAYS);
    settings.max_total_gb = settings.max_total_gb.min(MAX_TOTAL_GB);
    let media_root = settings.media_root.trim();
    if media_root.is_empty() {
        return Err(AppError::User("媒体目录不能为空".into()));
    }
    let path = PathBuf::from(media_root);
    if path.exists() && !path.is_dir() {
        return Err(AppError::User(format!("{} 不是目录", path.display())));
    }
    if !path.exists() {
        std::fs::create_dir_all(&path)
            .map_err(|err| AppError::from_io(err, &path))?;
    }
    settings.media_root = path.display().to_string();

    db.set_setting(KEY_RECORD_QN, &settings.default_record_qn.to_string())?;
    db.set_setting(KEY_VIEW_QN, &settings.default_view_qn.to_string())?;
    db.set_setting(KEY_MEDIA_ROOT, &settings.media_root)?;
    db.set_setting(
        KEY_MAX_CONCURRENT,
        &settings.max_concurrent_records.to_string(),
    )?;
    db.set_setting(KEY_NOTIFY, &settings.notify_enabled.to_string())?;
    db.set_setting(KEY_LANGUAGE, &settings.language)?;
    db.set_setting(KEY_RETENTION_DAYS, &settings.retention_days.to_string())?;
    db.set_setting(KEY_MAX_TOTAL_GB, &settings.max_total_gb.to_string())?;
    // 阈值调小后立刻生效，不用等下次录制收尾。
    crate::retention::enforce_from_settings(db);
    Ok(settings)
}

/// 应用到运行时对象：目录与并发上限立即生效，语言由前端负责。
pub fn apply(settings: &Settings, recorder: &Recorder) {
    recorder.set_media_root(PathBuf::from(&settings.media_root));
    recorder.set_max_concurrent(settings.max_concurrent_records as usize);
}

fn normalize_qn(qn: u32) -> u32 {
    // 0 表示「自动」，其余按平台档位取整到 100 的倍数，避免用户输入 12345 这种脏值。
    if qn == 0 {
        0
    } else {
        (qn / 100 * 100).clamp(80, 20000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qn_rounds_to_platform_steps() {
        assert_eq!(normalize_qn(0), 0, "0 是自动，不能被动");
        assert_eq!(normalize_qn(12345), 12300);
        assert_eq!(normalize_qn(10000), 10000);
        assert_eq!(normalize_qn(1), 80);
        assert_eq!(normalize_qn(999_999), 20000);
    }

    #[test]
    fn save_clamps_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("hivecast-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = Db::open(&dir.join("test.db")).expect("open db");
        let media = dir.join("media");

        let saved = save(
            &db,
            &Settings {
                default_record_qn: 12345,
                default_view_qn: 0,
                media_root: media.display().to_string(),
                max_concurrent_records: 99,
                notify_enabled: false,
                language: "fr-FR".into(),
                retention_days: 9_999,
                max_total_gb: 500_000,
            },
        )
        .expect("save settings");

        assert_eq!(saved.default_record_qn, 12300);
        assert_eq!(saved.max_concurrent_records, MAX_CONCURRENT_LIMIT);
        assert_eq!(saved.language, "system", "未知语言回落到跟随系统");
        assert_eq!(saved.retention_days, MAX_RETENTION_DAYS, "保留天数被夹到上限");
        assert_eq!(saved.max_total_gb, MAX_TOTAL_GB, "容量上限被夹到上限");
        assert!(media.is_dir(), "保存时应把媒体目录建出来");

        let loaded = load(&db);
        assert_eq!(loaded.default_record_qn, 12300);
        assert_eq!(loaded.max_concurrent_records, MAX_CONCURRENT_LIMIT);
        assert!(!loaded.notify_enabled);
        assert_eq!(loaded.language, "system");
        assert_eq!(loaded.retention_days, MAX_RETENTION_DAYS);
        assert_eq!(loaded.max_total_gb, MAX_TOTAL_GB);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rejects_empty_media_root() {
        let dir = std::env::temp_dir().join(format!("hivecast-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = Db::open(&dir.join("test.db")).expect("open db");
        let mut input = defaults();
        input.media_root = "   ".into();
        assert!(save(&db, &input).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
