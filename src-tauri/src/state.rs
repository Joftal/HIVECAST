use std::path::PathBuf;
use std::sync::Arc;

use hivecast_ipc::{AppInfo, StreamDescriptor};
use tauri::{AppHandle, Emitter};

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
    _isolated_cache: Option<tempfile::TempDir>,
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
    pub async fn init(app: &AppHandle, paths: &crate::paths::AppPaths) -> Result<Self, AppError> {
        let cache_root = paths.cache_dir.clone();
        let data_dir = paths.data_dir.clone();

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
        #[cfg(windows)]
        let new_system_profile = !db_path
            .try_exists()
            .map_err(|err| AppError::from_io(err, &db_path))?
            && std::env::var_os("HIVECAST_DATA_DIR").is_none()
            && crate::paths::portable_root().is_none_or(|root| data_dir != root.join("data"));
        let db = Arc::new(Db::open(&db_path).map_err(|err| {
            tracing::error!(db = %db_path.display(), error = %err, "打开数据库失败");
            err
        })?);
        #[cfg(windows)]
        if !cfg!(debug_assertions) {
            if let Some(root) = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(PathBuf::from))
            {
                let owner_key = "install.system_profile_owner";
                if new_system_profile {
                    db.set_setting(owner_key, &root.to_string_lossy())?;
                } else if let Some(owner) = db.get_setting(owner_key)? {
                    // Once opened by a second installation the system profile is shared.
                    if !owner.is_empty()
                        && PathBuf::from(owner).canonicalize().ok() != root.canonicalize().ok()
                    {
                        db.set_setting(owner_key, "")?;
                    }
                }
            }
        }
        tracing::info!(db = %db_path.display(), "数据库已打开");
        // 设置要最早读：手动 ffmpeg 路径必须在后处理恢复启动前生效，
        // 否则启动瞬间的合成会退回 PATH，日志里还会报「未找到 ffmpeg」。
        #[cfg(windows)]
        if let Some(root) = crate::paths::portable_root() {
            let explicit = std::env::var("HIVECAST_DATA_DIR").is_ok_and(|s| !s.trim().is_empty());
            let class = if explicit {
                "external"
            } else if data_dir == root.join("data") {
                "local"
            } else {
                "system"
            };
            crate::install_receipt::record_profile(&root, class)
                .map_err(|e| AppError::from_io(e, &root))?;
            if data_dir == root.join("data") {
                db.relocate_portable_library(&root)?;
            }
        }
        db.reseal_credentials()?;
        let mut settings = crate::settings::load(&db);
        // 后续错误序列化、系统通知都按这个语言输出。
        crate::i18n::set_preference(&settings.language);
        db.locate_legacy_media(
            std::path::Path::new(&settings.media_root),
            db.get_setting(crate::settings::KEY_MEDIA_ROOT)?
                .as_deref()
                .map(std::path::Path::new),
        )?;
        crate::settings::apply_ffmpeg_path(&settings.ffmpeg_path);
        // 先杀掉上次崩溃遗留、还在往分片里写的 ffmpeg（A6）：
        // 否则后面的「分片合成 mp4」会和它同时动同一批文件，产物直接坏掉。
        let deferred = crate::recorder::reap_orphans(&db)?;
        // 收尾要放在合并之前：合并会删掉分片，之后就取不到最后写入时间了（I1）。
        let count = db.close_orphan_sessions(&deferred)?;
        if count > 0 {
            tracing::info!(count, "收尾上次异常退出的录制记录");
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
            let (recovered, recovered_bytes) =
                crate::recorder::recover_parts(&recovery_db, &deferred).await;
            if recovered > 0 {
                tracing::info!(recovered, recovered_bytes, "恢复上次崩溃遗留的录像");
                // 媒体库可能已经打开并拉过列表：合并完成后让它重新取一次。
                let _ = recovery_app.emit(crate::recorder::EVENT_MEDIA_CHANGED, ());
            }
        });

        // 账号凭据先灌进内存，之后所有 HTTP 请求按域名自动带上。
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
        // Older versions allowed a recording library inside the cache. Never run
        // cache cleanup against that library, even after its configured root changed.
        let isolated_cache = if db.cache_overlaps_media(&cache_root, &media_root)? {
            Some(
                tempfile::Builder::new()
                    .prefix("hivecast-cache-")
                    .tempdir()
                    .map_err(|e| AppError::from_io(e, &cache_root))?,
            )
        } else {
            None
        };
        let cache_root = isolated_cache
            .as_ref()
            .map(|d| d.path().to_owned())
            .unwrap_or(cache_root);
        let view = ViewManager::new(cache_root.join("view"), server.clone(), danmaku.clone());
        view.clean_cache().await;
        crate::settings::apply(&settings, &recorder);
        let local = LocalPlayer::new(cache_root.join("play"), server.clone());
        let play_root = cache_root.join("play");
        let cleanup_root = play_root.clone();
        let cleanup = tauri::async_runtime::spawn_blocking(move || {
            crate::session_cache::clean(&cleanup_root)
        })
        .await;
        match cleanup {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(dir = %play_root.display(), error = %err, "清理回放缓存失败")
            }
            Err(err) => {
                tracing::warn!(dir = %play_root.display(), error = %err, "清理回放缓存任务失败")
            }
        }
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
            _isolated_cache: isolated_cache,
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

