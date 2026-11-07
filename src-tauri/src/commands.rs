use std::path::PathBuf;

use hivecast_ipc::{
    ActiveTask, AppInfo, CredentialInfo, FilenamePreview, LineInfo, MediaHostPage, MediaPage,
    Playback, PostResult, QrPoll, QrTicket, RecordSession, RoomRecord, RoomView, Settings,
    StreamDescriptor, ViewSlotInfo,
};
use tauri::{AppHandle, Emitter, State};

use crate::db::{now_ts, MediaFilter, MediaHostFilter};
use crate::error::AppError;
use crate::monitor::room_ref;
use crate::platform::resolve_input;
use crate::postprocess;
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
    // 是否加入监控；关掉后不参与自动检测，也不自动录制。
    monitor: Option<bool>,
    // 是否开播自动录制；仅在监控开启时有效。
    auto_record: Option<bool>,
) -> Result<RoomView, AppError> {
    let reference = resolve_input(&input)?;
    let monitor = monitor.unwrap_or(true);
    // 不监控就没有「自动录制」可言，直接按关闭处理，避免出现看不见的录制。
    let auto_record = monitor && auto_record.unwrap_or(true);
    if let Some(existing) = state
        .db
        .find_room(&reference.platform, &reference.room_key)?
    {
        // 之前移除过的房间还在库里（历史录像挂在它上面）：这次当恢复处理，
        // 走下面的插入分支，ON CONFLICT 会清掉软删除标记。
        if !state.db.room_removed(&existing.id)? {
            return Ok(RoomView {
                recording: state.recorder.active(&existing.id),
                error: state.recorder.last_error(&existing.id),
                room: existing,
            });
        }
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
        auto_record,
        monitor_paused: !monitor,
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
    // 停止录制会顺手打上「手动停录」标记；房间都移除了，别让它跨到下次重新添加。
    state.recorder.resume_auto(&id);
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

    if info.is_live {
        state.recorder.clear_offline(&room.id);
        // 和监控同一套门槛（B9）：手动暂停监控、手动停过这一场、刚自然结束
        // 还在静默期、连续失败冷却中——这些情况下「刷新」都不该把录制止起来。
        // 以前只看 auto_record，点一下刷新就能绕过用户刚做的停录决定。
        if room.auto_record
            && !state.recorder.is_recording(&room.id)
            && !room.monitor_paused
            && !state.recorder.auto_suppressed(&room.id, info.started_at)
            && !state.recorder.auto_blocked(&room.id, info.started_at)
            && !state.recorder.in_failure_cooldown(&room.id)
        {
            // 用刚写回库的房间记录开录：画质 / 线路 / 开播时间都以这次刷新为准。
            let room_id = room.id.clone();
            let fresh = state.db.get_room(&room_id)?.unwrap_or(room);
            // start_checked 会再查一遍「已被移除 / 监控已暂停」和失败冷却，
            // 与监控路径完全同构，避免刷新走一条更宽松的旁路。
            if let Err(err) = state
                .recorder
                .start_checked(&app, &state.broker, &fresh)
                .await
            {
                state.recorder.set_error(&room_id, err.to_string());
            }
        }
    } else if state.recorder.is_recording(&room.id) {
        // 手动刷新只拿到一次离线：和监控同口径，确认两次、且文件确实不长了
        // 才停录（A1）。否则平台一次状态抖动就会把一场健康录像切断。
        let confirmed = state.recorder.confirm_offline(&room.id);
        let flowing = confirmed && state.recorder.flowing(&room.id);
        if crate::monitor::should_stop_for_offline(confirmed, flowing) {
            state.recorder.note_offline_stop(&room.id);
            let _ = state.recorder.stop(&app, &room.id, "stream_ended").await;
        }
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

/// 暂停/恢复直播状态监控。暂停后定时轮询跳过该房间，已有录制不受影响。
#[tauri::command]
pub async fn room_set_monitor(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    paused: bool,
) -> Result<(), AppError> {
    state.db.set_monitor_paused(&id, paused)?;
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
pub async fn view_close(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
) -> Result<(), AppError> {
    state.view.close(&app, slot).await
}

/// 原子替换槽位里的房间：同一槽位停旧起新，前端不必先 close 再 open。
#[tauri::command]
pub async fn view_replace(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
    room_id: String,
) -> Result<ViewSlotInfo, AppError> {
    let room = state
        .db
        .get_room(&room_id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    if !room.is_live {
        return Err(AppError::User("房间未开播".into()));
    }
    state
        .view
        .replace(&app, state.broker.clone(), slot, &room)
        .await
}

/// 离开观看页：停掉全部观看管道，保留房间与布局，回来时再按布局恢复。
#[tauri::command]
pub async fn view_suspend(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    state.view.suspend_all(&app).await;
    Ok(())
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
    state.view.retry(&app, state.broker.clone(), slot).await
}

// ---- M5 本地后处理 ----

fn session_file(state: &AppState, session_id: &str) -> Result<RecordSession, AppError> {
    state
        .db
        .get_session(session_id)?
        .ok_or_else(|| AppError::Internal("录像不存在".into()))
}

fn file_label(path: &PathBuf) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
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
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PostResult, AppError> {
    let session = session_file(&state, &session_id)?;
    let path = session_video(&session, session.status == "recording")?;
    // 先登记再开跑：退出流程要能看到这个任务，命令返回（含报错）时 guard 自动注销。
    let _job = state
        .jobs
        .begin("remux", file_label(&path), session.quality.clone());
    let progress = postprocess::Progress {
        app: &app,
        task: &session_id,
        total_ms: session.duration_ms,
    };
    postprocess::remux(&path, Some(&progress)).await
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
    let title = state
        .db
        .get_room(&room_id)?
        .map(|room| room.title)
        .filter(|title| !title.trim().is_empty())
        .or_else(|| files.first().map(file_label))
        .unwrap_or_else(|| room_id.clone());
    let _job = state.jobs.begin("merge", title, None);
    postprocess::merge(&files, None).await
}

/// 合并选中的多段录像（按开录时间排序），跨房间也可以。
#[tauri::command]
pub async fn sessions_merge(
    app: tauri::AppHandle,
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
    let total_ms: i64 = sessions
        .iter()
        .map(|session| session.duration_ms.max(0))
        .sum();
    let files = merge_files_in_order(sessions);
    if files.len() < 2 {
        return Err(AppError::User("选中的录像里可用文件不足两段".into()));
    }
    let _job = state.jobs.begin("merge", file_label(&files[0]), None);
    let progress = postprocess::Progress {
        app: &app,
        task: "merge",
        total_ms,
    };
    postprocess::merge(&files, Some(&progress)).await
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
/// 文件名模板预览：用一份固定的示例房间渲染，保证与真正落盘时同一套逻辑。
#[tauri::command]
pub fn filename_preview(
    state: State<'_, AppState>,
    template: String,
    strip_emoji: bool,
    group_dirs: bool,
) -> FilenamePreview {
    let template = template.trim().to_string();
    if let Err(error) = crate::filename::validate(&template) {
        return FilenamePreview {
            name: String::new(),
            path: String::new(),
            valid: false,
            error: Some(error),
        };
    }

    let parts = crate::filename::NameParts {
        platform_id: "bilibili",
        platform: "哔哩哔哩",
        host: "老实憨厚的笑笑",
        title: "【直播】今晚打排位 🎮",
        area: "英雄联盟",
        room_key: "6",
        qn: 10000,
    };
    let naming = crate::recorder::Naming {
        template,
        strip_emoji,
        group_dirs,
    };
    let path = crate::recorder::session_path(
        &state.recorder.media_root(),
        &parts,
        now_ts(),
        "mp4",
        &naming,
    );
    FilenamePreview {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: path.display().to_string(),
        valid: true,
        error: None,
    }
}

#[tauri::command]
pub async fn settings_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, AppError> {
    let saved = crate::settings::save(&state.db, &settings)?;
    crate::settings::apply(&saved, &state.recorder);
    // 托盘菜单文案跟随界面语言。
    crate::tray::refresh(&app);
    Ok(saved)
}

// ---- 观看页布局（分屏树与比例，JSON 原文存 app_setting） ----

#[tauri::command]
pub fn layout_get(state: State<'_, AppState>) -> Result<Option<String>, AppError> {
    state.db.get_setting("view.layout")
}

/// 前端只做结构校验；后端按原文存取，方便以后加字段不迁移。
#[tauri::command]
pub fn layout_set(state: State<'_, AppState>, layout: String) -> Result<(), AppError> {
    if layout.len() > 64 * 1024 {
        return Err(AppError::User("布局数据过大".into()));
    }
    state.db.set_setting("view.layout", &layout)
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

/// 媒体库列表：筛选/排序/分页在 SQL 里做，返回本页 + 总数。
///
/// 顺手把缺缩略图的录像排进后台队列；生成完通过 EVENT_THUMB 回推，
/// 前端按 sessionId 换封面，不用为了等封面把列表卡住。
#[tauri::command]
pub async fn media_list(
    app: AppHandle,
    state: State<'_, AppState>,
    query: Option<String>,
    platform: Option<String>,
    room_id: Option<String>,
    days: Option<i64>,
    only_danmaku: Option<bool>,
    sort: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<MediaPage, AppError> {
    let filter = MediaFilter {
        query,
        platform,
        room_id,
        days,
        only_danmaku: only_danmaku.unwrap_or(false),
        sort: sort.unwrap_or_else(|| "recent".into()),
        limit: limit.unwrap_or(60) as usize,
        offset: offset.unwrap_or(0) as usize,
    };
    let (mut items, total) = state.db.media_list(&filter)?;
    let mut pending = Vec::new();
    for item in &mut items {
        item.thumbnail_url = state.thumbs.url_for(&item.id);
        // 还在录的文件不抽帧：ffmpeg 边写边读会拿到半张图。
        if item.thumbnail_url.is_none() && item.has_file && item.status != "recording" {
            pending.push((item.id.clone(), item.path.clone(), item.duration_ms));
        }
    }
    state.thumbs.ensure(app, pending);
    let platform_counts = state.db.media_platform_counts(&filter)?;
    Ok(MediaPage {
        items,
        total,
        platform_counts,
    })
}

/// 主播墙：按房间聚合出主播卡（条数 / 体积 / 最新一条时间）。
#[tauri::command]
pub async fn media_hosts(
    state: State<'_, AppState>,
    query: Option<String>,
    platform: Option<String>,
    days: Option<i64>,
    sort: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<MediaHostPage, AppError> {
    let filter = MediaHostFilter {
        query,
        platform,
        days,
        sort: sort.unwrap_or_else(|| "latest".into()),
        limit: limit.unwrap_or(60) as usize,
        offset: offset.unwrap_or(0) as usize,
    };
    state.db.media_hosts(&filter)
}

/// 把这条录像的弹幕 JSONL 导出成 ASS 字幕，返回产物路径。
#[tauri::command]
pub async fn session_export_danmaku(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PostResult, AppError> {
    let session = session_file(&state, &session_id)?;
    let path = session
        .danmaku_path
        .filter(|path| std::path::Path::new(path).is_file())
        .ok_or_else(|| AppError::User("这段录像没有弹幕文件".into()))?;
    let _job = state
        .jobs
        .begin("danmaku", file_label(&PathBuf::from(&path)), None);
    postprocess::danmaku_to_ass(std::path::Path::new(&path))
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

    state.thumbs.remove(&session_id);
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
    position_ms: Option<i64>,
) -> Result<Playback, AppError> {
    state.local.open(&state.db, &session_id, position_ms).await
}

/// 停止一条回放；只影响这一条，其余各格继续放。
#[tauri::command]
pub async fn media_stop(state: State<'_, AppState>, session_id: String) -> Result<(), AppError> {
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

// ---- 关闭与退出 ----

/// 当前还没结束的录制与后处理任务，前端在退出提示里逐条列出。
#[tauri::command]
pub fn active_tasks(state: State<'_, AppState>) -> Vec<ActiveTask> {
    crate::lifecycle::active_tasks(&state)
}

/// 用户确认「现在就退出」；只有任务清空后前端才会走到这里。
#[tauri::command]
pub fn close_quit_now(app: AppHandle) {
    crate::lifecycle::quit_now(&app);
}

/// 用户勾了「任务完成后自动退出」。
#[tauri::command]
pub fn close_arm_quit(app: AppHandle) {
    crate::lifecycle::arm_auto_quit(&app);
}

/// 撤销「任务完成后自动退出」，应用继续留着。
#[tauri::command]
pub fn close_cancel_quit() {
    crate::lifecycle::cancel_auto_quit();
}
