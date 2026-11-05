use hivecast_ipc::{AppInfo, StreamDescriptor};
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;
use crate::stream::demo;

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, AppError> {
    Ok(state.info.clone())
}

/// 生成（或复用）演示流并注册到本地回环服务，返回可直接交给 hls.js 的地址。
#[tauri::command]
pub async fn demo_stream(state: State<'_, AppState>) -> Result<StreamDescriptor, AppError> {
    let mut guard = state.demo.lock().await;
    if let Some(existing) = guard.as_ref() {
        return Ok(existing.clone());
    }

    let dir = demo::ensure_demo(&state.cache_root).await?;
    state.server.register("demo", dir);

    let descriptor = StreamDescriptor {
        url: state.server.playlist_url("demo"),
        session_id: "demo".into(),
        kind: "demo".into(),
        quality: "720p".into(),
        format: "hls-fmp4".into(),
    };
    *guard = Some(descriptor.clone());
    Ok(descriptor)
}