/// 媒体库根目录：
/// - Windows 优先「程序安装目录/Recording」，装到不可写位置（如 Program Files）或网络盘时回落到用户视频目录；
/// - macOS 统一使用用户影片目录 `~/Movies/HIVECAST`（避免管理员账号在 `/Applications` 下建出 `/Applications/Recording`）；
/// - `HIVECAST_MEDIA_DIR` 可覆盖（测试与迁移用）。
pub fn default_media_root() -> PathBuf {
    if let Ok(custom) = std::env::var("HIVECAST_MEDIA_DIR") {
        if !custom.trim().is_empty() {
            if let Ok(path) = std::path::absolute(custom) {
                return path;
            }
        }
    }

    static DEFAULT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DEFAULT.get_or_init(discover_media_root).clone()
}

#[cfg(test)]
static DEFAULT_PROBES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn empty_settings_reuse_default_media_directory() {
    let db = crate::db::Db::open(std::path::Path::new(":memory:")).unwrap();
    let expected = crate::settings::load(&db).media_root;
    let probes = DEFAULT_PROBES.load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..3 {
        assert_eq!(crate::settings::load(&db).media_root, expected);
    }
    assert_eq!(
        DEFAULT_PROBES.load(std::sync::atomic::Ordering::Relaxed),
        probes
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_default_media_root_uses_movies_folder() {
    if std::env::var("HIVECAST_MEDIA_DIR")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        return;
    }
    let home = std::env::var("HOME").expect("HOME 应存在");
    assert_eq!(
        default_media_root(),
        PathBuf::from(home).join("Movies").join("HIVECAST")
    );
}

fn discover_media_root() -> PathBuf {
    #[cfg(test)]
    DEFAULT_PROBES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Movies").join("HIVECAST");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(root) = crate::paths::portable_root() {
            return root.join("Recording");
        }
        if cfg!(debug_assertions) {
            if let Some(dir) = install_dir() {
                let candidate = dir.join("Recording");
                if crate::paths::ensure_writable(&candidate) {
                    return candidate;
                }
            }
        } else {
            tracing::info!("安装目录不可写或位于网络路径，媒体库回落到用户视频目录");
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Videos").join("HIVECAST");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(dir) = install_dir() {
            let candidate = dir.join("Recording");
            if crate::paths::ensure_writable(&candidate) {
                return candidate;
            }
            tracing::info!(dir = %candidate.display(), "安装目录不可写，媒体库回落到用户视频目录");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Videos").join("HIVECAST");
        }
    }

    PathBuf::from("Recording")
}

/// 可执行文件所在目录（仅用于非 macOS 的 debug / Linux 场景）。
#[cfg(not(target_os = "macos"))]
fn install_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    Some(dir)
}
