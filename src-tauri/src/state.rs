use std::path::PathBuf;
use std::sync::Arc;

use hivecast_ipc::{AppInfo, StreamDescriptor};
use tauri::{AppHandle, Manager};

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
    /// 演示流只生成一次，之后复用同一份缓存目录（真实会话在 M2 才会按槽位隔离）。
    pub demo: tokio::sync::Mutex<Option<StreamDescriptor>>,
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

        tokio::fs::create_dir_all(&cache_root)
            .await
            .map_err(|err| AppError::from_io(err, &cache_root))?;
        tokio::fs::create_dir_all(&data_dir)
            .await
            .map_err(|err| AppError::from_io(err, &data_dir))?;

        let db = Arc::new(Db::open(&data_dir.join("hivecast.db"))?);
        // 顺序很重要：先把崩溃遗留的分片合成 mp4，再收尾状态。
        // 反过来的话状态已经变成 aborted，就找不到「待恢复」的会话了。
        let (recovered, recovered_bytes) = crate::recorder::recover_parts(&db).await;
        if recovered > 0 {
            tracing::info!(recovered, recovered_bytes, "恢复上次崩溃遗留的录像");
        }
        if let Ok(count) = db.close_orphan_sessions() {
            if count > 0 {
                tracing::info!(count, "收尾上次异常退出的录制记录");
            }
        }
        // 启动时清一次：长期不录制时录制收尾不会触发，磁盘只涨不降。
        crate::retention::enforce_from_settings(&db);

        // 账号凭据先灌进内存，之后所有 HTTP 请求按域名自动带上。
        if let Err(err) = db.reseal_credentials() {
            tracing::warn!(error = %err, "历史 Cookie 加密迁移失败");
        }
        crate::credential::load_from_db(&db);
        // B 站登录态会过期，启动后异步复验一次；网络慢不该拖住启动。
        let db_for_auth = db.clone();
        let app_for_auth = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::account::revalidate(&db_for_auth, &app_for_auth).await;
        });

        // 设置里的媒体目录优先；目录不可用时回落到默认值，不要让用户因为改错路径就打不开应用。
        let mut settings = crate::settings::load(&db);
        let mut media_root = PathBuf::from(&settings.media_root);
        if tokio::fs::create_dir_all(&media_root).await.is_err() {
            media_root = default_media_root();
            let _ = tokio::fs::create_dir_all(&media_root).await;
            settings.media_root = media_root.display().to_string();
        }

        let server = StreamServer::start()
            .await
            .map_err(|err| AppError::Internal(err.to_string()))?;

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
        ));
        tokio::fs::create_dir_all(thumbs.dir())
            .await
            .map_err(|err| AppError::from_io(err, thumbs.dir()))?;
        // 录制收尾时自己生成封面，媒体库不用再等 ffmpeg。
        recorder.attach_thumbs(thumbs.clone());

        monitor::spawn(app.clone(), db.clone(), broker.clone(), recorder.clone());

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
            jobs: crate::jobs::Jobs::new(),
            demo: tokio::sync::Mutex::new(None),
        })
    }
}

/// 媒体库根目录：默认放到系统「影片 / 视频」目录，允许用环境变量覆盖（便于测试与迁移）。
pub fn default_media_root() -> PathBuf {
    if let Ok(custom) = std::env::var("HIVECAST_MEDIA_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
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

    PathBuf::from("HIVECAST")
}
