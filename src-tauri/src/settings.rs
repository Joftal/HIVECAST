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
const KEY_RECORD_SOURCE_FORMAT: &str = "record.source_format";
const KEY_RECORD_CODEC: &str = "record.codec";
const KEY_SPLIT_ON_TITLE: &str = "record.split_on_title";
const KEY_SPLIT_MAX_DURATION: &str = "record.split_max_duration_sec";
const KEY_SPLIT_MAX_SIZE_MB: &str = "record.split_max_size_mb";
const KEY_MEDIA_ROOT: &str = "media.root";
const KEY_MAX_CONCURRENT: &str = "record.max_concurrent";
const KEY_NOTIFY: &str = "ui.notify";
/// 播放/录制异常（取流失败、磁盘写满）是否通知。
pub const KEY_NOTIFY_ERROR: &str = "ui.notify_error";
/// 登录失效是否通知。
pub const KEY_NOTIFY_AUTH: &str = "ui.notify_auth";
const KEY_LANGUAGE: &str = "ui.language";
const KEY_THEME: &str = "ui.theme";
/// 保留策略：多少天后自动删除；0 表示不按天删。
pub const KEY_RETENTION_DAYS: &str = "media.retention_days";
/// 保留策略：媒体库总容量上限（GB）；0 表示不按容量删。
pub const KEY_MAX_TOTAL_GB: &str = "media.max_total_gb";
/// 文件后处理：录制结束后是否自动合成 mp4。
pub const KEY_POST_AUTO_REMUX: &str = "post.auto_remux";
/// 关闭主窗口的行为：tray（最小化到托盘）| quit（退出程序）。
pub const KEY_CLOSE_ACTION: &str = "app.close_action";
/// 阻止系统空闲休眠。
pub const KEY_PREVENT_SLEEP: &str = "app.prevent_sleep";
/// 监控检测间隔（秒）；0 = 关闭自动检测。
pub const KEY_MONITOR_INTERVAL: &str = "monitor.interval_sec";
/// 失败时是否自动加大间隔。
pub const KEY_MONITOR_BACKOFF: &str = "monitor.backoff_enabled";
/// 自动回退的间隔上限（秒）。
pub const KEY_MONITOR_BACKOFF_CAP: &str = "monitor.backoff_cap_sec";
/// 录制文件名模板。
pub const KEY_FILENAME_TEMPLATE: &str = "media.filename_template";
/// 去掉文件名里的 emoji 与特殊符号。
pub const KEY_STRIP_EMOJI: &str = "media.strip_emoji";
/// 按「平台/主播」建子目录。
pub const KEY_GROUP_DIRS: &str = "media.group_dirs";
/// 日志目录；空串表示使用平台默认目录。
pub const KEY_LOG_DIR: &str = "log.dir";
/// 日志保留天数；0 = 不按天清理（当天日志仍按 16 MiB 续段，不设总量上限）。
pub const KEY_LOG_RETENTION_DAYS: &str = "log.retention_days";

/// 关闭行为的两种取值，前端也按这两个字符串渲染。
pub const CLOSE_TRAY: &str = "tray";
pub const CLOSE_QUIT: &str = "quit";

pub const MAX_CONCURRENT_LIMIT: u32 = 8;
pub const MAX_RETENTION_DAYS: u32 = 3650;
pub const MAX_TOTAL_GB: u32 = 100_000;
pub const MAX_LOG_RETENTION_DAYS: u32 = 3650;
/// 最高可用画质：B 站会请求 4K/HDR 档，其它平台映射到各自的最高档。
pub const RECORD_QN_HIGHEST: u32 = 40_000;
/// 分割下限：太短的片段没有观看价值，也会让媒体库被碎片塞满。
pub const MIN_SPLIT_DURATION_SEC: u32 = 60;
pub const MIN_SPLIT_SIZE_MB: u32 = 64;
pub const MAX_SPLIT_DURATION_SEC: u32 = 86_400;
pub const MAX_SPLIT_SIZE_MB: u32 = 1_048_576;
/// 监控间隔上限：一天。再大就没有「监控」的意义了。
pub const MAX_MONITOR_INTERVAL: u32 = 86_400;
/// 回退上限的取值范围。
pub const MIN_BACKOFF_CAP: u32 = 60;
pub const MAX_BACKOFF_CAP: u32 = 3_600;

