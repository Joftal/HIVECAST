use std::path::PathBuf;
use std::sync::Arc;

use hivecast_ipc::{AppInfo, StreamDescriptor};
use tauri::{AppHandle, Manager};

use crate::error::AppError;
use crate::ffmpeg;
use crate::stream::StreamServer;

pub struct AppState {
    pub info: AppInfo,
    pub server: Arc<StreamServer>,
    pub cache_root: PathBuf,
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

        let server = StreamServer::start()
            .await
            .map_err(|err| AppError::Internal(err.to_string()))?;

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
            demo: tokio::sync::Mutex::new(None),
        })
    }
}
