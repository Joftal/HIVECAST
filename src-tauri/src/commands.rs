use std::path::PathBuf;

use hivecast_ipc::{
    AppInfo, CredentialInfo, LineInfo, MediaItem, Playback, PostResult, QrPoll, QrTicket,
    RecordSession, RoomRecord, RoomView, Settings, StreamDescriptor, ViewSlotInfo,
};
use tauri::{AppHandle, Emitter, State};

use crate::db::now_ts;
use crate::error::AppError;
use crate::monitor::room_ref;
use crate::postprocess;
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
    let settings = crate::settings::load(&state.db);
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
        record_qn: settings.default_record_qn,
        view_qn: settings.default_view_qn,
        record_line: None,
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
    state.view.close_room(&app, &id).await;
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

/// 列出某房间当前可用的线路组（需要触网解析，所以只在用户展开选择器时调）。
#[tauri::command]
pub async fn room_lines(
    state: State<'_, AppState>,
    id: String,
    qn: Option<u32>,
) -> Result<Vec<LineInfo>, AppError> {
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    let groups = state
        .broker
        .groups(&room_ref(&room), qn.unwrap_or(room.record_qn))
        .await?;
    Ok(groups
        .iter()
        .map(|group| LineInfo {
            name: group.name.clone(),
            priority: group.priority,
            quality: group
                .candidates
                .iter()
                .find(|candidate| candidate.url.is_some())
                .map(|candidate| candidate.name.clone())
                .unwrap_or_else(|| "—".into()),
            candidates: group.candidates.iter().filter(|c| c.url.is_some()).count(),
        })
        .collect())
}