/// 首次启动的默认值。
pub fn defaults() -> Settings {
    Settings {
        default_record_qn: RECORD_QN_HIGHEST,
        default_view_qn: 0,
        record_source_format: "auto".into(),
        record_codec: "auto".into(),
        split_on_title: false,
        split_max_duration_sec: 0,
        split_max_size_mb: 0,
        media_root: default_media_root().display().to_string(),
        max_concurrent_records: 4,
        notify_enabled: true,
        notify_error: true,
        notify_auth: true,
        language: "system".into(),
        theme: "light".into(),
        retention_days: 0,
        max_total_gb: 0,
        auto_remux: true,
        close_action: CLOSE_TRAY.into(),
        prevent_sleep: false,
        monitor_interval_sec: 30,
        monitor_backoff_enabled: true,
        monitor_backoff_cap_sec: 120,
        media_filename_template: crate::filename::DEFAULT_TEMPLATE.into(),
        media_strip_emoji: false,
        media_group_dirs: true,
        log_dir: String::new(),
        log_retention_days: 7,
    }
}

/// 设置里的布尔值统一是 `true`/`false`，同时容忍手工写入的 `1`/`0`。
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
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
    if let Some(value) = read(KEY_RECORD_SOURCE_FORMAT)
        .filter(|v| matches!(v.as_str(), "auto" | "flv" | "hls" | "ts"))
    {
        settings.record_source_format = value;
    }
    if let Some(value) =
        read(KEY_RECORD_CODEC).filter(|v| matches!(v.as_str(), "auto" | "h264" | "h265"))
    {
        settings.record_codec = value;
    }
    if let Some(value) = read(KEY_SPLIT_ON_TITLE).and_then(|v| parse_bool(&v)) {
        settings.split_on_title = value;
    }
    if let Some(value) = read(KEY_SPLIT_MAX_DURATION).and_then(|v| v.parse::<u32>().ok()) {
        settings.split_max_duration_sec = sanitize_split_duration(value);
    }
    if let Some(value) = read(KEY_SPLIT_MAX_SIZE_MB).and_then(|v| v.parse::<u32>().ok()) {
        settings.split_max_size_mb = sanitize_split_size(value);
    }
    if let Some(value) = read(KEY_MEDIA_ROOT).filter(|v| !v.trim().is_empty()) {
        settings.media_root = value;
    }
    if let Some(value) = read(KEY_MAX_CONCURRENT).and_then(|v| v.parse::<u32>().ok()) {
        settings.max_concurrent_records = value.clamp(1, MAX_CONCURRENT_LIMIT);
    }
    if let Some(value) = read(KEY_NOTIFY).and_then(|v| parse_bool(&v)) {
        settings.notify_enabled = value;
    }
    if let Some(value) = read(KEY_NOTIFY_ERROR).and_then(|v| parse_bool(&v)) {
        settings.notify_error = value;
    }
    if let Some(value) = read(KEY_NOTIFY_AUTH).and_then(|v| parse_bool(&v)) {
        settings.notify_auth = value;
    }
    if let Some(value) =
        read(KEY_LANGUAGE).filter(|v| matches!(v.as_str(), "zh-CN" | "en-US" | "system"))
    {
        settings.language = value;
    }
    if let Some(value) =
        read(KEY_THEME).filter(|v| matches!(v.as_str(), "light" | "dark" | "system"))
    {
        settings.theme = value;
    }
    if let Some(value) = read(KEY_RETENTION_DAYS).and_then(|v| v.parse::<u32>().ok()) {
        settings.retention_days = value.min(MAX_RETENTION_DAYS);
    }
    if let Some(value) = read(KEY_MAX_TOTAL_GB).and_then(|v| v.parse::<u32>().ok()) {
        settings.max_total_gb = value.min(MAX_TOTAL_GB);
    }
    if let Some(value) = read(KEY_POST_AUTO_REMUX).and_then(|v| parse_bool(&v)) {
        settings.auto_remux = value;
    }
    if let Some(value) = read(KEY_CLOSE_ACTION).filter(|v| v == CLOSE_TRAY || v == CLOSE_QUIT) {
        settings.close_action = value;
    }
    if let Some(value) = read(KEY_PREVENT_SLEEP).and_then(|v| parse_bool(&v)) {
        settings.prevent_sleep = value;
    }
    if let Some(value) = read(KEY_MONITOR_INTERVAL).and_then(|v| v.parse::<u32>().ok()) {
        settings.monitor_interval_sec = value.min(MAX_MONITOR_INTERVAL);
    }
    if let Some(value) = read(KEY_MONITOR_BACKOFF).and_then(|v| parse_bool(&v)) {
        settings.monitor_backoff_enabled = value;
    }
    if let Some(value) = read(KEY_MONITOR_BACKOFF_CAP).and_then(|v| v.parse::<u32>().ok()) {
        settings.monitor_backoff_cap_sec = value.clamp(MIN_BACKOFF_CAP, MAX_BACKOFF_CAP);
    }
    if let Some(value) = read(KEY_FILENAME_TEMPLATE) {
        let value = value.trim().to_string();
        // 旧版本或手改库可能留下坏模板，读的时候校验一次，坏了回落到默认值。
        if crate::filename::validate(&value).is_ok() {
            settings.media_filename_template = value;
        }
    }
    if let Some(value) = read(KEY_STRIP_EMOJI).and_then(|v| parse_bool(&v)) {
        settings.media_strip_emoji = value;
    }
    if let Some(value) = read(KEY_GROUP_DIRS).and_then(|v| parse_bool(&v)) {
        settings.media_group_dirs = value;
    }
    if let Some(value) = read(KEY_LOG_DIR).filter(|v| !v.trim().is_empty()) {
        settings.log_dir = value;
    }
    if let Some(value) = read(KEY_LOG_RETENTION_DAYS).and_then(|v| v.parse::<u32>().ok()) {
        settings.log_retention_days = value.min(MAX_LOG_RETENTION_DAYS);
    }
    settings
}

