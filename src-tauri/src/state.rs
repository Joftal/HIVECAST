use std::path::PathBuf;
use std::sync::Arc;

use hivecast_ipc::{AppInfo, StreamDescriptor};
use tauri::{AppHandle, Emitter, Manager};

use crate::broker::StreamBroker;
use crate::danmaku::DanmakuHub;
use crate::db::Db;
use crate::error::AppError;
use crate::ffmpeg;
use crate::local::LocalPlayer;
use crate::monitor;
use crate::recorder::Recorder;
use crate::stream::StreamServer;
use crate::view::ViewManager;

pub struct AppState {
    pub info: AppInfo,
    pub server: Arc<StreamServer>,
    pub cache_root: PathBuf,
    #[allow(dead_code)]
    pub media_root: PathBuf,
    pub db: Arc<Db>,
    pub broker: Arc<StreamBroker>,
    pub recorder: Arc<Recorder>,
    pub view: Arc<ViewManager>,
    /// 媒体库的本地回放（同一时刻只有一个）。
    pub local: Arc<LocalPlayer>,
    /// 弹幕扇出中心：一房间一条连接，录制与观看共用。
    pub danmaku: Arc<DanmakuHub>,
    /// 媒体库九宫格缩略图（后台限流生成，完成后广播事件）。
    pub thumbs: Arc<crate::thumbs::Thumbs>,
    /// 正在跑的后处理任务（转封装、合并、弹幕导出），关闭程序时用来拦退出。
    pub jobs: Arc<crate::jobs::Jobs>,
    /// 监控排期快照：详情抽屉轮询下次检查/退避/失败计数。
    pub monitor_status: monitor::MonitorStatusMap,
    /// 演示流只生成一次，之后复用同一份缓存目录（真实会话在 M2 才会按槽位隔离）。
    pub demo: tokio::sync::Mutex<Option<StreamDescriptor>>,
    /// 启动时媒体目录不可用、被迫回落：(设置里的路径, 真正用的路径)。只在启动那一刻有值。
    pub media_fallback: Option<(String, String)>,
    /// 启动时日志目录不可用、被迫回落：(设置里的路径, 真正用的路径)。只在启动那一刻有值。
    pub log_fallback: Option<(String, String)>,
}

impl AppState {
    pub async fn init(app: &AppHandle) -> Result<Self, AppError> {
        let cache_root = app
            .path()
            .app_cache_dir()
            .map_err(|err| AppError::Internal(err.to_string()))?;
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|err| AppError::Internal(err.to_string()))?;

        tracing::info!(
            data_dir = %data_dir.display(),
            cache_dir = %cache_root.display(),
            "应用状态初始化开始"
        );

        tokio::fs::create_dir_all(&cache_root)
            .await
            .map_err(|err| {
                tracing::error!(dir = %cache_root.display(), error = %err, "创建缓存目录失败");
                AppError::from_io(err, &cache_root)
            })?;
        tokio::fs::create_dir_all(&data_dir).await.map_err(|err| {
            tracing::error!(dir = %data_dir.display(), error = %err, "创建数据目录失败");
            AppError::from_io(err, &data_dir)
        })?;