/// 指定录制线路；传 None 回到自动选择。只影响下一次开录/续录，不打断当前录制。
#[tauri::command]
pub async fn room_set_line(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    line: Option<String>,
) -> Result<(), AppError> {
    state.db.set_record_line(&id, line.as_deref())?;
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

#[tauri::command]
pub async fn view_slots(state: State<'_, AppState>) -> Result<Vec<ViewSlotInfo>, AppError> {
    Ok(state.view.snapshot())
}

/// 把房间放进空闲观看槽位。qn 为空表示使用房间的观看画质设置。
#[tauri::command]
pub async fn view_open(
    app: AppHandle,
    state: State<'_, AppState>,
    room_id: String,
    qn: Option<u32>,
) -> Result<ViewSlotInfo, AppError> {
    let room = state
        .db
        .get_room(&room_id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    if !room.is_live {
        return Err(AppError::User("房间未开播".into()));
    }
    if let Some(qn) = qn {
        if room.view_qn != qn {
            state.db.set_quality(&room.id, room.record_qn, qn)?;
        }
    }
    let room = state
        .db
        .get_room(&room_id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    state.view.open(&app, state.broker.clone(), &room).await
}

#[tauri::command]
pub async fn view_close(app: AppHandle, state: State<'_, AppState>, slot: u8) -> Result<(), AppError> {
    state.view.close(&app, slot).await
}

/// 切换某个槽位的观看画质：只换这一路的租约，录制进程不受影响。
#[tauri::command]
pub async fn view_set_quality(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
    qn: u32,
) -> Result<ViewSlotInfo, AppError> {
    state.view.set_quality(&app, slot, qn)
}

#[tauri::command]
pub fn view_set_line(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
    line: Option<String>,
) -> Result<ViewSlotInfo, AppError> {
    state.view.set_line(&app, state.broker.clone(), slot, line)
}

#[tauri::command]
pub async fn view_retry(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
) -> Result<ViewSlotInfo, AppError> {
    state
        .view
        .retry(&app, state.broker.clone(), slot)
        .await
}

// ---- M5 本地后处理 ----

fn session_file(state: &AppState, session_id: &str) -> Result<RecordSession, AppError> {
    state
        .db
        .get_session(session_id)?
        .ok_or_else(|| AppError::Internal("录像不存在".into()))
}

fn session_video(session: &RecordSession, active: bool) -> Result<PathBuf, AppError> {
    if active {
        return Err(AppError::User("这段还在录制，先停下再处理".into()));
    }
    let path = session
        .file_path
        .as_ref()
        .ok_or_else(|| AppError::Internal("这段录像没有文件".into()))?;
    Ok(PathBuf::from(path))
}

/// 转封装成 mp4（只换容器，不重编码）。
#[tauri::command]
pub async fn session_remux(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PostResult, AppError> {
    let session = session_file(&state, &session_id)?;
    let path = session_video(&session, session.status == "recording")?;
    postprocess::remux(&path).await
}

/// 截图：默认第 10 秒，避开刚开播的黑帧。
#[tauri::command]
pub async fn session_screenshot(
    state: State<'_, AppState>,
    session_id: String,
    at_secs: Option<f64>,
) -> Result<PostResult, AppError> {
    let session = session_file(&state, &session_id)?;
    let path = session_video(&session, session.status == "recording")?;
    postprocess::screenshot(&path, at_secs).await
}

/// 弹幕 JSONL → ASS 字幕。
#[tauri::command]
pub async fn session_export_danmaku(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PostResult, AppError> {
    let session = session_file(&state, &session_id)?;
    let path = session
        .danmaku_path
        .as_ref()
        .ok_or_else(|| AppError::User("这场没有弹幕存档".into()))?;
    postprocess::danmaku_to_ass(std::path::Path::new(path))
}

/// 把某个房间已完成的分段按时间顺序合并成一个 mp4。
#[tauri::command]
pub async fn room_merge_sessions(
    state: State<'_, AppState>,
    room_id: String,
) -> Result<PostResult, AppError> {
    let mut sessions: Vec<_> = state
        .db
        .list_sessions(Some(&room_id), 200)?
        .into_iter()
        .filter(|session| session.status != "recording" && session.file_path.is_some())
        .collect();
    sessions.sort_by_key(|session| session.started_at);
    let files: Vec<PathBuf> = sessions
        .into_iter()
        .filter_map(|session| session.file_path.map(PathBuf::from))
        .filter(|path| path.is_file())
        .collect();
    postprocess::merge(&files).await
}

/// 合并选中的多段录像（按开录时间排序），跨房间也可以。
#[tauri::command]
pub async fn sessions_merge(
    state: State<'_, AppState>,
    session_ids: Vec<String>,
) -> Result<PostResult, AppError> {
    if session_ids.len() < 2 {
        return Err(AppError::User("至少选两段录像才能合并".into()));
    }
    let mut sessions = Vec::new();
    for id in &session_ids {
        let session = session_file(&state, id)?;
        if session.status == "recording" {
            return Err(AppError::User("有录像还在录制，先停下再合并".into()));
        }
        sessions.push(session);
    }
    let files = merge_files_in_order(sessions);
    if files.len() < 2 {
        return Err(AppError::User("选中的录像里可用文件不足两段".into()));
    }
    postprocess::merge(&files).await
}

/// 合并顺序只看开录时间，与勾选顺序无关；文件已被删掉的跳过。
fn merge_files_in_order(mut sessions: Vec<RecordSession>) -> Vec<PathBuf> {
    sessions.sort_by_key(|session| session.started_at);
    sessions
        .into_iter()
        .filter_map(|session| session.file_path.map(PathBuf::from))
        .filter(|path| path.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, started_at: i64, file: PathBuf) -> RecordSession {
        RecordSession {
            id: id.into(),
            room_id: "room".into(),
            started_at,
            ended_at: Some(started_at + 60),
            status: "finished".into(),
            reason: Some("stopped".into()),
            file_path: Some(file.display().to_string()),
            file_size: 1,
            duration_ms: 60_000,
            quality: None,
            line: None,
            error: None,
            danmaku_path: None,
            danmaku_count: 0,
        }
    }

    #[test]
    fn merge_order_follows_start_time_and_skips_missing() {
        let dir = std::env::temp_dir().join(format!("hivecast-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let missing = dir.join("gone.mp4");
        // 乱序传入，缺失文件在中间。
        let files = merge_files_in_order(vec![
            session("2", 200, b.clone()),
            session("3", 300, missing),
            session("1", 100, a.clone()),
        ]);
        assert_eq!(files, vec![a, b]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- 设置 ----

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> Result<Settings, AppError> {
    Ok(crate::settings::load(&state.db))
}

/// 保存设置；返回规范化后的值（并发数夹到 1..=8、目录会被创建）。
#[tauri::command]
pub async fn settings_update(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, AppError> {
    let saved = crate::settings::save(&state.db, &settings)?;
    crate::settings::apply(&saved, &state.recorder);
    Ok(saved)
}

// ---- 账号与登录（M7.2） ----

#[tauri::command]
pub async fn credentials_list(state: State<'_, AppState>) -> Result<Vec<CredentialInfo>, AppError> {
    crate::account::list(&state.db)
}

/// 保存粘贴的 Cookie。B 站会先打 nav 接口验证并回填昵称，
/// 所以这里可能因「Cookie 失效」失败，而不是静默存一份用不了的凭据。
#[tauri::command]
pub async fn credential_save(
    state: State<'_, AppState>,
    platform: String,
    cookie: String,
) -> Result<CredentialInfo, AppError> {
    crate::account::save_cookie(&state.db, &platform, &cookie).await
}

#[tauri::command]
pub async fn credential_delete(
    state: State<'_, AppState>,
    platform: String,
) -> Result<(), AppError> {
    crate::account::delete(&state.db, &platform)
}

#[tauri::command]
pub async fn bilibili_qr_start() -> Result<QrTicket, AppError> {
    crate::account::bilibili_qr_start().await
}

#[tauri::command]
pub async fn bilibili_qr_poll(
    state: State<'_, AppState>,
    qrcode_key: String,
) -> Result<QrPoll, AppError> {
    crate::account::bilibili_qr_poll(&state.db, &qrcode_key).await
}

// ---- 媒体库 ----

#[tauri::command]
pub fn media_list(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<MediaItem>, AppError> {
    state.db.media_list(limit.unwrap_or(200) as usize)
}

/// 删除一条录像：先停掉可能正在回放的它，再按需删文件，最后删库。
#[tauri::command]
pub async fn media_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    delete_file: bool,
) -> Result<(), AppError> {
    let session = state
        .db
        .get_session(&session_id)?
        .ok_or_else(|| AppError::User("录像不存在".into()))?;
    if session.status == "recording" {
        return Err(AppError::User("这段还在录制，先停下再删除".into()));
    }

    state.local.stop(&session_id).await;

    if delete_file {
        if let Some(path) = session.file_path.as_deref() {
            let path = std::path::Path::new(path);
            if path.is_file() {
                tokio::fs::remove_file(path)
                    .await
                    .map_err(|err| AppError::from_io(err, path))?;
            }
        }
        if let Some(path) = session.danmaku_path.as_deref() {
            let path = std::path::Path::new(path);
            if path.is_file() {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
    }

    state.db.delete_session(&session_id)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

/// 在系统文件管理器里定位文件。
#[tauri::command]
pub async fn media_reveal(state: State<'_, AppState>, session_id: String) -> Result<(), AppError> {
    let session = state
        .db
        .get_session(&session_id)?
        .ok_or_else(|| AppError::User("录像不存在".into()))?;
    let path = session
        .file_path
        .filter(|p| std::path::Path::new(p).is_file())
        .ok_or_else(|| AppError::User("录像文件已不存在".into()))?;
    reveal_in_file_manager(std::path::Path::new(&path))
}

#[tauri::command]
pub async fn media_play(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Playback, AppError> {
    state.local.open(&state.db, &session_id).await
}

/// 停止一条回放；只影响这一条，其余各格继续放。
#[tauri::command]
pub async fn media_stop(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), AppError> {
    state.local.stop(&session_id).await;
    Ok(())
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), AppError> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开 Finder：{err}")))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), AppError> {
    std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开资源管理器：{err}")))?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), AppError> {
    let dir = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open")
        .arg(dir)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开文件管理器：{err}")))?;
    Ok(())
}