/// 只把媒体目录写回设置：启动时目录不可用、回落默认目录后用（H2）。
/// 不走 save()，因为这里已经知道目录能建出来，校验只会重复一次 IO。
pub fn persist_media_root(db: &Db, path: &str) -> Result<(), AppError> {
    db.set_setting(KEY_MEDIA_ROOT, path)
}

/// 校验并落库；返回纠正过的值（例如并发数被夹到 1..=8），前端据此回填输入框。
pub fn save(db: &Db, input: &Settings) -> Result<Settings, AppError> {
    let mut settings = input.clone();
    settings.default_record_qn = normalize_qn(settings.default_record_qn);
    settings.default_view_qn = normalize_qn(settings.default_view_qn);
    if !matches!(
        settings.record_source_format.as_str(),
        "auto" | "flv" | "hls" | "ts"
    ) {
        settings.record_source_format = "auto".into();
    }
    if !matches!(settings.record_codec.as_str(), "auto" | "h264" | "h265") {
        settings.record_codec = "auto".into();
    }
    settings.split_max_duration_sec = sanitize_split_duration(settings.split_max_duration_sec);
    settings.split_max_size_mb = sanitize_split_size(settings.split_max_size_mb);
    settings.max_concurrent_records = settings
        .max_concurrent_records
        .clamp(1, MAX_CONCURRENT_LIMIT);
    if !matches!(settings.language.as_str(), "zh-CN" | "en-US" | "system") {
        settings.language = "system".into();
    }
    if !matches!(settings.theme.as_str(), "light" | "dark" | "system") {
        settings.theme = "light".into();
    }
    if !matches!(settings.close_action.as_str(), CLOSE_TRAY | CLOSE_QUIT) {
        settings.close_action = CLOSE_TRAY.into();
    }
    settings.retention_days = settings.retention_days.min(MAX_RETENTION_DAYS);
    settings.max_total_gb = settings.max_total_gb.min(MAX_TOTAL_GB);
    settings.monitor_interval_sec = settings.monitor_interval_sec.min(MAX_MONITOR_INTERVAL);
    settings.monitor_backoff_cap_sec = settings
        .monitor_backoff_cap_sec
        .clamp(MIN_BACKOFF_CAP, MAX_BACKOFF_CAP);
    settings.media_filename_template = settings.media_filename_template.trim().to_string();
    crate::filename::validate(&settings.media_filename_template)
        .map_err(|reason| AppError::User(format!("文件名模板不可用：{reason}")))?;
    settings.log_retention_days = settings.log_retention_days.min(MAX_LOG_RETENTION_DAYS);
    settings.log_dir = settings.log_dir.trim().to_string();
    if !settings.log_dir.is_empty() {
        settings.log_dir = validate_log_dir(&settings.log_dir)?;
    }
    let media_root = settings.media_root.trim();
    if media_root.is_empty() {
        return Err(AppError::User("媒体目录不能为空".into()));
    }
    let path = PathBuf::from(media_root);
    if path.exists() && !path.is_dir() {
        return Err(AppError::User(format!("{} 不是目录", path.display())));
    }
    if !path.exists() {
        std::fs::create_dir_all(&path).map_err(|err| AppError::from_io(err, &path))?;
    }
    settings.media_root = path.display().to_string();

    db.set_setting(KEY_RECORD_QN, &settings.default_record_qn.to_string())?;
    db.set_setting(KEY_VIEW_QN, &settings.default_view_qn.to_string())?;
    db.set_setting(KEY_RECORD_SOURCE_FORMAT, &settings.record_source_format)?;
    db.set_setting(KEY_RECORD_CODEC, &settings.record_codec)?;
    db.set_setting(KEY_SPLIT_ON_TITLE, &settings.split_on_title.to_string())?;
    db.set_setting(
        KEY_SPLIT_MAX_DURATION,
        &settings.split_max_duration_sec.to_string(),
    )?;
    db.set_setting(
        KEY_SPLIT_MAX_SIZE_MB,
        &settings.split_max_size_mb.to_string(),
    )?;
    db.set_setting(KEY_MEDIA_ROOT, &settings.media_root)?;
    db.set_setting(
        KEY_MAX_CONCURRENT,
        &settings.max_concurrent_records.to_string(),
    )?;
    db.set_setting(KEY_NOTIFY, &settings.notify_enabled.to_string())?;
    db.set_setting(KEY_NOTIFY_ERROR, &settings.notify_error.to_string())?;
    db.set_setting(KEY_NOTIFY_AUTH, &settings.notify_auth.to_string())?;
    db.set_setting(KEY_LANGUAGE, &settings.language)?;
    db.set_setting(KEY_THEME, &settings.theme)?;
    db.set_setting(KEY_RETENTION_DAYS, &settings.retention_days.to_string())?;
    db.set_setting(KEY_MAX_TOTAL_GB, &settings.max_total_gb.to_string())?;
    db.set_setting(KEY_POST_AUTO_REMUX, &settings.auto_remux.to_string())?;
    db.set_setting(
        KEY_MONITOR_INTERVAL,
        &settings.monitor_interval_sec.to_string(),
    )?;
    db.set_setting(
        KEY_MONITOR_BACKOFF,
        &settings.monitor_backoff_enabled.to_string(),
    )?;
    db.set_setting(
        KEY_MONITOR_BACKOFF_CAP,
        &settings.monitor_backoff_cap_sec.to_string(),
    )?;
    db.set_setting(KEY_FILENAME_TEMPLATE, &settings.media_filename_template)?;
    db.set_setting(KEY_STRIP_EMOJI, &settings.media_strip_emoji.to_string())?;
    db.set_setting(KEY_GROUP_DIRS, &settings.media_group_dirs.to_string())?;
    db.set_setting(KEY_LOG_DIR, &settings.log_dir)?;
    db.set_setting(
        KEY_LOG_RETENTION_DAYS,
        &settings.log_retention_days.to_string(),
    )?;
    // 阈值调小后立刻生效，不用等下次录制收尾。
    crate::retention::enforce_from_settings(db);
    Ok(settings)
}

