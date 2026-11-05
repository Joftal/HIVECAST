use hivecast_ipc::{AppInfo, RecordSession, RoomRecord, RoomView, StreamDescriptor};
use tauri::{AppHandle, Emitter, State};

use crate::db::now_ts;
use crate::error::AppError;
use crate::monitor::room_ref;
use crate::platform::resolve_input;
use crate::recorder::EVENT_ROOMS_CHANGED;
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

#[tauri::command]
pub async fn rooms_list(state: State<'_, AppState>) -> Result<Vec<RoomView>, AppError> {
    let rooms = state.db.list_rooms()?;
    Ok(rooms
        .into_iter()
        .map(|room| RoomView {
            recording: state.recorder.active(&room.id),
            error: state.recorder.last_error(&room.id),
            room,
        })
        .collect())
}

/// 添加房间：纯本地解析 + 拉一次房间信息；同平台同房间号已存在时直接返回已有记录。
#[tauri::command]
pub async fn room_add(
    app: AppHandle,
    state: State<'_, AppState>,
    input: String,
) -> Result<RoomView, AppError> {
    let reference = resolve_input(&input)?;
    if let Some(existing) = state
        .db
        .find_room(&reference.platform, &reference.room_key)?
    {
        return Ok(RoomView {
            recording: state.recorder.active(&existing.id),
            error: state.recorder.last_error(&existing.id),
            room: existing,
        });
    }

    let info = state.broker.room_info(&reference).await?;
    let timestamp = now_ts();
    let draft = RoomRecord {
        id: uuid::Uuid::new_v4().to_string(),
        platform: reference.platform.clone(),
        room_key: reference.room_key.clone(),
        canonical_url: reference.canonical_url.clone(),
        title: info.room_name.clone(),
        host_name: info.host_name.clone(),
        avatar_url: info.avatar_url.clone(),
        cover_url: info.cover_url.clone(),
        area_name: info.area_name.clone(),
        is_live: info.is_live,
        started_at: info.started_at,
        online: info.online,
        auto_record: true,
        record_qn: 10000,
        view_qn: 0,
        last_checked_at: Some(timestamp),
        created_at: timestamp,
        updated_at: timestamp,
    };

    let room = state.db.insert_room(&draft, &info)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());

    if room.is_live && room.auto_record {
        if let Err(err) = state.recorder.start(&app, &state.broker, &room).await {
            state.recorder.set_error(&room.id, err.to_string());
        }
    }

    Ok(RoomView {
        recording: state.recorder.active(&room.id),
        error: state.recorder.last_error(&room.id),
        room,
    })
}

#[tauri::command]
pub async fn room_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    if state.recorder.is_recording(&id) {
        let _ = state.recorder.stop(&app, &id, "stopped").await;
    }
    state.db.delete_room(&id)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

/// 主动刷新一个房间；返回刷新后的视图，前端不用再整表重拉。
#[tauri::command]
pub async fn room_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RoomView, AppError> {
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    let reference = room_ref(&room);
    state.broker.invalidate(&reference);
    let info = state.broker.room_info(&reference).await?;
    state.db.update_room_info(&room.id, &info)?;

    if info.is_live && room.auto_record && !state.recorder.is_recording(&room.id) {
        if let Err(err) = state.recorder.start(&app, &state.broker, &room).await {
            state.recorder.set_error(&room.id, err.to_string());
        }
    } else if !info.is_live && state.recorder.is_recording(&room.id) {
        let _ = state.recorder.stop(&app, &room.id, "stream_ended").await;
    }

    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(RoomView {
        recording: state.recorder.active(&room.id),
        error: state.recorder.last_error(&room.id),
        room,
    })
}

#[tauri::command]
pub async fn room_set_auto_record(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), AppError> {
    state.db.set_auto_record(&id, enabled)?;
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;

    if enabled {
        state.recorder.resume_auto(&id);
    }
    if enabled && room.is_live && !state.recorder.is_recording(&id) {
        if let Err(err) = state.recorder.start(&app, &state.broker, &room).await {
            state.recorder.set_error(&id, err.to_string());
        }
    } else if !enabled && state.recorder.is_recording(&id) {
        let _ = state.recorder.stop(&app, &id, "stopped").await;
    }
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

#[tauri::command]
pub async fn room_set_quality(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    record_qn: u32,
    view_qn: u32,
) -> Result<(), AppError> {
    state.db.set_quality(&id, record_qn, view_qn)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

#[tauri::command]
pub async fn record_start(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    if !room.is_live {
        return Err(AppError::Internal("房间未开播".into()));
    }
    // 手动开录视为用户重新授权自动录制。
    state.recorder.resume_auto(&id);
    match state.recorder.start(&app, &state.broker, &room).await {
        Ok(_) => Ok(()),
        Err(err) => {
            state.recorder.set_error(&id, err.to_string());
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn record_stop(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    state.recorder.stop(&app, &id, "stopped").await?;
    Ok(())
}

#[tauri::command]
pub async fn sessions_list(
    state: State<'_, AppState>,
    room_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<RecordSession>, AppError> {
    state
        .db
        .list_sessions(room_id.as_deref(), limit.unwrap_or(100) as usize)
}
