use std::path::PathBuf;
use std::sync::Arc;

use hivecast_ipc::{AppInfo, StreamDescriptor};
use tauri::{AppHandle, Manager};

use crate::broker::StreamBroker;
use crate::danmaku::DanmakuHub;
use crate::db::Db;
use crate::error::AppError;
use crate::ffmpeg;
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
    /// 弹幕扇出中心：一房间一条连接，录制与观看共用。
    pub danmaku: Arc<DanmakuHub>,
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
        if let Ok(count) = db.close_orphan_sessions() {
            if count > 0 {
                tracing::info!(count, "收尾上次异常退出的录制记录");
            }
        }

        let media_root = default_media_root();
        tokio::fs::create_dir_all(&media_root)
            .await
            .map_err(|err| AppError::from_io(err, &media_root))?;

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
            danmaku,
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