/// 应用到运行时对象：目录与并发上限立即生效，语言由前端负责。
pub fn apply(settings: &Settings, recorder: &Recorder) {
    recorder.set_media_root(PathBuf::from(&settings.media_root));
    recorder.set_max_concurrent(settings.max_concurrent_records as usize);
    recorder.set_naming(
        settings.media_filename_template.clone(),
        settings.media_strip_emoji,
        settings.media_group_dirs,
    );
    tracing::debug!(
        media_root = %settings.media_root,
        max_concurrent = settings.max_concurrent_records,
        prevent_sleep = settings.prevent_sleep,
        "应用运行时设置"
    );
    // 电源断言与界面无关，启动和保存都走同一条路径，避免两处状态不一致。
    if let Err(err) = crate::power::apply(settings.prevent_sleep) {
        tracing::warn!(error = %err, "设置防休眠失败");
    }
}

/// 校验日志目录：必须是绝对路径、不能是根目录、不能塞进 macOS .app 包内，
/// 并实际探一次可写（创建后立刻删掉探测文件）。
fn validate_log_dir(raw: &str) -> Result<String, AppError> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(AppError::User("日志目录必须是绝对路径".into()));
    }
    if path.parent().is_none() {
        return Err(AppError::User("日志目录不能是磁盘根目录".into()));
    }
    #[cfg(target_os = "macos")]
    if path.to_string_lossy().contains(".app/Contents") {
        return Err(AppError::User(
            "不能把日志写进 .app 包内，请选择包外的目录".into(),
        ));
    }
    if path.exists() && !path.is_dir() {
        return Err(AppError::User(format!("{} 不是目录", path.display())));
    }
    std::fs::create_dir_all(&path).map_err(|err| AppError::from_io(err, &path))?;
    // create_dir_all 之前只能用字面路径挡 .app；这里再用真实路径挡一次符号链接与 .. 绕过。
    let resolved = path
        .canonicalize()
        .map_err(|err| AppError::from_io(err, &path))?;
    if resolved.parent().is_none() {
        return Err(AppError::User("日志目录不能是磁盘根目录".into()));
    }
    #[cfg(target_os = "macos")]
    if resolved.to_string_lossy().contains(".app/Contents") {
        return Err(AppError::User(
            "不能把日志写进 .app 包内，请选择包外的目录".into(),
        ));
    }
    // create_new 不会跟随已存在的符号链接；带纳秒后缀避免覆盖用户同名文件。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe = path.join(format!(
        ".hivecast-write-test-{}-{stamp}",
        std::process::id()
    ));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|err| AppError::from_io(err, &path))?;
    drop(file);
    let _ = std::fs::remove_file(&probe);
    Ok(path.display().to_string())
}