        let db_path = data_dir.join("hivecast.db");
        let db = Arc::new(Db::open(&db_path).map_err(|err| {
            tracing::error!(db = %db_path.display(), error = %err, "打开数据库失败");
            err
        })?);
        tracing::info!(db = %db_path.display(), "数据库已打开");
        // 设置要最早读：手动 ffmpeg 路径必须在后处理恢复启动前生效，
        // 否则启动瞬间的合成会退回 PATH，日志里还会报「未找到 ffmpeg」。
        let mut settings = crate::settings::load(&db);
        crate::settings::apply_ffmpeg_path(&settings.ffmpeg_path);
        // 先杀掉上次崩溃遗留、还在往分片里写的 ffmpeg（A6）：
        // 否则后面的「分片合成 mp4」会和它同时动同一批文件，产物直接坏掉。
        crate::recorder::reap_orphans(&db);
        // 收尾要放在合并之前：合并会删掉分片，之后就取不到最后写入时间了（I1）。
        match db.close_orphan_sessions() {
            Ok(count) if count > 0 => tracing::info!(count, "收尾上次异常退出的录制记录"),
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "收尾上次异常退出的录制记录失败"),
        }
        // 启动时清一次：长期不录制时录制收尾不会触发，磁盘只涨不降。
        crate::retention::enforce_from_settings(&db);
        crate::retention::enforce_tasks(&db);
        // 分片合成搬到后台（I2）：10 GB 残留分片要合并几分钟，不能挡主窗口出现。
        // 收尾后这些会话已是 aborted，recover_parts 会从「失败/中止」列表里捞回来。
        let recovery_db = db.clone();
        let recovery_app = app.clone();
        // 恢复也可能跑很久的后处理：登记成任务，退出提示里能看见，也不会被静默打断。
        let jobs = crate::jobs::Jobs::new();
        jobs.attach_db(db.clone());
        jobs.mark_interrupted();
        let recovery_jobs = jobs.clone();
        tauri::async_runtime::spawn(async move {
            let _recovery_job = recovery_jobs.begin_silent("finalize", "恢复上次录制", None);
            let (recovered, recovered_bytes) = crate::recorder::recover_parts(&recovery_db).await;
            if recovered > 0 {
                tracing::info!(recovered, recovered_bytes, "恢复上次崩溃遗留的录像");
                // 媒体库可能已经打开并拉过列表：合并完成后让它重新取一次。
                let _ = recovery_app.emit(crate::recorder::EVENT_MEDIA_CHANGED, ());
            }
        });

        // 账号凭据先灌进内存，之后所有 HTTP 请求按域名自动带上。
        if let Err(err) = db.reseal_credentials() {
            tracing::warn!(error = %err, "历史 Cookie 加密迁移失败");
        }
        crate::credential::load_from_db(&db);
        // 自动续期与复验是两条独立的循环：
        // - 续期每 12 小时问一次 cookie/info，需要时才轮换 Cookie；
        // - 复验每 30 分钟打一次 nav，只负责发现失效。
        let db_for_refresh = db.clone();
        let app_for_refresh = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::account::refresh_loop(db_for_refresh, app_for_refresh).await;
        });

        // B 站登录态会过期，启动后异步复验一次；之后每 30 分钟再看一次，
        // 运行中 Cookie 失效也能及时发现（网络慢不该拖住启动）。
        let db_for_auth = db.clone();
        let app_for_auth = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                crate::account::revalidate(&db_for_auth, &app_for_auth).await;
                tokio::time::sleep(crate::account::REVALIDATE_INTERVAL).await;
            }
        });

        // 斗鱼的续期/复验单独一条循环：LTP0 每 3 天才需要换票，并带 6h/24h 退避；
        // 不复用 B 站的 refresh_loop，避免两个平台的节奏互相影响。
        let db_for_douyu = db.clone();
        let app_for_douyu = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::douyu_auth::maintenance_loop(db_for_douyu, app_for_douyu).await;
        });

        let db_for_douyin = db.clone();
        let app_for_douyin = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::douyin_auth::maintenance_loop(db_for_douyin, app_for_douyin).await;
        });

        // 虎牙：20 分钟 cookieExchange + 6 小时 checkLogin 复验，失败按 6h/24h 退避。
        let db_for_huya = db.clone();
        let app_for_huya = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::huya_auth::maintenance_loop(db_for_huya, app_for_huya).await;
        });

        let db_for_huya_check = db.clone();
        let app_for_huya_check = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                crate::huya_auth::revalidate(&db_for_huya_check, &app_for_huya_check).await;
                tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
            }
        });

        // 抖音离线关注没有 web_rid，只能低频翻关注列表等开播；这条循环和监控分开，
        // 不占用房间 30 秒轮询的预算。
        let db_for_pool = db.clone();
        let app_for_pool = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::platform::douyin_follow::pool_loop(db_for_pool, app_for_pool).await;
        });

        // 设置里的媒体目录优先；目录不可用时回落到默认值，不要让用户因为改错路径就打不开应用。
        let mut media_root = PathBuf::from(&settings.media_root);
        let mut media_fallback = None;
        if tokio::fs::create_dir_all(&media_root).await.is_err() {
            let configured = settings.media_root.clone();
            media_root = default_media_root();
            let _ = tokio::fs::create_dir_all(&media_root).await;
            settings.media_root = media_root.display().to_string();
            // 只改内存会让「设置里写着 A、文件全在 B」并且之后任何一次保存设置都失败（H2）。
            // 这里把回落结果落库，并留给 lib.rs 弹一条系统通知。
            tracing::warn!(
                configured = %configured,
                actual = %media_root.display(),
                "媒体目录不可用，已回落到默认目录"
            );
            if let Err(err) = crate::settings::persist_media_root(&db, &settings.media_root) {
                tracing::warn!(error = %err, "回落的媒体目录写库失败");
            }
            media_fallback = Some((configured, settings.media_root.clone()));
        }

        // 日志目录在设置加载后应用；不可用时保留用户设置、回落到平台默认目录，
        // 并把「设置里写的」与「实际写的」留给 lib.rs 通知。
        let log_fallback =
            match crate::logging::apply_settings(&settings.log_dir, settings.log_retention_days) {
                Ok(dir) => {
                    tracing::info!(dir = %dir.display(), "日志目录已就绪");
                    None
                }
                Err(reason) => {
                    tracing::warn!(error = %reason, "日志目录不可用，已回落到默认目录");
                    crate::logging::take_fallback().or_else(|| {
                        Some((
                            settings.log_dir.clone(),
                            crate::logging::default_dir().display().to_string(),
                        ))
                    })
                }
            };
        crate::logging::cleanup(settings.log_retention_days);

        let server = StreamServer::start().await.map_err(|err| {
            tracing::error!(error = %err, "启动回环播放服务失败");
            AppError::Internal(err.to_string())
        })?;
        tracing::info!(port = server.port(), "回环播放服务已就绪");

        let broker = Arc::new(StreamBroker::new());
        let danmaku = DanmakuHub::new(app.clone());
        let recorder = Arc::new(Recorder::new(
            db.clone(),
            media_root.clone(),
            Some(danmaku.clone()),
        ));
        let view = ViewManager::new(cache_root.join("view"), server.clone(), danmaku.clone());
        view.clean_cache().await;
        crate::settings::apply(&settings, &recorder);
        let local = LocalPlayer::new(cache_root.join("play"), server.clone());
        let _ = tokio::fs::remove_dir_all(cache_root.join("play")).await;
        let thumbs = Arc::new(crate::thumbs::Thumbs::new(
            cache_root.join("thumbs"),
            server.clone(),
            jobs.clone(),
        ));
        tokio::fs::create_dir_all(thumbs.dir())
            .await
            .map_err(|err| {
                tracing::error!(dir = %thumbs.dir().display(), error = %err, "创建缩略图目录失败");
                AppError::from_io(err, thumbs.dir())
            })?;
        // 录制收尾时自己生成封面，媒体库不用再等 ffmpeg。
        recorder.attach_thumbs(thumbs.clone());

        let monitor_status = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        monitor::spawn(
            app.clone(),
            db.clone(),
            broker.clone(),
            recorder.clone(),
            monitor_status.clone(),
        );

        let info = AppInfo {
            name: "HIVECAST".into(),
            version: app.package_info().version.to_string(),
            platform: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            ffmpeg_path: ffmpeg::path_string(),
            data_dir: data_dir.display().to_string(),
            cache_dir: cache_root.display().to_string(),
            loopback_port: server.port(),
        };

        tracing::info!(
            media_root = %media_root.display(),
            cache_root = %cache_root.display(),
            thumbs = %thumbs.dir().display(),
            "应用状态初始化完成"
        );

        Ok(Self {
            info,
            server,
            cache_root: cache_root.join("streams"),
            media_root,
            db,
            broker,
            recorder,
            view,
            local,
            danmaku,
            thumbs,
            jobs,
            monitor_status,
            demo: tokio::sync::Mutex::new(None),
            media_fallback,
            log_fallback,
        })
    }
}

/// 媒体库根目录：默认在「程序安装目录/Recording」，程序装在哪就存哪；
/// 装到不可写的位置（例如 /Applications 下无权限、直接从 DMG 运行）时，
/// 回落到用户影片目录。`HIVECAST_MEDIA_DIR` 可覆盖（测试与迁移用）。
pub fn default_media_root() -> PathBuf {
    if let Ok(custom) = std::env::var("HIVECAST_MEDIA_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }

    if let Some(dir) = install_dir() {
        let candidate = dir.join("Recording");
        if ensure_writable(&candidate) {
            return candidate;
        }
        tracing::info!(dir = %candidate.display(), "安装目录不可写，媒体库回落到用户影片目录");
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Movies").join("HIVECAST");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Videos").join("HIVECAST");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Videos").join("HIVECAST");
        }
    }

    PathBuf::from("Recording")
}

/// 安装目录：macOS 取 .app 所在目录（可执行文件在 Contents/MacOS 下，上溯四层），
/// 其它平台取可执行文件所在目录。未打包运行时就是 target/debug 这类目录。
fn install_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();

    #[cfg(target_os = "macos")]
    {
        // .../HIVECAST.app/Contents/MacOS/hivecast
        let is_macos = dir.file_name().and_then(|n| n.to_str()) == Some("MacOS");
        if is_macos {
            if let Some(root) = dir
                .parent()
                .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("Contents"))
                .and_then(|p| p.parent())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
                .and_then(|p| p.parent())
            {
                return Some(root.to_path_buf());
            }
        }
    }

    Some(dir)
}

/// 目录能建出来、也能真的写进去一个文件。
fn ensure_writable(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    // 唯一文件名 + create_new：不能截断用户目录里恰好同名的文件，
    // 也不能跟随已存在的符号链接。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe = dir.join(format!(
        ".hivecast-write-test-{}-{stamp}",
        std::process::id()
    ));
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    else {
        return false;
    };
    drop(file);
    let _ = std::fs::remove_file(&probe);
    true
}

#[cfg(test)]
mod tests {
    use super::ensure_writable;

    #[test]
    fn write_probe_never_touches_an_existing_fixed_name() {
        let dir = std::env::temp_dir().join(format!("hivecast-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join(".hivecast-write-test");
        std::fs::write(&legacy, b"do not touch").unwrap();

        assert!(ensure_writable(&dir));
        assert_eq!(std::fs::read(&legacy).unwrap(), b"do not touch");
        let leaked: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy() != ".hivecast-write-test")
            .collect();
        assert!(leaked.is_empty(), "探测文件必须删除");
        std::fs::remove_dir_all(&dir).ok();
    }
}