fn normalize_qn(qn: u32) -> u32 {
    // 0 表示「自动」，其余按平台档位取整到 100 的倍数，避免用户输入 12345 这种脏值。
    if qn == 0 {
        0
    } else {
        (qn / 100 * 100).clamp(80, RECORD_QN_HIGHEST)
    }
}

/// 分割时长：0 = 关闭；否则夹在 60 秒..24 小时。
fn sanitize_split_duration(value: u32) -> u32 {
    if value == 0 {
        0
    } else {
        value.clamp(MIN_SPLIT_DURATION_SEC, MAX_SPLIT_DURATION_SEC)
    }
}

/// 分割体积：0 = 关闭；否则夹在 64 MiB..1 TiB。
fn sanitize_split_size(value: u32) -> u32 {
    if value == 0 {
        0
    } else {
        value.clamp(MIN_SPLIT_SIZE_MB, MAX_SPLIT_SIZE_MB)
    }
}

/// 房间录制画质：0 表示跟随全局；显式档位直接用房间自己的值。
pub fn effective_record_qn(global: u32, room: u32) -> u32 {
    if room == 0 {
        global
    } else {
        room
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
        assert_eq!(normalize_qn(999_999), 40000);
    }

    #[test]
    fn split_limits_are_sanitized() {
        assert_eq!(sanitize_split_duration(0), 0, "0 是不限，不能被夹成 60");
        assert_eq!(sanitize_split_duration(1), MIN_SPLIT_DURATION_SEC);
        assert_eq!(sanitize_split_duration(86_399), 86_399);
        assert_eq!(sanitize_split_duration(u32::MAX), MAX_SPLIT_DURATION_SEC);
        assert_eq!(sanitize_split_size(0), 0, "0 是不限，不能变成最小体积");
        assert_eq!(sanitize_split_size(1), MIN_SPLIT_SIZE_MB);
        assert_eq!(sanitize_split_size(1_024), 1_024);
        assert_eq!(sanitize_split_size(u32::MAX), MAX_SPLIT_SIZE_MB);
    }

    #[test]
    fn room_qn_follows_global_only_when_zero() {
        assert_eq!(effective_record_qn(40_000, 0), 40_000);
        assert_eq!(effective_record_qn(40_000, 150), 150);
        assert_eq!(effective_record_qn(400, 0), 400);
        assert_eq!(effective_record_qn(0, 0), 0);
    }

    #[test]
    fn validate_log_dir_rejects_relative_and_root() {
        assert!(validate_log_dir("relative/logs").is_err());
        assert!(validate_log_dir("/").is_err());
    }

    #[test]
    fn validate_log_dir_accepts_a_writable_directory() {
        let dir = std::env::temp_dir().join(format!("hivecast-logs-{}", uuid::Uuid::new_v4()));
        let accepted = validate_log_dir(&dir.display().to_string()).expect("可写目录应当通过");
        assert_eq!(accepted, dir.display().to_string());
        assert!(dir.is_dir());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".hivecast-write-test")
            })
            .collect();
        assert!(leftovers.is_empty(), "探测文件必须清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_log_dir_rejects_root_aliases() {
        assert!(
            validate_log_dir("/tmp/../..").is_err(),
            ".. 不能绕过根目录限制"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = std::env::temp_dir().join(format!("hivecast-logs-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let link = dir.join("root-link");
            symlink("/", &link).unwrap();
            assert!(validate_log_dir(&link.display().to_string()).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn validate_log_dir_rejects_app_bundle_paths() {
        let dir = std::env::temp_dir().join(format!("hivecast-logs-{}", uuid::Uuid::new_v4()));
        let inside = dir.join("Foo.app").join("Contents").join("logs");
        assert!(validate_log_dir(&inside.display().to_string()).is_err());
        assert!(!dir.exists(), "被拒绝的 .app 路径不应被创建");
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
                record_source_format: "hls".into(),
                record_codec: "h265".into(),
                split_on_title: true,
                split_max_duration_sec: 30,
                split_max_size_mb: 10,
                media_root: media.display().to_string(),
                max_concurrent_records: 99,
                notify_enabled: false,
                notify_error: false,
                notify_auth: false,
                language: "fr-FR".into(),
                theme: "neon".into(),
                retention_days: 9_999,
                max_total_gb: 500_000,
                auto_remux: false,
                close_action: "tray".into(),
                prevent_sleep: false,
                ..defaults()
            },
        )
        .expect("save settings");

        assert_eq!(saved.default_record_qn, 12300);
        assert_eq!(saved.record_source_format, "hls");
        assert_eq!(saved.record_codec, "h265");
        assert!(saved.split_on_title);
        assert_eq!(saved.split_max_duration_sec, MIN_SPLIT_DURATION_SEC);
        assert_eq!(saved.split_max_size_mb, MIN_SPLIT_SIZE_MB);
        assert_eq!(saved.max_concurrent_records, MAX_CONCURRENT_LIMIT);
        assert_eq!(saved.language, "system", "未知语言回落到跟随系统");
        assert_eq!(saved.theme, "light", "未知主题回落到浅色");
        assert_eq!(
            saved.retention_days, MAX_RETENTION_DAYS,
            "保留天数被夹到上限"
        );
        assert_eq!(saved.max_total_gb, MAX_TOTAL_GB, "容量上限被夹到上限");
        assert!(!saved.auto_remux, "文件后处理开关应原样保存");
        assert!(media.is_dir(), "保存时应把媒体目录建出来");

        let loaded = load(&db);
        assert_eq!(loaded.default_record_qn, 12300);
        assert_eq!(loaded.record_source_format, "hls");
        assert_eq!(loaded.record_codec, "h265");
        assert!(loaded.split_on_title);
        assert_eq!(loaded.split_max_duration_sec, MIN_SPLIT_DURATION_SEC);
        assert_eq!(loaded.split_max_size_mb, MIN_SPLIT_SIZE_MB);
        assert_eq!(loaded.max_concurrent_records, MAX_CONCURRENT_LIMIT);
        assert!(!loaded.notify_enabled);
        assert!(!loaded.notify_error, "异常通知开关应原样保存");
        assert!(!loaded.notify_auth, "登录失效通知开关应原样保存");
        assert_eq!(loaded.language, "system");
        assert_eq!(loaded.theme, "light");
        assert_eq!(loaded.retention_days, MAX_RETENTION_DAYS);
        assert_eq!(loaded.max_total_gb, MAX_TOTAL_GB);
        assert!(!loaded.auto_remux);
        assert_eq!(loaded.close_action, CLOSE_TRAY, "未知关闭行为回落到托盘");
        assert!(!loaded.prevent_sleep);

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

    #[test]
    fn load_accepts_numeric_booleans() {
        let dir = std::env::temp_dir().join(format!("hivecast-bool-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = Db::open(&dir.join("test.db")).expect("open db");
        db.set_setting(KEY_SPLIT_ON_TITLE, "1").expect("设置");
        db.set_setting(KEY_POST_AUTO_REMUX, "0").expect("设置");
        let loaded = load(&db);
        assert!(loaded.split_on_title);
        assert!(!loaded.auto_remux);
    }
}
