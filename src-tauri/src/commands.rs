use std::path::{Path, PathBuf};
use std::time::Instant;

use hivecast_ipc::{
    ActiveTask, AddRoomResult, AppInfo, CredentialInfo, DanmakuMessage, DouyinFollow,
    DouyinFollowingPage, DouyinPoolAddResult, DouyuFollowingPage, FfmpegStatus, FilenamePreview,
    FollowingPage, HuyaFollowingPage, ImportFailure, ImportRoomInput, ImportRoomsResult, LineInfo,
    LiveStatus, LogStatus, MediaFile, MediaItem, MediaPage, MonitorStatus, Playback, PostResult,
    QrPoll, QrTicket, RecordSession, RecordingUsage, RefreshStatus, RemovedRoom, RoomRecord,
    RoomView, SessionSummary, Settings, StreamDescriptor, StreamOptions, TaskBoard, TaskCenter,
    TaskEventSearch, TaskRecord, TrashInfo, ViewSlotInfo,
};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_notification::NotificationExt;

use crate::db::{now_ts, MediaFilter};
use crate::error::AppError;
use crate::monitor::room_ref;
use crate::platform::{resolve_input, RoomInfo, RoomRef};
use crate::postprocess;
use crate::recorder::EVENT_ROOMS_CHANGED;
use crate::state::AppState;
use crate::stream::demo;

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, AppError> {
    Ok(state.info.clone())
}

/// 当前实际使用的 ffmpeg：手动配置、PATH、候选目录或缺失。
#[tauri::command]
pub fn ffmpeg_status() -> FfmpegStatus {
    crate::ffmpeg::status()
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

/// 已移除的直播间：房间信息 + 剩余录像汇总，供监控页底部入口找回。
#[tauri::command]
pub async fn removed_rooms(state: State<'_, AppState>) -> Result<Vec<RemovedRoom>, AppError> {
    state.db.list_removed_rooms()
}

/// 恢复一个已移除的直播间；下一次监控轮询会立刻刷新它。
#[tauri::command]
pub async fn room_restore(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RoomView, AppError> {
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, "恢复房间");
    state.db.restore_room(&id)?;
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    state.broker.invalidate(&room_ref(&room));
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(RoomView {
        recording: state.recorder.active(&room.id),
        error: state.recorder.last_error(&room.id),
        room,
    })
}

#[tauri::command]
pub async fn monitor_status(
    state: State<'_, AppState>,
    room_id: String,
) -> Result<Option<MonitorStatus>, AppError> {
    Ok(state
        .monitor_status
        .lock()
        .map_err(|_| AppError::Internal("监控状态锁被污染".into()))?
        .get(&room_id)
        .cloned())
}

/// 把短号/别名归一成平台稳定 ID，避免同一场直播被录两遍。
fn canonical_reference(reference: &RoomRef, info: &RoomInfo) -> RoomRef {
    let Some(stable) = info
        .stable_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return reference.clone();
    };
    if stable == reference.room_key {
        return reference.clone();
    }
    let canonical_url = match reference.platform.as_str() {
        "bilibili" => format!("https://live.bilibili.com/{stable}"),
        // 斗鱼别名同样归一到数字房间号，后续导入数字 room_id 才能命中同一行。
        "douyu" => format!("https://www.douyu.com/{stable}"),
        // 虎牙别名同样归一到数字房号，手动添加 lpl 与 660000 只保留一行。
        "huya" => format!("https://www.huya.com/{stable}"),
        _ => reference.canonical_url.clone(),
    };
    RoomRef {
        platform: reference.platform.clone(),
        room_key: stable.to_string(),
        canonical_url,
        input: reference.input.clone(),
    }
}

/// 添加房间：先解析稳定 ID，再查重；同一直播间只保留一条记录。
#[tauri::command]
pub async fn room_add(
    app: AppHandle,
    state: State<'_, AppState>,
    input: String,
    // 是否加入监控；关掉后不参与自动检测，也不自动录制。
    monitor: Option<bool>,
    // 是否开播自动录制；仅在监控开启时有效。
    auto_record: Option<bool>,
) -> Result<AddRoomResult, AppError> {
    // b23.tv 短链先跟随跳转，拿到真实房间号后再走统一解析。
    let expanded = crate::platform::bilibili::expand_short_link(&input).await;
    let reference = resolve_input(expanded.as_deref().unwrap_or(&input))?;
    let info = state.broker.room_info(&reference).await?;
    let canonical = canonical_reference(&reference, &info);
    let monitor = monitor.unwrap_or(true);
    // 不监控就没有「自动录制」可言，直接按关闭处理，避免出现看不见的录制。
    let auto_record = monitor && auto_record.unwrap_or(true);

    // 先按稳定 ID 查；再查旧短号行，把历史数据并到稳定 ID 上。
    let canonical_existing = state
        .db
        .find_room(&canonical.platform, &canonical.room_key)?;
    let alias_existing = if canonical.room_key != reference.room_key {
        state
            .db
            .find_room(&reference.platform, &reference.room_key)?
    } else {
        None
    };
    let mut existing = canonical_existing;
    if let Some(alias) = alias_existing {
        match existing.take() {
            None => {
                state
                    .db
                    .rekey_room(&alias.id, &canonical.room_key, &canonical.canonical_url)?;
                existing = state.db.get_room(&alias.id)?;
            }
            Some(canonical_row) if canonical_row.id != alias.id => {
                // 两边都有记录时，优先保留仍在监控中的那条。
                let canonical_active = !state.db.room_removed(&canonical_row.id)?;
                let alias_active = !state.db.room_removed(&alias.id)?;
                let (keep, drop) = if !canonical_active && alias_active {
                    (&alias, &canonical_row)
                } else {
                    (&canonical_row, &alias)
                };
                state.db.merge_room(&keep.id, &drop.id)?;
                if keep.room_key != canonical.room_key {
                    state
                        .db
                        .rekey_room(&keep.id, &canonical.room_key, &canonical.canonical_url)?;
                }
                existing = state.db.get_room(&keep.id)?;
            }
            _ => {}
        }
    }
    if let Some(existing) = existing {
        if !state.db.room_removed(&existing.id)? {
            state.db.update_room_info(&existing.id, &info)?;
            let room = state
                .db
                .get_room(&existing.id)?
                .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
            return Ok(AddRoomResult {
                recording: state.recorder.active(&room.id),
                error: state.recorder.last_error(&room.id),
                room,
                created: false,
            });
        }
    }

    let settings = crate::settings::load(&state.db);
    let timestamp = now_ts();
    let draft = RoomRecord {
        id: uuid::Uuid::new_v4().to_string(),
        platform: canonical.platform.clone(),
        room_key: canonical.room_key.clone(),
        canonical_url: canonical.canonical_url.clone(),
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
        // 0 表示跟随全局设置：设置页改画质后，新房间不用逐个重设。
        record_qn: 0,
        view_qn: settings.default_view_qn,
        record_line: None,
        last_checked_at: Some(timestamp),
        last_session_id: None,
        last_session_started_at: None,
        last_session_duration_ms: 0,
        last_session_status: None,
        last_session_reason: None,
        last_session_file_size: 0,
        last_session_quality: None,
        removed_at: None,
        note: String::new(),
        tags: String::new(),
        title_include: None,
        title_exclude: None,
        split_max_duration_sec: None,
        split_max_size_mb: None,
        notify_live: None,
        notify_error: None,
        created_at: timestamp,
        updated_at: timestamp,
        platform_uid: crate::db::platform_uid_of(&info),
        // 手动添加的房间不保证在关注列表里，继续走单房检查。
        batch_follow: false,
        internal_room_id: crate::db::internal_room_id_of(&info),
    };

    let room = state.db.insert_room(&draft, &info)?;
    tracing::info!(
        target: "hivecast_lib::commands",
        room_id = %room.id,
        platform = %room.platform,
        room_key = %room.room_key,
        monitor = !room.monitor_paused,
        auto_record = room.auto_record,
        "添加房间"
    );
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());

    if room.is_live && room.auto_record {
        if let Err(err) = state.recorder.start(&app, &state.broker, &room).await {
            tracing::warn!(target: "hivecast_lib::commands", room_id = %room.id, error = %err, "添加房间后自动开录失败");
            state.recorder.set_error(&room.id, err.to_string());
        }
    }

    Ok(AddRoomResult {
        recording: state.recorder.active(&room.id),
        error: state.recorder.last_error(&room.id),
        room,
        created: true,
    })
}

/// 当前登录的 B 站账号 UID；关注列表只能用自己的账号拉。
fn bilibili_uid(state: &AppState) -> Result<String, AppError> {
    state
        .db
        .credentials()?
        .into_iter()
        .find(|row| row.platform == "bilibili" && !row.uid.trim().is_empty())
        .map(|row| row.uid)
        .ok_or_else(|| AppError::User("未登录 B 站，请先在设置中登录".into()))
}

/// B 站接口的 -101 统一翻译成「重新登录」，其余错误保持原样。
fn map_bilibili_error(err: crate::platform::PlatformError) -> AppError {
    match err {
        crate::platform::PlatformError::Upstream { code: -101, .. } => {
            AppError::User("B 站登录已失效，请重新扫码登录".into())
        }
        other => other.into(),
    }
}

#[tauri::command]
pub async fn bilibili_followings(
    state: State<'_, AppState>,
    page: Option<u32>,
) -> Result<FollowingPage, AppError> {
    // 关注列表的分页状态由前端持有，这里只负责转成稳定 ID。
    let uid = bilibili_uid(&state)?;
    crate::platform::bilibili::fetch_followings(&uid, page.unwrap_or(1))
        .await
        .map_err(map_bilibili_error)
}

#[tauri::command]
pub async fn bilibili_follow_statuses(uids: Vec<String>) -> Result<Vec<LiveStatus>, AppError> {
    crate::platform::bilibili::fetch_live_statuses(&uids)
        .await
        .map_err(map_bilibili_error)
}

/// 斗鱼接口的 -1 通常表示登录态失效；其余错误保持原样。
fn map_douyu_error(err: crate::platform::PlatformError) -> AppError {
    match err {
        crate::platform::PlatformError::Upstream { code: -1, .. } => {
            AppError::User("斗鱼登录已失效，请重新扫码登录".into())
        }
        other => other.into(),
    }
}

#[tauri::command]
pub async fn douyu_follow_page(page: Option<u32>) -> Result<DouyuFollowingPage, AppError> {
    crate::platform::douyu_follow::fetch_follow_page(page.unwrap_or(1))
        .await
        .map_err(map_douyu_error)
}

/// 虎牙关注列表的一页。
#[tauri::command]
pub async fn huya_follow_page(page: Option<u32>) -> Result<HuyaFollowingPage, AppError> {
    crate::platform::huya_follow::fetch_follow_page(page.unwrap_or(1))
        .await
        .map_err(|err| match err {
            crate::platform::PlatformError::Upstream { code: -1, message }
                if message.contains("登录") =>
            {
                AppError::User(message)
            }
            other => other.into(),
        })
}

/// 抖音关注列表的一页；顺带标记哪些关注已经在关注池里。
#[tauri::command]
pub async fn douyin_follow_page(
    state: State<'_, AppState>,
    cursor: Option<String>,
) -> Result<DouyinFollowingPage, AppError> {
    let mut page = crate::platform::douyin_follow::fetch_follow_page(cursor.as_deref()).await?;
    let pool = state.db.list_douyin_follows()?;
    let by_uid: std::collections::HashMap<&str, &DouyinFollow> =
        pool.iter().map(|item| (item.uid.as_str(), item)).collect();
    for item in &mut page.items {
        if let Some(saved) = by_uid.get(item.uid.as_str()) {
            item.in_pool = true;
            item.promoted_room_id = saved.promoted_room_id.clone();
        }
    }
    Ok(page)
}

/// 把暂时没有直播间的抖音关注放进关注池，开播后由后台扫描自动转成房间。
#[tauri::command]
pub fn douyin_pool_add(
    state: State<'_, AppState>,
    items: Vec<DouyinFollow>,
    monitor: Option<bool>,
    auto_record: Option<bool>,
) -> Result<DouyinPoolAddResult, AppError> {
    let monitor = monitor.unwrap_or(true);
    let auto_record = monitor && auto_record.unwrap_or(false);
    let existing: std::collections::HashSet<String> = state
        .db
        .list_douyin_follows()?
        .into_iter()
        .map(|item| item.uid)
        .collect();
    let mut added = 0u32;
    let mut updated = 0u32;
    for item in items {
        if item.uid.trim().is_empty() {
            continue;
        }
        if existing.contains(&item.uid) {
            updated += 1;
        } else {
            added += 1;
        }
        state.db.upsert_douyin_follow(&item, monitor, auto_record)?;
    }
    tracing::info!(
        target: "hivecast_lib::commands",
        added,
        updated,
        monitor,
        auto_record,
        "抖音关注加入关注池"
    );
    Ok(DouyinPoolAddResult { added, updated })
}

/// 批量导入关注的主播：只写本地库，不逐房间请求平台。
///
/// 已经存在的房间只刷新展示字段，不覆盖用户设置的画质/线路/备注/自动录制；
/// 没有直播间的关注记为失败，而不是当成离线房间加进来。
fn import_room_info(
    item: &ImportRoomInput,
    platform: &str,
    uid: &str,
    uname: &str,
) -> crate::platform::RoomInfo {
    // 两个平台的 extra 是刷新时回填状态的缓存，键不能混用：
    // 斗鱼没有 UID，轮播标记必须保留；B 站继续用 UID 做批量状态映射。
    let extra = if platform == "douyu" {
        serde_json::json!({
            "roomId": item.room_id.trim(),
            "liveStatus": if item.is_live { 1 } else { 0 },
            "videoLoop": item.video_loop.unwrap_or(false),
        })
    } else if platform == "douyin" {
        // 抖音的 room_id 是页面 web_rid，批量状态要用内部 19 位号，不能混写。
        serde_json::json!({
            "uid": uid,
            "roomId": item.internal_room_id.as_deref().unwrap_or_default(),
            "shortId": item.short_id,
            "liveStatus": if item.is_live { 1 } else { 0 },
        })
    } else if platform == "huya" {
        // 虎牙批量按账号 UID 匹配；数字房号就是 room_key，不再重复写 internal。
        serde_json::json!({
            "uid": uid,
            "shortId": item.short_id,
            "liveStatus": if item.is_live { 1 } else { 0 },
        })
    } else {
        serde_json::json!({
            "uid": uid,
            "roomId": item.room_id.trim(),
            "shortId": item.short_id,
            "liveStatus": if item.is_live { 1 } else { 0 },
        })
    };
    crate::platform::RoomInfo {
        host_name: uname.to_string(),
        room_name: item.title.clone(),
        avatar_url: item.avatar_url.clone(),
        cover_url: item.cover_url.clone(),
        area_name: item.area_name.clone(),
        is_live: item.is_live,
        started_at: item.started_at,
        online: item.online,
        stable_id: Some(item.room_id.trim().to_string()),
        extra,
    }
}

/// 把导入时命中的别名/UID 行折叠到数字房号行。
///
/// 虎牙和 B 站都有「别名 + 数字房号」两种写法。导入只能修复这一次命中的行；
/// 如果库里同时有软删的数字行和活跃的别名行，必须按活跃者优先合并，
/// 否则导入后仍会留下两条指向同一场直播的记录。
fn fold_import_alias(
    db: &crate::db::Db,
    known: &mut Option<RoomRecord>,
    alias: RoomRecord,
    room_id: &str,
    canonical_url: &str,
    uid: &str,
) -> Result<(u32, u32), AppError> {
    if known.as_ref().is_some_and(|room| room.id == alias.id) {
        return Ok((0, 0));
    }
    let mut merged = 0;
    let mut rekeyed = 0;
    match known.take() {
        None => {
            if alias.room_key != room_id {
                db.rekey_room(&alias.id, room_id, canonical_url)?;
                rekeyed = 1;
            }
            *known = db.get_room(&alias.id)?;
        }
        Some(primary) => {
            let primary_active = !db.room_removed(&primary.id)?;
            let alias_active = !db.room_removed(&alias.id)?;
            let (keep, drop) = if !primary_active && alias_active {
                (&alias, &primary)
            } else {
                (&primary, &alias)
            };
            db.merge_room(&keep.id, &drop.id)?;
            merged = 1;
            tracing::info!(
                target: "hivecast_lib::commands",
                keep = %keep.id,
                keep_key = %keep.room_key,
                drop = %drop.id,
                drop_key = %drop.room_key,
                uid,
                "导入关注：合并重复房间"
            );
            if keep.room_key != room_id {
                db.rekey_room(&keep.id, room_id, canonical_url)?;
                rekeyed = 1;
            }
            *known = db.get_room(&keep.id)?;
        }
    }
    Ok((merged, rekeyed))
}
#[tauri::command]
pub async fn rooms_import(
    app: AppHandle,
    state: State<'_, AppState>,
    items: Vec<ImportRoomInput>,
    monitor: Option<bool>,
    auto_record: Option<bool>,
) -> Result<ImportRoomsResult, AppError> {
    let monitor = monitor.unwrap_or(true);
    let auto_record = monitor && auto_record.unwrap_or(false);
    let settings = crate::settings::load(&state.db);
    let timestamp = now_ts();

    let import_started = Instant::now();
    let total = items.len();
    let mut added = 0u32;
    let mut existing = 0u32;
    let mut restored = 0u32;
    let mut merged = 0u32;
    let mut rekeyed = 0u32;
    let mut upgraded = 0u32;
    let mut failed = Vec::new();
    let mut started: Vec<RoomRecord> = Vec::new();

    for item in items {
        // 缺省按 B 站处理，兼容旧前端；斗鱼没有独立 UID，房间号就是稳定键。
        let platform = item.platform.as_deref().unwrap_or("bilibili");
        let is_bilibili = platform == "bilibili";
        let uid = item.uid.trim();
        let room_id = item.room_id.trim();
        let identity = if is_bilibili && !uid.is_empty() {
            uid
        } else {
            room_id
        };
        let uname = if item.uname.trim().is_empty() {
            if uid.is_empty() {
                room_id.to_string()
            } else {
                uid.to_string()
            }
        } else {
            item.uname.trim().to_string()
        };
        if room_id.is_empty() {
            failed.push(ImportFailure {
                uid: identity.to_string(),
                uname,
                message: "没有可用的直播间".into(),
            });
            continue;
        };

        // B 站同一个主播可能已经用短号加过：先按真实房间号查，再按 UID 兜底。
        // 两边都命中时合并，避免导入后又留下两条指向同一直播间的记录。
        // 斗鱼没有短号别名，直接按房间号查。
        let canonical_url = match platform {
            "bilibili" => format!("https://live.bilibili.com/{room_id}"),
            "douyu" => format!("https://www.douyu.com/{room_id}"),
            "douyin" => format!("https://live.douyin.com/{room_id}"),
            "huya" => format!("https://www.huya.com/{room_id}"),
            _ => format!("https://live.bilibili.com/{room_id}"),
        };
        let mut known = state.db.find_room(platform, room_id)?;
        // 虎牙房间同时有数字房号、别名（如 lpl）和账号 UID。用户可能先按别名
        // 手动加过，导入关注时要把它改挂到数字房号，再补 UID 才能进批量。
        if platform == "huya" {
            if known.is_none() {
                if let Some(short) = item
                    .short_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    known = state.db.find_room(platform, short)?;
                }
            }
            if known.is_none() {
                known = state.db.find_room_by_uid(platform, uid)?;
            }
            // 别名行与数字行可能同时存在（手动加过别名、后来又加过数字房号，
            // 或数字行已软删而别名仍活跃）。逐条折叠，活跃者优先保留。
            // short_id 与 UID 可能指向同一行；去重后再折叠，避免第二次合并
            // 拿着已经删掉的 drop 行去迁移会话。
            let mut aliases: Vec<RoomRecord> = Vec::new();
            if let Some(short) = item
                .short_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if let Some(row) = state.db.find_room(platform, short)? {
                    aliases.push(row);
                }
            }
            if let Some(row) = state.db.find_room_by_uid(platform, uid)? {
                if !aliases.iter().any(|existing| existing.id == row.id) {
                    aliases.push(row);
                }
            }
            for alias in aliases {
                let (m, r) =
                    fold_import_alias(&state.db, &mut known, alias, room_id, &canonical_url, uid)?;
                merged += m;
                rekeyed += r;
            }
        }
        if is_bilibili {
            if let Some(alias) = state.db.find_room_by_uid(platform, uid)? {
                if known.as_ref().is_none_or(|room| room.id != alias.id) {
                    match known.take() {
                        None => {
                            state.db.rekey_room(&alias.id, room_id, &canonical_url)?;
                            rekeyed += 1;
                            tracing::info!(
                                target: "hivecast_lib::commands",
                                room_id = %alias.id,
                                room_key = room_id,
                                uid,
                                "导入关注：短号行改挂真实房间号"
                            );
                            known = state.db.get_room(&alias.id)?;
                        }
                        Some(primary) => {
                            // 短号行可能已经软删、而 UID 行还在用：这时保留仍在
                            // 使用的那条，把它的设置当作 keep，不能反过来吞掉。
                            let primary_active = !state.db.room_removed(&primary.id)?;
                            let alias_active = !state.db.room_removed(&alias.id)?;
                            let (keep, drop) = if !primary_active && alias_active {
                                (&alias, &primary)
                            } else {
                                (&primary, &alias)
                            };
                            state.db.merge_room(&keep.id, &drop.id)?;
                            merged += 1;
                            tracing::info!(
                                target: "hivecast_lib::commands",
                                keep = %keep.id,
                                keep_key = %keep.room_key,
                                drop = %drop.id,
                                drop_key = %drop.room_key,
                                uid,
                                "导入关注：合并重复房间"
                            );
                            if keep.room_key != room_id {
                                state.db.rekey_room(&keep.id, room_id, &canonical_url)?;
                                rekeyed += 1;
                            }
                            known = state.db.get_room(&keep.id)?;
                        }
                    }
                }
            }
        }
        if let Some(room) = known {
            if room.room_key != room_id {
                state.db.rekey_room(&room.id, room_id, &canonical_url)?;
                rekeyed += 1;
            }
            let info = import_room_info(&item, platform, uid, &uname);
            // 软删除的行不在界面上，导入时必须恢复，否则会误报「已存在」。
            if state.db.room_removed(&room.id)? {
                let draft = RoomRecord {
                    room_key: room_id.to_string(),
                    canonical_url: canonical_url.clone(),
                    auto_record,
                    monitor_paused: !monitor,
                    platform_uid: if is_bilibili || platform == "douyin" || platform == "huya" {
                        Some(uid.to_string())
                    } else {
                        None
                    },
                    batch_follow: platform == "douyin" || platform == "huya",
                    internal_room_id: crate::db::internal_room_id_of(&info),
                    updated_at: timestamp,
                    ..room
                };
                match state.db.insert_room(&draft, &info) {
                    Ok(restored_room) => {
                        added += 1;
                        restored += 1;
                        tracing::info!(
                            target: "hivecast_lib::commands",
                            room_id = %restored_room.id,
                            room_key = %restored_room.room_key,
                            uid,
                            "导入关注：恢复已移除房间"
                        );
                        if restored_room.is_live && restored_room.auto_record {
                            started.push(restored_room);
                        }
                    }
                    Err(err) => failed.push(ImportFailure {
                        uid: identity.to_string(),
                        uname,
                        message: err.to_string(),
                    }),
                }
            } else {
                // 单行刷新失败不应该让整批导入停在半路。
                match state.db.update_room_info(&room.id, &info) {
                    Ok(()) => {
                        existing += 1;
                        // 先手动添加、后导入关注：房间已存在，但 batch_follow 还是 0。
                        // 这里补标记，让它下一次调度就进 follow/feed 批量。
                        if platform == "douyin" {
                            let internal = crate::db::internal_room_id_of(&info);
                            if !room.batch_follow
                                || room.platform_uid.as_deref() != Some(uid)
                                || room.internal_room_id.is_none()
                            {
                                match state.db.mark_douyin_batch_room(
                                    &room.id,
                                    Some(uid),
                                    internal.as_deref(),
                                ) {
                                    Ok(()) => {
                                        upgraded += 1;
                                        tracing::info!(
                                            target: "hivecast_lib::commands",
                                            room_id = %room.id,
                                            room_key = room_id,
                                            uid,
                                            internal_room_id = internal.as_deref().unwrap_or_default(),
                                            "导入关注：已有抖音房间加入批量状态"
                                        );
                                    }
                                    Err(err) => tracing::warn!(
                                        target: "hivecast_lib::commands",
                                        room_id = %room.id,
                                        error = %err,
                                        "导入关注：补写批量标记失败"
                                    ),
                                }
                            }
                        }
                        // 虎牙：先按别名手动添加的房间此时已 rekey 到数字房号，
                        // 补 batch_follow 与平台 UID 后就能进关注列表批量。
                        if platform == "huya"
                            && (!room.batch_follow || room.platform_uid.as_deref() != Some(uid))
                        {
                            match state
                                .db
                                .mark_platform_batch_room(&room.id, "huya", Some(uid))
                            {
                                Ok(()) => {
                                    upgraded += 1;
                                    tracing::info!(
                                        target: "hivecast_lib::commands",
                                        room_id = %room.id,
                                        room_key = room_id,
                                        uid,
                                        "导入关注：已有虎牙房间加入批量状态"
                                    );
                                }
                                Err(err) => tracing::warn!(
                                    target: "hivecast_lib::commands",
                                    room_id = %room.id,
                                    error = %err,
                                    "导入关注：补写虎牙批量标记失败"
                                ),
                            }
                        }
                    }
                    Err(err) => failed.push(ImportFailure {
                        uid: identity.to_string(),
                        uname,
                        message: err.to_string(),
                    }),
                }
            }
            continue;
        }
        let draft = RoomRecord {
            id: uuid::Uuid::new_v4().to_string(),
            platform: platform.to_string(),
            room_key: room_id.to_string(),
            canonical_url,
            title: item.title.clone(),
            host_name: uname.clone(),
            avatar_url: item.avatar_url.clone(),
            cover_url: item.cover_url.clone(),
            area_name: item.area_name.clone(),
            is_live: item.is_live,
            started_at: item.started_at,
            online: item.online,
            auto_record,
            monitor_paused: !monitor,
            record_qn: 0,
            view_qn: settings.default_view_qn,
            record_line: None,
            last_checked_at: Some(timestamp),
            last_session_id: None,
            last_session_started_at: None,
            last_session_duration_ms: 0,
            last_session_status: None,
            last_session_reason: None,
            last_session_file_size: 0,
            last_session_quality: None,
            removed_at: None,
            note: String::new(),
            tags: String::new(),
            title_include: None,
            title_exclude: None,
            split_max_duration_sec: None,
            split_max_size_mb: None,
            notify_live: None,
            notify_error: None,
            created_at: timestamp,
            updated_at: timestamp,
            platform_uid: if is_bilibili || platform == "douyin" || platform == "huya" {
                Some(uid.to_string())
            } else {
                None
            },
            batch_follow: platform == "douyin" || platform == "huya",
            internal_room_id: None,
        };
        let info = import_room_info(&item, platform, uid, &uname);
        match state.db.insert_room(&draft, &info) {
            Ok(room) => {
                added += 1;
                tracing::debug!(
                    target: "hivecast_lib::commands",
                    room_id = %room.id,
                    room_key = %room.room_key,
                    platform,
                    uid,
                    "导入关注：新建房间"
                );
                if room.is_live && room.auto_record {
                    started.push(room);
                }
            }
            Err(err) => failed.push(ImportFailure {
                uid: identity.to_string(),
                uname,
                message: err.to_string(),
            }),
        }
    }

    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    tracing::info!(
        target: "hivecast_lib::commands",
        total,
        added,
        existing,
        restored,
        merged,
        rekeyed,
        upgraded,
        failed = failed.len(),
        monitor,
        auto_record,
        elapsed_ms = import_started.elapsed().as_millis() as u64,
        "批量导入关注"
    );
    for failure in &failed {
        tracing::warn!(
            target: "hivecast_lib::commands",
            uid = %failure.uid,
            uname = %failure.uname,
            error = %failure.message,
            "导入关注跳过"
        );
    }
    for room in started {
        if let Err(err) = state
            .recorder
            .start_checked(&app, &state.broker, &room)
            .await
        {
            tracing::warn!(target: "hivecast_lib::commands", room_id = %room.id, error = %err, "导入后自动开录失败");
            state.recorder.set_error(&room.id, err.to_string());
        }
    }

    Ok(ImportRoomsResult {
        added,
        existing,
        failed,
    })
}

#[tauri::command]
pub async fn room_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, "移除房间");
    if state.recorder.is_recording(&id) {
        if let Err(err) = state.recorder.stop(&app, &id, "stopped").await {
            tracing::warn!(target: "hivecast_lib::commands", room_id = %id, error = %err, "移除房间时停止录制失败");
        }
    }
    state.view.close_room(&app, &id).await;
    // 停止录制会顺手打上「手动停录」标记；房间都移除了，别让它跨到下次重新添加。
    state.recorder.resume_auto(&id);
    // 关注池项一起删掉：只解绑会让池扫描把刚移除的房间重新转正。
    // 用户想恢复关注批量，重新导入关注即可。
    let follow_removed = state.db.delete_douyin_follow_for_room(&id)?;
    state.db.delete_room(&id)?;
    if follow_removed > 0 {
        tracing::info!(
            target: "hivecast_lib::commands",
            room_id = %id,
            follow_removed,
            "移除房间：同时删除关注池项"
        );
    }
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
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, "手动刷新房间");
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    let reference = room_ref(&room);
    state.broker.invalidate(&reference);
    let info = state.broker.room_info(&reference).await?;
    tracing::debug!(target: "hivecast_lib::commands", room_id = %id, live = info.is_live, "房间刷新完成");
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
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, enabled, "设置自动录制");
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    if enabled && room.monitor_paused {
        return Err(AppError::User(
            "监控已暂停，恢复监控后才能开启自动录制".into(),
        ));
    }
    state.db.set_auto_record(&id, enabled)?;

    if enabled {
        state.recorder.resume_auto(&id);
    }
    if enabled
        && room.is_live
        && !room.monitor_paused
        && !state.recorder.is_recording(&id)
        && crate::monitor::auto_record_allowed(&room, &room.title)
    {
        // 走 start_checked：用户从关闭到再次打开可能只隔一瞬，拿锁后必须
        // 重新读库确认自动录制仍然是开的，否则这次开录会盖掉刚做的关闭。
        if let Err(err) = state
            .recorder
            .start_checked(&app, &state.broker, &room)
            .await
        {
            tracing::warn!(target: "hivecast_lib::commands", room_id = %id, error = %err, "开启自动录制后开录失败");
            state.recorder.set_error(&id, err.to_string());
        }
    } else if !enabled {
        // 解析线路可能已经跑了几秒：先打取消标记，让在途的自动开录在拉 ffmpeg 前退出。
        state.recorder.cancel_start(&id);
        if state.recorder.is_recording(&id) {
            if let Err(err) = state.recorder.stop(&app, &id, "stopped").await {
                tracing::warn!(target: "hivecast_lib::commands", room_id = %id, error = %err, "关闭自动录制时停止录制失败");
            }
        }
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
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, paused, "设置房间监控");
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
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, record_qn, view_qn, "设置房间画质");
    state.db.set_quality(&id, record_qn, view_qn)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

/// 单房间规则/备注：一次写回，前端保存按钮触发。
// 前端一次提交完整规则表，保持扁平参数与 IPC 形状一致。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn room_set_rules(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    note: String,
    tags: String,
    title_include: Option<String>,
    title_exclude: Option<String>,
    split_max_duration_sec: Option<i64>,
    split_max_size_mb: Option<i64>,
    notify_live: Option<bool>,
    notify_error: Option<bool>,
) -> Result<(), AppError> {
    let clean = |value: Option<String>| {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    state.db.set_room_rules(
        &id,
        note.trim(),
        tags.trim(),
        clean(title_include).as_deref(),
        clean(title_exclude).as_deref(),
        split_max_duration_sec.map(|value| value.clamp(0, 86_400)),
        split_max_size_mb.map(|value| value.clamp(0, 1_048_576)),
        notify_live,
        notify_error,
    )?;
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
    // 房间画质为 0 表示跟随全局；线路列表也要用真正会用于录制的档位，
    // 否则用户看到的是 qn=0 对应的平台默认档，和实际开录画质对不上。
    let settings = crate::settings::load(&state.db);
    let requested_qn = match qn {
        Some(qn) => qn,
        None => crate::settings::effective_record_qn(settings.default_record_qn, room.record_qn),
    };
    let plan = state.broker.plan(&room_ref(&room), requested_qn).await?;
    tracing::debug!(target: "hivecast_lib::commands", room_id = %id, groups = plan.groups.len(), "查询房间线路");
    Ok(line_infos(&plan.groups))
}

/// 当前房间的画质与线路能力：线路与画质来自同一次解析，前端不再维护平台映射表。
#[tauri::command]
pub async fn room_stream_options(
    state: State<'_, AppState>,
    id: String,
    qn: Option<u32>,
) -> Result<StreamOptions, AppError> {
    let room = state
        .db
        .get_room(&id)?
        .ok_or_else(|| AppError::Internal("房间不存在".into()))?;
    let settings = crate::settings::load(&state.db);
    let requested_qn = match qn {
        Some(qn) => qn,
        None => crate::settings::effective_record_qn(settings.default_record_qn, room.record_qn),
    };
    let plan = state.broker.plan(&room_ref(&room), requested_qn).await?;
    tracing::debug!(
        target: "hivecast_lib::commands",
        room_id = %id,
        groups = plan.groups.len(),
        qualities = plan.qualities.len(),
        "查询房间画质与线路"
    );
    Ok(StreamOptions {
        qualities: plan.qualities.clone(),
        lines: line_infos(&plan.groups),
    })
}

fn line_infos(groups: &[crate::platform::StreamGroup]) -> Vec<LineInfo> {
    groups
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
        .collect()
}

/// 指定录制线路；传 None 回到自动选择。只影响下一次开录/续录，不打断当前录制。
#[tauri::command]
pub async fn room_set_line(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    line: Option<String>,
) -> Result<(), AppError> {
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, line = ?line, "设置录制线路");
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
        tracing::warn!(target: "hivecast_lib::commands", room_id = %id, "手动开录被拒绝：房间未开播");
        return Err(AppError::Internal("房间未开播".into()));
    }
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, "手动开始录制");
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
    tracing::info!(target: "hivecast_lib::commands", room_id = %id, "手动停止录制");
    state.recorder.stop(&app, &id, "stopped").await?;
    Ok(())
}

#[tauri::command]
pub async fn sessions_list(
    state: State<'_, AppState>,
    room_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<RecordSession>, AppError> {
    state.db.list_sessions(
        room_id.as_deref(),
        limit.unwrap_or(100) as usize,
        offset.unwrap_or(0) as usize,
    )
}

#[tauri::command]
pub async fn session_summary(
    state: State<'_, AppState>,
    room_id: String,
) -> Result<SessionSummary, AppError> {
    state.db.session_summary(&room_id)
}

/// 录制中的文件增长与磁盘剩余；详情抽屉打开时按 3 秒轮询。
#[tauri::command]
pub async fn recording_usage(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<RecordingUsage, AppError> {
    let session = state
        .db
        .get_session(&session_id)?
        .ok_or_else(|| AppError::User("录像不存在".into()))?;
    let path = session.file_path.map(std::path::PathBuf::from);
    // 录制中 ffmpeg 用 segment 封装，落盘的是 .partNNNN.ts，主文件要等收尾合并后才出现；
    // 只 stat 主文件会让详情抽屉在整场录制期间一直显示「已写入 —」。两边取较大值，收尾后互不重复。
    let bytes = path
        .as_deref()
        .map(|path| {
            let master = std::fs::metadata(path)
                .map(|meta| meta.len() as i64)
                .unwrap_or(0);
            let parts = crate::segment::parts_total(path) as i64;
            master.max(parts)
        })
        .unwrap_or(0);
    let dir = path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| state.recorder.media_root());
    let disk_free = available_space(&dir).unwrap_or(0) as i64;
    Ok(RecordingUsage { bytes, disk_free })
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
        tracing::warn!(target: "hivecast_lib::commands", room_id = %room_id, "观看请求被拒绝：房间未开播");
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
    tracing::info!(target: "hivecast_lib::commands", room_id = %room_id, qn = ?qn, "打开观看");
    let slot = state.view.open(&app, state.broker.clone(), &room).await?;
    tracing::info!(target: "hivecast_lib::commands", room_id = %room_id, slot = slot.slot, "观看已打开");
    Ok(slot)
}

#[tauri::command]
pub async fn view_close(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
) -> Result<(), AppError> {
    tracing::debug!(target: "hivecast_lib::commands", slot, "关闭观看槽位");
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
        tracing::warn!(target: "hivecast_lib::commands", room_id = %room_id, slot, "替换观看被拒绝：房间未开播");
        return Err(AppError::User("房间未开播".into()));
    }
    tracing::info!(target: "hivecast_lib::commands", room_id = %room_id, slot, "替换观看槽位");
    state
        .view
        .replace(&app, state.broker.clone(), slot, &room)
        .await
}

/// 离开观看页：停掉全部观看管道，保留房间与布局，回来时再按布局恢复。
#[tauri::command]
pub async fn view_suspend(app: AppHandle, state: State<'_, AppState>) -> Result<(), AppError> {
    tracing::info!(target: "hivecast_lib::commands", "离开观看页，挂起全部观看");
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
    tracing::info!(target: "hivecast_lib::commands", slot, qn, "切换观看画质");
    state.view.set_quality(&app, state.broker.clone(), slot, qn)
}

#[tauri::command]
pub fn view_set_line(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
    line: Option<String>,
) -> Result<ViewSlotInfo, AppError> {
    tracing::info!(target: "hivecast_lib::commands", slot, line = ?line, "切换观看线路");
    state.view.set_line(&app, state.broker.clone(), slot, line)
}

#[tauri::command]
pub async fn view_retry(
    app: AppHandle,
    state: State<'_, AppState>,
    slot: u8,
) -> Result<ViewSlotInfo, AppError> {
    tracing::info!(target: "hivecast_lib::commands", slot, "重试观看");
    state.view.retry(&app, state.broker.clone(), slot).await
}

// ---- M5 本地后处理 ----

fn session_file(state: &AppState, session_id: &str) -> Result<RecordSession, AppError> {
    state
        .db
        .get_session(session_id)?
        .ok_or_else(|| AppError::Internal("录像不存在".into()))
}

fn file_label(path: &Path) -> String {
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
    // 任务由 post::run 统一登记到任务中心，命令层不再重复登记。
    tracing::info!(target: "hivecast_lib::commands", session_id = %session_id, file = %path.display(), "开始转封装");
    // 手动转封装与自动后处理走同一条 convert 实现：临时文件、ffprobe 校验、
    // 进度、取消、磁盘预检都一致；唯一区别是手动永远保留源封装。
    let settings = crate::settings::load(&state.db);
    let mut cfg = crate::post::PostConfig::from_settings(&settings);
    cfg.mode = crate::settings::POST_MODE_CONVERT.to_string();
    cfg.convert_format = "mp4".into();
    cfg.delete_source = false;
    let task_id = format!("manual:{session_id}");
    let outcome = crate::post::run(&task_id, &session_id, &path, &cfg, Some(&app)).await;
    if outcome.state != crate::post::STATE_DONE {
        let reason = outcome.error.unwrap_or_else(|| "转封装失败".to_string());
        // 失败/取消也要落库，媒体库才能给出可重试的徽标；主文件保持在原位置。
        let kept_path = session
            .file_path
            .clone()
            .unwrap_or_else(|| path.display().to_string());
        let _ = state.db.update_session_post_file(
            &session_id,
            &kept_path,
            session.file_size,
            session.source_path.as_deref(),
            "convert",
            &outcome.state,
            session.source_deleted,
            Some(&reason),
        );
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        return Err(if outcome.state == crate::post::STATE_CANCELLED {
            AppError::User(reason)
        } else {
            AppError::Ffmpeg(reason)
        });
    }
    // 手动转封装保留源：第一次转封装时把原文件登记为母带，主文件切到新 mp4。
    // 已经存在 source_path 时沿用原状态，不能把用户「仅删源」的结果重置成未删，
    // 否则媒体库会显示一个已经不在回收站外的源文件。
    let switched = outcome.path != path;
    let source_path = session
        .source_path
        .clone()
        .or_else(|| switched.then(|| path.display().to_string()));
    let source_deleted = source_path.is_some() && session.source_deleted;
    state.db.update_session_post_file(
        &session_id,
        &outcome.path.display().to_string(),
        outcome.size,
        source_path.as_deref(),
        "convert",
        "done",
        source_deleted,
        None,
    )?;
    tracing::info!(
        target: "hivecast_lib::commands",
        session_id = %session_id,
        out = %outcome.path.display(),
        source = ?source_path,
        "手动转封装完成并切换主文件"
    );
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(PostResult {
        kind: "remux".into(),
        path: outcome.path.display().to_string(),
        size: outcome.size,
        message: Some("已转封装为 MP4，源封装已保留".into()),
    })
}

/// 手动重试一次失败的录制收尾；只处理该场次留下的分割清单。
#[tauri::command]
pub async fn finalize_retry(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), AppError> {
    let session = state
        .db
        .get_session(&session_id)?
        .ok_or_else(|| AppError::User("这段录像不存在，可能已被清理".into()))?;
    // 先把「没有可恢复清单」这类确定失败挡在任务创建之前，避免留下一条
    // 永远不会成功的 finalize 失败任务，进而变成常驻的「重试收尾」异常。
    if session.status == "recording" {
        return Err(AppError::User("这段还在录制，先停下再重试收尾".into()));
    }
    let manifest = state.db.split_manifest_for(&session_id)?;
    let manifest_ready = manifest
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty() && Path::new(path).is_file());
    if !manifest_ready {
        return Err(AppError::User(
            "这段录像没有待恢复的收尾清单，可能已经收尾完成".into(),
        ));
    }
    // 先占清单锁再登记任务：另一个窗口正在重试时直接返回，不留下假的失败任务。
    let manifest = manifest.expect("manifest_ready 已确认清单存在");
    let lease = crate::recorder::acquire_finalize_retry(&manifest)?;
    let room_id = Some(session.room_id.clone());
    let title = room_id
        .as_deref()
        .and_then(|id| state.db.get_room(id).ok().flatten())
        .map(|room| room.host_name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "重试录制收尾".into());
    let job = state.jobs.begin_for(
        "finalize",
        title,
        Some("重新合并分片并执行后处理".into()),
        Some(session_id.clone()),
        room_id,
    );
    job.progress(5, "finalizing", Some("重新合并分片并执行后处理"));
    match crate::recorder::retry_finalize_with_lease(&state.db, &session_id, lease).await {
        Ok(()) => {
            job.finish("done", Some("收尾完成"));
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            Ok(())
        }
        Err(err) => {
            job.finish("failed", Some(&err.to_string()));
            Err(err)
        }
    }
}

/// 重试一条失败/取消的后处理：仍按当前设置执行，成功的派生文件重新写库。
#[tauri::command]
pub async fn post_retry(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PostResult, AppError> {
    let session = session_file(&state, &session_id)?;
    if session.status == "recording" {
        return Err(AppError::User("这段还在录制，先停下再处理".into()));
    }
    if !matches!(
        session.post_state.as_str(),
        crate::post::STATE_FAILED | crate::post::STATE_CANCELLED
    ) {
        return Err(AppError::User("只有失败或已取消的后处理才能重试".into()));
    }
    let source = session
        .source_path
        .clone()
        .or_else(|| session.file_path.clone())
        .ok_or_else(|| AppError::User("这段录像没有可处理的源文件".into()))?;
    let source_path = PathBuf::from(&source);
    if !source_path.is_file() {
        return Err(AppError::User("源封装文件已不存在，无法重试".into()));
    }
    let settings = crate::settings::load(&state.db);
    let mut cfg = crate::post::PostConfig::from_settings(&settings);
    if matches!(
        session.post_mode.as_str(),
        crate::settings::POST_MODE_SOURCE
            | crate::settings::POST_MODE_CONVERT
            | crate::settings::POST_MODE_CUSTOM
    ) {
        cfg.mode = session.post_mode.clone();
    }
    let task_id = format!("manual:{session_id}:retry");
    let outcome = crate::post::run(&task_id, &session_id, &source_path, &cfg, Some(&app)).await;
    let mode = cfg.mode.clone();
    if outcome.state == crate::post::STATE_DONE {
        let main = outcome.path.display().to_string();
        let keep_source = if outcome.path == source_path {
            session.source_path.clone()
        } else {
            Some(source.clone())
        };
        state.db.update_session_post_file(
            &session_id,
            &main,
            outcome.size,
            keep_source.as_deref(),
            &mode,
            &outcome.state,
            false,
            None,
        )?;
    } else {
        // 重试失败时主文件仍是源封装，只更新失败原因，不能把可播放路径改坏。
        state.db.update_session_post_file(
            &session_id,
            session.file_path.as_deref().unwrap_or(&source),
            session.file_size,
            session.source_path.as_deref(),
            &mode,
            &outcome.state,
            false,
            outcome.error.as_deref(),
        )?;
    }
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    if outcome.state != crate::post::STATE_DONE {
        // 失败/取消必须冒泡成错误：前端此前只按 Ok 提示成功，会把失败说成完成（文档 196-11）。
        let message = outcome.error.unwrap_or_else(|| "后处理未完成".to_string());
        return Err(if outcome.state == crate::post::STATE_CANCELLED {
            AppError::User(message)
        } else {
            AppError::Ffmpeg(message)
        });
    }
    Ok(PostResult {
        kind: "post".into(),
        path: outcome.path.display().to_string(),
        size: outcome.size,
        message: None,
    })
}

/// 取消一条正在排队/执行的后处理。
#[tauri::command]
pub fn post_cancel(session_id: String) -> usize {
    crate::post::cancel_session(&session_id)
}

/// 只删源封装母带：派生文件必须通过 ffprobe 校验，避免把唯一可播放文件删掉。
#[tauri::command]
pub async fn media_delete_source(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), AppError> {
    let session = session_file(&state, &session_id)?;
    if session.status == "recording" {
        return Err(AppError::User("这段还在录制，先停下再删源".into()));
    }
    let Some(main) = session.file_path.as_deref() else {
        return Err(AppError::User("这段录像没有主文件".into()));
    };
    let Some(source) = session.source_path.as_deref() else {
        return Err(AppError::User("这段录像没有独立的源封装".into()));
    };
    if source == main {
        return Err(AppError::User("源封装就是当前主文件，不能只删源".into()));
    }
    if !std::path::Path::new(source).is_file() {
        state.db.set_source_deleted(&session_id, true)?;
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        return Ok(());
    }
    crate::post::validate_media(std::path::Path::new(main))
        .await
        .map_err(|err| AppError::User(format!("主文件校验未通过，不能删除源封装：{err}")))?;
    state.local.stop(&session_id).await;
    crate::post::trash(std::path::Path::new(source)).map_err(AppError::Internal)?;
    state.db.set_source_deleted(&session_id, true)?;
    tracing::info!(target: "hivecast_lib::commands", session_id = %session_id, source, "已把源封装移入回收站");
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

/// 把某个房间已完成的分段按时间顺序合并成一个 mp4。
#[tauri::command]
pub async fn room_merge_sessions(
    state: State<'_, AppState>,
    room_id: String,
) -> Result<PostResult, AppError> {
    // 合并要覆盖整个房间的历史，分页读完（list_sessions 单页上限 500）。
    let mut all_sessions = Vec::new();
    let mut offset = 0;
    loop {
        let page = state.db.list_sessions(Some(&room_id), 500, offset)?;
        let count = page.len();
        all_sessions.extend(page);
        if count < 500 {
            break;
        }
        offset += count;
    }
    let sessions: Vec<_> = all_sessions
        .into_iter()
        .filter(|session| session.status != "recording" && session.file_path.is_some())
        // 合并产物本身也是 finished 会话，再整房合并时跳过，避免越合越多。
        .filter(|session| session.reason.as_deref() != Some("merge"))
        .collect();
    let plan = merge_plan(sessions);
    if !plan.missing.is_empty() {
        return Err(missing_files_error(&plan.missing));
    }
    let files = plan.files;
    // 弹幕偏移跟视频容器时长走；ffprobe 失败时才退回数据库时长。
    let durations = probe_merge_durations(&files).await;
    let sources = retime_danmaku_sources(plan.danmaku, &durations);
    let total_ms = actual_total_ms(&durations).unwrap_or(plan.total_ms);
    let title = state
        .db
        .get_room(&room_id)?
        .map(|room| room.title)
        .filter(|title| !title.trim().is_empty())
        .or_else(|| files.first().map(|path| file_label(path)))
        .unwrap_or_else(|| room_id.clone());
    let job = state.jobs.begin("merge", title, None);
    tracing::info!(target: "hivecast_lib::commands", room_id = %room_id, files = files.len(), "开始合并房间录像");
    let result = postprocess::merge(&files, None).await;
    match &result {
        Ok(merged) => {
            let danmaku = merge_danmaku_files(&sources, &merged.path);
            if let Err(err) =
                register_merged_session(&state.db, &room_id, merged, danmaku.as_deref(), total_ms)
            {
                let message = format!(
                    "合并完成但登记失败：{err}。成品已保存在 {}，请重试合并或手动导入",
                    merged.path
                );
                job.finish("failed", Some(&message));
                return Err(AppError::User(message));
            }
            job.finish("done", None);
        }
        Err(err) => job.finish("failed", Some(&err.to_string())),
    }
    result
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
        if session.reason.as_deref() == Some("merge") {
            return Err(AppError::User(
                "合并产物不能再参与合并，请选择原始录像".into(),
            ));
        }
        sessions.push(session);
    }
    // 缺文件的片段必须与它的时长一起剔除。旧实现先按全部片段累加时长推进
    // 弹幕偏移，再过滤视频，少一段就会让后面的弹幕整体错位。
    let plan = merge_plan(sessions);
    if !plan.missing.is_empty() {
        return Err(missing_files_error(&plan.missing));
    }
    let merged_room_id = plan.room_id.clone().unwrap_or_default();
    let files = plan.files;
    if files.len() < 2 {
        return Err(AppError::User("选中的录像里可用文件不足两段".into()));
    }
    // 弹幕偏移必须跟视频容器时长走，不能跟数据库里的墙钟时长走。
    // 分片按 60 秒收尾时，单个容器常比数据库多 0.2–0.5 秒；九段累计就能差出 7 秒，
    // 合并后后半段弹幕会整体晚到。ffprobe 拿不到时长时才退回数据库值。
    let durations = probe_merge_durations(&files).await;
    let sources = retime_danmaku_sources(plan.danmaku, &durations);
    let total_ms = if let Some(actual) = actual_total_ms(&durations) {
        actual
    } else {
        plan.total_ms
    };
    let job = state.jobs.begin("merge", file_label(&files[0]), None);
    tracing::info!(target: "hivecast_lib::commands", count = files.len(), "开始合并选中录像");
    let progress = postprocess::Progress {
        app: &app,
        task: "merge",
        total_ms,
    };
    let result = postprocess::merge(&files, Some(&progress)).await;
    match &result {
        Ok(merged) => {
            let danmaku = merge_danmaku_files(&sources, &merged.path);
            if let Err(err) = register_merged_session(
                &state.db,
                &merged_room_id,
                merged,
                danmaku.as_deref(),
                total_ms,
            ) {
                let message = format!(
                    "合并完成但登记失败：{err}。成品已保存在 {}，请重试合并或手动导入",
                    merged.path
                );
                job.finish("failed", Some(&message));
                return Err(AppError::User(message));
            }
            job.finish("done", None);
        }
        Err(err) => job.finish("failed", Some(&err.to_string())),
    }
    result
}

/// 把合并产物登记成一条普通录像，媒体库才能挂上弹幕轨、继续导出/删除。
fn register_merged_session(
    db: &crate::db::Db,
    room_id: &str,
    merged: &PostResult,
    danmaku: Option<&std::path::Path>,
    duration_ms: i64,
) -> Result<(), AppError> {
    let now = now_ts();
    let duration_ms = duration_ms.max(0);
    let session = RecordSession {
        id: uuid::Uuid::new_v4().to_string(),
        room_id: room_id.to_string(),
        started_at: now - duration_ms / 1000,
        ended_at: Some(now),
        status: "finished".into(),
        reason: Some("merge".into()),
        file_path: Some(merged.path.clone()),
        file_size: merged.size,
        duration_ms,
        quality: None,
        quality_history: Vec::new(),
        line: None,
        error: None,
        danmaku_path: danmaku.map(|path| path.to_string_lossy().to_string()),
        danmaku_count: danmaku.map(count_lines).unwrap_or(0),
        source_path: None,
        post_mode: "source".into(),
        post_state: "done".into(),
        source_deleted: false,
        post_error: None,
        group_id: None,
        clip_index: 0,
    };
    if let Err(err) = db.create_session(&session) {
        tracing::warn!(target: "hivecast_lib::commands", %err, "合并录像登记失败");
        return Err(err);
    }
    Ok(())
}

fn count_lines(path: &std::path::Path) -> i64 {
    std::fs::File::open(path)
        .map(std::io::BufReader::new)
        .map(|reader| {
            std::io::BufRead::lines(reader)
                .filter(|line| line.as_ref().is_ok_and(|line| !line.trim().is_empty()))
                .count() as i64
        })
        .unwrap_or(0)
}

/// 合并成功后把各段 JSONL 拼成一条，并导出同名 ASS。
/// 弹幕失败只记日志，不影响已经成功的视频合并。
fn merge_danmaku_files(sources: &[postprocess::DanmakuSource], merged: &str) -> Option<PathBuf> {
    let path = std::path::Path::new(merged);
    match postprocess::merge_danmaku(sources, path) {
        Ok(Some(jsonl)) => {
            match postprocess::danmaku_to_ass(&jsonl) {
                Ok(result) => {
                    tracing::info!(target: "hivecast_lib::commands", ass = %result.path, "合并弹幕字幕完成")
                }
                Err(err) => {
                    tracing::warn!(target: "hivecast_lib::commands", %err, "合并弹幕转 ASS 失败")
                }
            }
            Some(jsonl)
        }
        Ok(None) => {
            tracing::info!(target: "hivecast_lib::commands", "合并的录像没有弹幕文件");
            None
        }
        Err(err) => {
            tracing::warn!(target: "hivecast_lib::commands", %err, "合并弹幕失败");
            None
        }
    }
}

struct MergePlan {
    files: Vec<PathBuf>,
    danmaku: Vec<postprocess::DanmakuSource>,
    total_ms: i64,
    room_id: Option<String>,
    /// 数据库有记录、但磁盘上找不到的片段；调用方必须报错，不能静默少合一段。
    missing: Vec<String>,
}

/// 合并顺序按开录时间，同一秒内再按分段序号；与勾选顺序无关。
/// 视频、弹幕、时长必须来自同一批片段，否则少一段视频就会让后续弹幕错位。
/// 缺文件的片段不再静默跳过：记进 `missing`，由调用方中止整次合并。
fn merge_plan(mut sessions: Vec<RecordSession>) -> MergePlan {
    sessions.sort_by_key(|session| (session.started_at, session.clip_index));
    let room_id = sessions.first().map(|session| session.room_id.clone());
    let total_ms: i64 = sessions
        .iter()
        .map(|session| session.duration_ms.max(0))
        .sum();
    let danmaku = sessions
        .iter()
        .map(|session| postprocess::DanmakuSource {
            jsonl: session
                .danmaku_path
                .as_ref()
                .map(PathBuf::from)
                .filter(|path| path.is_file()),
            duration_ms: session.duration_ms.max(0),
        })
        .collect();
    let mut missing = Vec::new();
    let mut files = Vec::new();
    for session in sessions {
        match session.file_path {
            Some(path) if Path::new(&path).is_file() => files.push(PathBuf::from(path)),
            Some(path) => missing.push(path),
            None => missing.push(format!("{}（无文件路径）", session.id)),
        }
    }
    MergePlan {
        files,
        danmaku,
        total_ms,
        room_id,
        missing,
    }
}

/// 缺段时给用户一句能定位的提示，并阻止“少合一段也算成功”。
fn missing_files_error(missing: &[String]) -> AppError {
    let shown: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
    let more = missing.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" 等 {} 段", missing.len())
    } else {
        String::new()
    };
    AppError::User(format!(
        "有 {} 段录像文件不存在，已停止合并：{}{suffix}",
        missing.len(),
        shown.join("、")
    ))
}

/// 逐个 ffprobe 合并源；数组顺序与 `merge_plan.files` 一致。
async fn probe_merge_durations(files: &[PathBuf]) -> Vec<Option<i64>> {
    let mut durations = Vec::with_capacity(files.len());
    for file in files {
        let duration = crate::post::media_duration_ms(file)
            .await
            .map(|ms| ms as i64)
            .filter(|ms| *ms > 0);
        durations.push(duration);
    }
    durations
}

/// 只有所有源都探测成功才使用容器时长，避免一段失败后偏移半新半旧。
fn actual_total_ms(durations: &[Option<i64>]) -> Option<i64> {
    if durations.is_empty() || durations.iter().any(Option::is_none) {
        return None;
    }
    Some(durations.iter().map(|value| value.unwrap_or(0)).sum())
}

fn retime_danmaku_sources(
    mut sources: Vec<postprocess::DanmakuSource>,
    durations: &[Option<i64>],
) -> Vec<postprocess::DanmakuSource> {
    if sources.len() != durations.len() || durations.iter().any(Option::is_none) {
        return sources;
    }
    for (source, duration) in sources.iter_mut().zip(durations) {
        source.duration_ms = duration.unwrap_or(source.duration_ms);
    }
    sources
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
            quality_history: Vec::new(),
            line: None,
            error: None,
            danmaku_path: None,
            danmaku_count: 0,
            source_path: None,
            post_mode: "source".into(),
            post_state: "done".into(),
            source_deleted: false,
            post_error: None,
            group_id: None,
            clip_index: 0,
        }
    }

    #[test]
    fn canonical_reference_promotes_bilibili_short_id() {
        let reference = RoomRef {
            platform: "bilibili".into(),
            room_key: "6".into(),
            canonical_url: "https://live.bilibili.com/6".into(),
            input: "6".into(),
        };
        let info = RoomInfo {
            host_name: "主播".into(),
            room_name: "标题".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: None,
            online: None,
            stable_id: Some("7734200".into()),
            extra: serde_json::Value::Null,
        };
        let canonical = canonical_reference(&reference, &info);
        assert_eq!(canonical.room_key, "7734200");
        assert_eq!(canonical.canonical_url, "https://live.bilibili.com/7734200");

        // 没有稳定 ID 的平台保持原键。
        let mut no_stable = info;
        no_stable.stable_id = None;
        let unchanged = canonical_reference(&reference, &no_stable);
        assert_eq!(unchanged.room_key, "6");
    }

    #[test]
    fn canonical_reference_promotes_huya_alias_to_numeric_room() {
        let reference = RoomRef {
            platform: "huya".into(),
            room_key: "lpl".into(),
            canonical_url: "https://www.huya.com/lpl".into(),
            input: "https://www.huya.com/lpl".into(),
        };
        let info = RoomInfo {
            host_name: "虎牙英雄联盟赛事".into(),
            room_name: "S16".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: None,
            online: None,
            stable_id: Some("660000".into()),
            extra: serde_json::Value::Null,
        };
        let canonical = canonical_reference(&reference, &info);
        assert_eq!(canonical.room_key, "660000");
        assert_eq!(canonical.canonical_url, "https://www.huya.com/660000");
    }

    #[test]
    fn canonical_reference_promotes_douyu_alias_to_numeric_room() {
        let reference = RoomRef {
            platform: "douyu".into(),
            room_key: "yyfyyf".into(),
            canonical_url: "https://www.douyu.com/yyfyyf".into(),
            input: "https://www.douyu.com/yyfyyf".into(),
        };
        let info = RoomInfo {
            host_name: "主播".into(),
            room_name: "标题".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: None,
            online: None,
            stable_id: Some("9999".into()),
            extra: serde_json::Value::Null,
        };
        let canonical = canonical_reference(&reference, &info);
        assert_eq!(canonical.room_key, "9999");
        assert_eq!(
            canonical.canonical_url, "https://www.douyu.com/9999",
            "别名房间归一后，导入数字 room_id 才能命中同一行"
        );
    }

    #[test]
    fn merge_order_follows_start_time_and_reports_missing() {
        let dir = std::env::temp_dir().join(format!("hivecast-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let missing = dir.join("gone.mp4");
        // 乱序传入，缺失文件在中间。
        let plan = merge_plan(vec![
            session("2", 200, b.clone()),
            session("3", 300, missing.clone()),
            session("1", 100, a.clone()),
        ]);
        assert_eq!(plan.files, vec![a, b]);
        assert_eq!(plan.missing, vec![missing.display().to_string()]);
        // 缺段不能静默成功；调用方会用它中止合并。
        assert!(matches!(
            missing_files_error(&plan.missing),
            AppError::User(message) if message.contains("3 段") || message.contains("1 段")
        ));
        assert_eq!(plan.total_ms, 180_000);
        assert_eq!(plan.danmaku.len(), 3);
        assert_eq!(plan.room_id.as_deref(), Some("room"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_order_uses_clip_index_within_the_same_second() {
        let dir = std::env::temp_dir().join(format!("hivecast-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let mut second = session("2", 100, b.clone());
        second.clip_index = 1;
        let plan = merge_plan(vec![second, session("1", 100, a.clone())]);
        assert_eq!(plan.files, vec![a, b]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn deleting_a_recording_clears_its_generated_sidecars() {
        let dir = std::env::temp_dir().join(format!("hivecast-sidecars-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("clip.mp4");
        std::fs::write(&main, b"main").unwrap();
        let sidecars = [
            dir.join("clip.ass"),
            dir.join("clip.jsonl"),
            dir.join("clip.split.json"),
            dir.join("clip.ffmpeg.pid"),
            dir.join("clip.part0000.ts"),
            dir.join("clip.part0001.ts"),
            dir.join(".clip.merging.mp4"),
        ];
        for sidecar in &sidecars {
            std::fs::write(sidecar, b"x").unwrap();
        }
        let unrelated = dir.join("other.mp4");
        std::fs::write(&unrelated, b"keep").unwrap();

        remove_session_sidecars(&main).await;

        assert!(main.is_file(), "主录像不能被伴随文件清理顺带删掉");
        assert!(unrelated.is_file(), "同目录的其他录像不能被误删");
        for sidecar in &sidecars {
            assert!(!sidecar.exists(), "{} 应被清理", sidecar.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn danmaku_replay_streams_only_the_requested_window() {
        let dir =
            std::env::temp_dir().join(format!("hivecast-danmaku-window-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("track.jsonl");
        let lines = [
            r#"{"t":0,"k":"chat","c":"a"}"#,
            "not json",
            r#"{"t":1500,"k":"gift","c":"b","u":"u"}"#,
            r#"{"t":3000,"k":"chat","c":"c"}"#,
        ];
        std::fs::write(
            &path,
            lines.join(
                "
",
            ) + "
",
        )
        .unwrap();
        let window = read_danmaku_range(&path, Some(1000), Some(2500)).unwrap();
        assert_eq!(window.len(), 1, "只返回窗口内的弹幕");
        assert_eq!(window[0].content, "b");
        assert_eq!(window[0].offset_ms, 1500);
        assert_eq!(window[0].kind, "gift");
        let all = read_danmaku_range(&path, None, None).unwrap();
        assert_eq!(all.len(), 3, "坏行跳过，其余全部返回");
        assert_eq!(
            all.iter().map(|m| m.offset_ms).collect::<Vec<_>>(),
            vec![0, 1500, 3000]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retimes_danmaku_offsets_to_actual_container_durations() {
        let source = |duration_ms| postprocess::DanmakuSource {
            jsonl: None,
            duration_ms,
        };
        let sources = vec![source(60_000), source(60_000)];
        let durations = vec![Some(60_384), Some(62_100)];
        let retimed = retime_danmaku_sources(sources, &durations);
        assert_eq!(retimed[0].duration_ms, 60_384);
        assert_eq!(retimed[1].duration_ms, 62_100);
        assert_eq!(actual_total_ms(&durations), Some(122_484));
    }

    #[test]
    fn merge_duration_falls_back_when_one_probe_fails() {
        let source = |duration_ms| postprocess::DanmakuSource {
            jsonl: None,
            duration_ms,
        };
        let sources = vec![source(60_000), source(60_000)];
        let durations = vec![Some(61_000), None];
        let retimed = retime_danmaku_sources(sources, &durations);
        assert_eq!(retimed[0].duration_ms, 60_000);
        assert_eq!(retimed[1].duration_ms, 60_000);
        assert_eq!(actual_total_ms(&durations), None);
    }

    #[test]
    fn import_room_info_keeps_started_at() {
        let item = ImportRoomInput {
            platform: None,
            uid: "42".into(),
            room_id: " 7734200 ".into(),
            short_id: Some("6".into()),
            uname: "主播".into(),
            title: "标题".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: Some(1_792_050_615),
            online: Some(10),
            video_loop: None,
            internal_room_id: None,
        };
        let info = import_room_info(&item, "bilibili", "42", "主播");
        assert_eq!(info.started_at, Some(1_792_050_615));
        assert_eq!(info.stable_id.as_deref(), Some("7734200"));
        assert_eq!(info.extra["liveStatus"], 1);
        assert_eq!(info.extra["roomId"], "7734200");
    }

    #[test]
    fn douyu_import_keeps_video_loop_without_a_uid() {
        let item = ImportRoomInput {
            platform: Some("douyu".into()),
            uid: String::new(),
            room_id: "9999".into(),
            short_id: None,
            uname: "主播".into(),
            title: "轮播".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: false,
            started_at: None,
            online: Some(0),
            video_loop: Some(true),
            internal_room_id: None,
        };
        let info = import_room_info(&item, "douyu", "", "主播");
        assert_eq!(info.extra["roomId"], "9999");
        assert_eq!(info.extra["liveStatus"], 0);
        assert_eq!(
            info.extra["videoLoop"], true,
            "轮播标记必须写进 extra，刷新后不能再当直播"
        );
        assert!(info.extra.get("uid").is_none(), "斗鱼没有账号 UID");
    }

    #[test]
    fn merged_session_registers_danmaku_and_media_track() {
        let dir = std::env::temp_dir().join(format!("hivecast-merge-reg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("hivecast.db")).unwrap();
        db.exec_batch_for_test(
            "INSERT INTO room (id, platform, room_key, canonical_url, title, host_name, created_at, updated_at) \
             VALUES ('room-merge', 'bilibili', '6', 'https://live.bilibili.com/6', '标题', '主播', 1, 1)",
        )
        .unwrap();
        let video = dir.join("merged.mp4");
        std::fs::write(&video, b"merged-video").unwrap();
        let jsonl = dir.join("merged.jsonl");
        std::fs::write(
            &jsonl,
            "{\"t\":1000,\"k\":\"chat\",\"u\":\"甲\",\"c\":\"一\",\"col\":16777215}\n\n\
             {\"t\":2500,\"k\":\"chat\",\"u\":\"乙\",\"c\":\"二\",\"col\":16777215}\n\
             {\"t\":3000,\"k\":\"chat\",\"u\":\"丙\",\"c\":\"三\",\"col\":16777215}\n",
        )
        .unwrap();
        let merged = PostResult {
            kind: "merge".into(),
            path: video.display().to_string(),
            size: 12,
            message: Some("合并 2 段".into()),
        };

        register_merged_session(&db, "room-merge", &merged, Some(&jsonl), 125_000).unwrap();

        let (items, total) = db
            .media_list(&MediaFilter {
                room_id: Some("room-merge".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        let item = &items[0];
        assert_eq!(item.danmaku_count, 3);
        assert_eq!(
            item.danmaku_path.as_deref(),
            Some(jsonl.to_str().unwrap()),
            "媒体库条目必须能拿到弹幕轨路径"
        );
        let session = db.get_session(&item.id).unwrap().unwrap();
        assert_eq!(session.reason.as_deref(), Some("merge"));
        assert_eq!(session.status, "finished");
        assert_eq!(session.post_mode, "source");
        assert_eq!(session.post_state, "done");
        assert_eq!(session.duration_ms, 125_000);
        assert_eq!(session.danmaku_count, 3);

        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn csv_export_uses_readable_labels_and_blocks_formulas() {
        assert_eq!(csv_stage(None, "merging"), "合并");
        assert_eq!(csv_stage(None, "postprocessing"), "后处理");
        assert_eq!(csv_state(None, "running"), "运行中");
        assert_eq!(csv_state(None, "queued"), "排队中");
        assert_eq!(csv_stage(None, "unknown"), "unknown");
        assert_eq!(csv_stage(Some("en-US"), "merging"), "Merging");
        assert_eq!(
            csv_stage(Some("en-US"), "postprocessing"),
            "Post-processing"
        );
        assert_eq!(csv_stage(Some("en-US"), "thumbnail"), "Cover");
        assert_eq!(csv_state(Some("en-US"), "running"), "Running");
        assert_eq!(csv_state(Some("en-US"), "partial"), "Partially completed");
        assert!(csv_header(None).starts_with("时间,平台"));
        assert!(csv_header(Some("en-US")).starts_with("Time,Platform"));

        assert_eq!(csv_cell("=1+1"), "\"'=1+1\"");
        assert_eq!(csv_cell("  @cmd"), "\"'  @cmd\"");
        assert_eq!(csv_cell("-2+3"), "\"'-2+3\"");
        assert_eq!(csv_cell("正常标题"), "\"正常标题\"");
        assert_eq!(csv_cell("a\"b"), "\"a\"\"b\"");
    }
}

// ---- 设置 ----

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> Result<Settings, AppError> {
    Ok(crate::settings::load(&state.db))
}

/// 保存设置；返回规范化后的值（目录会被创建）。
/// 文件名模板预览：用一份固定的示例房间渲染，保证与真正落盘时同一套逻辑。
#[tauri::command]
pub fn filename_preview(
    state: State<'_, AppState>,
    template: String,
    strip_emoji: bool,
    group_dirs: bool,
    ext: Option<String>,
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
    // 扩展名跟随当前后处理设置：保留源封装时用 flv 作代表，转封装/自定义用各自目标格式。
    let ext = ext
        .as_deref()
        .map(str::trim)
        .map(|value| value.trim_start_matches('.'))
        .filter(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("flv");
    let path =
        crate::recorder::session_path(&state.recorder.media_root(), &parts, now_ts(), ext, &naming);
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
    let saved = match crate::settings::save(&state.db, &settings) {
        Ok(saved) => saved,
        Err(err) => {
            tracing::warn!(target: "hivecast_lib::settings", error = %err, "设置保存被拒绝");
            return Err(err);
        }
    };
    crate::settings::apply(&saved, &state.recorder);
    if let Err(reason) = crate::logging::apply_settings(&saved.log_dir, saved.log_retention_days) {
        tracing::warn!(error = %reason, "日志目录切换失败，已回落到默认目录");
        let pair = crate::logging::take_fallback();
        let body = pair
            .map(|(from, to)| format!("{from} 不可写，日志已改写到 {to}；设置里仍保留原路径"))
            .unwrap_or(reason);
        if let Err(err) = app
            .notification()
            .builder()
            .title("HIVECAST 日志目录已切换")
            .body(body)
            .show()
        {
            tracing::warn!(%err, "系统通知发送失败");
        }
    }
    crate::logging::cleanup(saved.log_retention_days);
    tracing::info!(
        target: "hivecast_lib::settings",
        log_dir = %saved.log_dir,
        log_retention_days = saved.log_retention_days,
        "设置已保存"
    );
    // 托盘菜单文案跟随界面语言。
    crate::tray::refresh(&app);
    Ok(saved)
}

// ---- 日志 ----

#[tauri::command]
pub fn logs_status(state: State<'_, AppState>) -> Result<LogStatus, AppError> {
    let settings = crate::settings::load(&state.db);
    let mut status = crate::logging::status();
    status.configured = settings.log_dir;
    status.retention_days = settings.log_retention_days;
    Ok(status)
}

/// 只打开「当前实际生效」的日志目录，不接受前端传路径，避免变成任意目录打开器。
#[tauri::command]
pub fn logs_open() -> Result<(), AppError> {
    let dir = std::path::PathBuf::from(crate::logging::status().dir);
    std::fs::create_dir_all(&dir).map_err(|err| {
        tracing::warn!(target: "hivecast_lib::commands", dir = %dir.display(), error = %err, "创建日志目录失败");
        AppError::from_io(err, &dir)
    })?;
    tracing::info!(target: "hivecast_lib::commands", dir = %dir.display(), "打开日志目录");
    open_directory(&dir)
}

/// 前端未捕获错误兜底。截断到 4 KiB，避免把整块 React 树塞进日志。
#[tauri::command]
pub fn ui_log(level: String, message: String, context: Option<String>) -> Result<(), AppError> {
    let message: String = message.chars().take(4096).collect();
    let context: String = context.unwrap_or_default().chars().take(4096).collect();
    match level.as_str() {
        "error" => {
            tracing::error!(target: "hivecast_lib::ui", context = %context, message = %message, "界面错误")
        }
        "warn" => {
            tracing::warn!(target: "hivecast_lib::ui", context = %context, message = %message, "界面警告")
        }
        "debug" => {
            tracing::debug!(target: "hivecast_lib::ui", context = %context, message = %message, "界面调试")
        }
        _ => {
            tracing::info!(target: "hivecast_lib::ui", context = %context, message = %message, "界面日志")
        }
    }
    Ok(())
}

/// 打开直播间网页：只允许 http/https，避免把任意协议交给系统。
#[tauri::command]
pub fn open_url(url: String) -> Result<(), AppError> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::User("只支持 http/https 链接".into()));
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开链接：{err}")))?;
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(url)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开链接：{err}")))?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    std::process::Command::new("xdg-open")
        .arg(url)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开链接：{err}")))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn available_space(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(unix)]
fn available_space(path: &std::path::Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::statvfs(path.as_ptr(), &mut stat) };
    (ok == 0).then_some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(target_os = "macos")]
fn open_directory(path: &std::path::Path) -> Result<(), AppError> {
    std::process::Command::new("open")
        .arg(path)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开日志目录：{err}")))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_directory(path: &std::path::Path) -> Result<(), AppError> {
    std::process::Command::new("explorer")
        .arg(path)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开日志目录：{err}")))?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_directory(path: &std::path::Path) -> Result<(), AppError> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .status()
        .map_err(|err| AppError::Internal(format!("无法打开日志目录：{err}")))?;
    Ok(())
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
        tracing::warn!(target: "hivecast_lib::commands", bytes = layout.len(), "观看布局被拒绝：数据过大");
        return Err(AppError::User("布局数据过大".into()));
    }
    tracing::debug!(target: "hivecast_lib::commands", bytes = layout.len(), "保存观看布局");
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
    app: AppHandle,
    state: State<'_, AppState>,
    platform: String,
    cookie: String,
) -> Result<CredentialInfo, AppError> {
    tracing::debug!(target: "hivecast_lib::commands", platform = %platform, "保存平台登录凭据");
    let info = crate::account::save_cookie(&state.db, &platform, &cookie).await?;
    state.broker.clear_groups();
    state
        .recorder
        .recheck_all_quality(&app, state.broker.clone(), &platform, "credential_saved");
    Ok(info)
}

#[tauri::command]
pub async fn credential_delete(
    state: State<'_, AppState>,
    platform: String,
) -> Result<(), AppError> {
    tracing::info!(target: "hivecast_lib::commands", platform = %platform, "删除平台登录凭据");
    crate::account::delete(&state.db, &platform)?;
    state.broker.clear_groups();
    Ok(())
}

#[tauri::command]
pub async fn bilibili_qr_start() -> Result<QrTicket, AppError> {
    crate::account::bilibili_qr_start().await
}

#[tauri::command]
pub async fn bilibili_qr_poll(
    app: AppHandle,
    state: State<'_, AppState>,
    qrcode_key: String,
) -> Result<QrPoll, AppError> {
    let poll = crate::account::bilibili_qr_poll(&state.db, &qrcode_key).await?;
    if poll.state == "confirmed" {
        state.broker.clear_groups();
        state
            .recorder
            .recheck_all_quality(&app, state.broker.clone(), "bilibili", "qr_confirmed");
    }
    Ok(poll)
}

/// 设置页「立即检查」：跑一次完整续期链路并把结果落库。
#[tauri::command]
pub async fn bilibili_refresh_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RefreshStatus, AppError> {
    crate::account::refresh_now(&state.db, &app).await
}

#[tauri::command]
pub async fn douyu_qr_start() -> Result<QrTicket, AppError> {
    crate::douyu_auth::qr_start().await
}

#[tauri::command]
pub async fn douyu_qr_poll(
    app: AppHandle,
    state: State<'_, AppState>,
    qrcode_key: String,
) -> Result<QrPoll, AppError> {
    let poll = crate::douyu_auth::qr_poll(&state.db, &app, &qrcode_key).await?;
    if poll.state == "confirmed" {
        state.broker.clear_groups();
        state.recorder.recheck_all_quality(
            &app,
            state.broker.clone(),
            "douyu",
            "douyu_qr_confirmed",
        );
    }
    Ok(poll)
}

#[tauri::command]
pub async fn douyu_refresh_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RefreshStatus, AppError> {
    crate::douyu_auth::refresh_now(&state.db, &app).await
}

/// 手动检查抖音登录态：先轮换 Cookie，再复验身份。
#[tauri::command]
pub async fn douyin_refresh_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RefreshStatus, AppError> {
    crate::douyin_auth::refresh_now(&state.db, &app).await
}

#[tauri::command]
pub async fn huya_qr_start() -> Result<QrTicket, AppError> {
    crate::huya_auth::qr_start().await
}

#[tauri::command]
pub fn huya_qr_cancel(qrcode_key: String) {
    crate::huya_auth::qr_cancel(&qrcode_key);
}

#[tauri::command]
pub async fn huya_qr_poll(
    app: AppHandle,
    state: State<'_, AppState>,
    qrcode_key: String,
) -> Result<QrPoll, AppError> {
    let poll = crate::huya_auth::qr_poll(&state.db, &app, &qrcode_key).await?;
    // 与 B 站/斗鱼扫码确认保持一致：确认后清一次取流缓存并重检画质。
    // 只重检虎牙，避免扫虎牙码影响其他平台正在录制的进程。
    if poll.state == "confirmed" {
        state.broker.clear_groups();
        state
            .recorder
            .recheck_all_quality(&app, state.broker.clone(), "huya", "huya_qr_confirmed");
    }
    Ok(poll)
}

#[tauri::command]
pub async fn huya_refresh_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RefreshStatus, AppError> {
    crate::huya_auth::refresh_now(&state.db, &app).await
}

// ---- 媒体库 ----

/// 媒体库列表：筛选/排序/分页在 SQL 里做，返回本页 + 总数。
///
/// 顺手把缺缩略图的录像排进后台队列；生成完通过 EVENT_THUMB 回推，
/// 前端按 sessionId 换封面，不用为了等封面把列表卡住。
// 筛选参数与前端查询一一对应，拆结构体会改变 IPC 参数形状。
#[allow(clippy::too_many_arguments)]
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
    tracing::debug!(target: "hivecast_lib::commands", total, page = items.len(), thumbs_queued = pending.len(), "媒体库列表");
    state.thumbs.ensure(app, pending);
    let platform_counts = state.db.media_platform_counts(&filter)?;
    Ok(MediaPage {
        items,
        total,
        platform_counts,
    })
}

/// 取一整场直播的全部片段：媒体库分页可能只拿到其中几段，前端点“+N”时补齐。
#[tauri::command]
pub async fn media_group_clips(
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
) -> Result<Vec<MediaItem>, AppError> {
    let mut items = state.db.media_group_items(&group_id)?;
    let mut pending = Vec::new();
    for item in &mut items {
        item.thumbnail_url = state.thumbs.url_for(&item.id);
        if item.thumbnail_url.is_none() && item.has_file && item.status != "recording" {
            pending.push((item.id.clone(), item.path.clone(), item.duration_ms));
        }
    }
    tracing::debug!(target: "hivecast_lib::commands", group_id = %group_id, clips = items.len(), "补齐整场片段");
    state.thumbs.ensure(app, pending);
    Ok(items)
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
    tracing::info!(target: "hivecast_lib::commands", session_id = %session_id, file = %path, "导出弹幕为 ASS");
    postprocess::danmaku_to_ass(std::path::Path::new(&path))
}

/// 从 JSONL 里流式读出 [from_ms, to_ms] 的弹幕，避免整文件读进内存。
///
/// 录像 JSONL 是追加写、天然按时间有序；只对取出的窗口排序，窗口大小由调用方控制。
fn read_danmaku_range(
    path: &std::path::Path,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> Result<Vec<DanmakuMessage>, AppError> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)
        .map_err(|err| AppError::Internal(format!("读取弹幕文件失败：{err}")))?;
    let mut messages = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let offset = value.get("t").and_then(|v| v.as_i64()).unwrap_or(0);
        if from_ms.is_some_and(|from| offset < from) || to_ms.is_some_and(|to| offset > to) {
            continue;
        }
        let content = value.get("c").and_then(|v| v.as_str()).unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        messages.push(DanmakuMessage {
            offset_ms: offset,
            kind: value
                .get("k")
                .and_then(|v| v.as_str())
                .unwrap_or("chat")
                .to_string(),
            user: value.get("u").and_then(|v| v.as_str()).map(str::to_string),
            content: content.to_string(),
            color: value
                .get("col")
                .and_then(|v| v.as_u64())
                .unwrap_or(0xFF_FF_FF) as u32,
        });
    }
    messages.sort_by_key(|message| message.offset_ms);
    Ok(messages)
}

/// 读取一段录像的弹幕 JSONL，供媒体库按视频进度回放。
///
/// 只返回播放位置附近的窗口（前端按时间推进换窗），避免长录像把整轨复制到 webview。
#[tauri::command]
pub async fn media_danmaku(
    state: State<'_, AppState>,
    session_id: String,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> Result<Vec<DanmakuMessage>, AppError> {
    let session = session_file(&state, &session_id)?;
    let path = session
        .danmaku_path
        .filter(|path| std::path::Path::new(path).is_file())
        .ok_or_else(|| AppError::User("这段录像没有弹幕文件".into()))?;
    let messages = read_danmaku_range(std::path::Path::new(&path), from_ms, to_ms)?;
    tracing::debug!(target: "hivecast_lib::commands", session_id = %session_id, count = messages.len(), from_ms, to_ms, "读取录像弹幕窗口");
    Ok(messages)
}

/// 删掉录像主文件之外由录制流程生成的伴随文件：分片、分割清单、pid、
/// 弹幕字幕和合并临时文件。主文件删除失败时不要调用，避免留下半套数据。
async fn remove_session_sidecars(path: &std::path::Path) -> Vec<PathBuf> {
    let mut targets = vec![
        path.with_extension("ass"),
        path.with_extension("jsonl"),
        path.with_extension("split.json"),
        path.with_extension("ffmpeg.pid"),
    ];
    targets.extend(crate::segment::collect_parts(path));
    if let (Some(parent), Some(stem)) = (
        path.parent(),
        path.file_stem().and_then(|stem| stem.to_str()),
    ) {
        let prefix = format!(".{stem}.merging.");
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    targets.push(entry.path());
                }
            }
        }
    }
    let mut failed = Vec::new();
    for target in targets {
        match tokio::fs::remove_file(&target).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    target: "hivecast_lib::commands",
                    file = %target.display(),
                    error = %err,
                    "删除录像伴随文件失败"
                );
                failed.push(target);
            }
        }
    }
    failed
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
    // 分割录像是一个整体：单删首段会把其余片段的 group_id 指向不存在的行。
    if state.db.session_group_size(&session_id)? > 1 {
        return Err(AppError::User("这段录像包含多个片段，请删除整场".into()));
    }

    state.local.stop(&session_id).await;

    if delete_file {
        if let Some(path) = session.file_path.as_deref() {
            let path = std::path::Path::new(path);
            if path.is_file() {
                tokio::fs::remove_file(path).await.map_err(|err| {
                    tracing::warn!(target: "hivecast_lib::commands", session_id = %session_id, file = %path.display(), error = %err, "删除录像文件失败");
                    AppError::from_io(err, path)
                })?;
            }
        }
        let mut failed: Vec<String> = Vec::new();
        if let Some(path) = session.danmaku_path.as_deref() {
            let path = std::path::Path::new(path);
            if path.is_file() {
                if let Err(err) = tokio::fs::remove_file(path).await {
                    tracing::warn!(target: "hivecast_lib::commands", session_id = %session_id, file = %path.display(), error = %err, "删除弹幕文件失败");
                    failed.push(path.display().to_string());
                }
            }
        }
        if let Some(path) = session.file_path.as_deref() {
            failed.extend(
                remove_session_sidecars(std::path::Path::new(path))
                    .await
                    .into_iter()
                    .map(|path| path.display().to_string()),
            );
        }
        // 源封装与历史派生文件也要一起删，否则转封装几次就会留下孤儿。
        // 任何文件删失败都保留数据库行：主文件可能已经删掉，但用户还能从
        // 残留列表重试，不会出现“界面说删完了，母带还在盘上”。
        for file in state.db.media_files(&session_id)? {
            if file.state != "present" || Some(file.path.as_str()) == session.file_path.as_deref() {
                continue;
            }
            let path = std::path::Path::new(&file.path);
            if path.is_file() {
                if let Err(err) = tokio::fs::remove_file(path).await {
                    tracing::warn!(target: "hivecast_lib::commands", session_id = %session_id, file = %path.display(), error = %err, "删除派生文件失败");
                    failed.push(path.display().to_string());
                    continue;
                }
            }
            failed.extend(
                remove_session_sidecars(path)
                    .await
                    .into_iter()
                    .map(|path| path.display().to_string()),
            );
        }
        if !failed.is_empty() {
            failed.sort();
            failed.dedup();
            return Err(AppError::User(format!(
                "录像记录已保留，以下文件删除失败，请重试：{}",
                failed.join("、")
            )));
        }
    }

    tracing::info!(target: "hivecast_lib::commands", session_id = %session_id, delete_file, "删除录像");
    state.thumbs.remove(&session_id);
    state.db.delete_session(&session_id)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    Ok(())
}

/// 删除一整场直播（同一 group 下的所有片段）。
#[tauri::command]
pub async fn media_delete_group(
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
    delete_file: bool,
) -> Result<u32, AppError> {
    let sessions = state.db.group_sessions(&group_id)?;
    if sessions.is_empty() {
        return Err(AppError::User("录像不存在".into()));
    }
    if sessions.iter().any(|session| session.status == "recording") {
        return Err(AppError::User("这场直播还在录制，先停下再删除".into()));
    }

    let mut removed = 0u32;
    // 一个片段删失败不能把后面的片段卡住：先尽力删完，最后再把第一个错误
    // 返回给界面。视频文件删失败的片段保留数据库行，用户重试时还能再删一次；
    // 文件已经不在的片段则照常清理数据库，避免留下永远点不掉的幽灵录像。
    let mut first_error: Option<AppError> = None;
    for session in &sessions {
        state.local.stop(&session.id).await;
        let mut file_failed = false;
        let mut failed_files: Vec<String> = Vec::new();
        if delete_file {
            if let Some(path) = session.file_path.as_deref() {
                let path = std::path::Path::new(path);
                if path.is_file() {
                    if let Err(err) = tokio::fs::remove_file(path).await {
                        tracing::warn!(target: "hivecast_lib::commands", session_id = %session.id, file = %path.display(), error = %err, "删除录像文件失败");
                        if first_error.is_none() {
                            first_error = Some(AppError::from_io(err, path));
                        }
                        failed_files.push(path.display().to_string());
                        file_failed = true;
                    }
                }
            }
            if let Some(path) = session.danmaku_path.as_deref() {
                let path = std::path::Path::new(path);
                if path.is_file() {
                    if let Err(err) = tokio::fs::remove_file(path).await {
                        tracing::warn!(target: "hivecast_lib::commands", session_id = %session.id, file = %path.display(), error = %err, "删除弹幕文件失败");
                        failed_files.push(path.display().to_string());
                        file_failed = true;
                    }
                }
            }
        }
        if file_failed {
            if first_error.is_none() && !failed_files.is_empty() {
                first_error = Some(AppError::User(format!(
                    "录像记录已保留，以下文件删除失败，请重试：{}",
                    failed_files.join("、")
                )));
            }
            continue;
        }
        // 与单条删除保持一致：用户选择「保留文件」时不要顺手删掉弹幕、
        // 分割清单和分片，否则留下的 mp4 会缺少可追溯的伴随数据。
        if delete_file {
            if let Some(path) = session.file_path.as_deref() {
                let leftovers = remove_session_sidecars(std::path::Path::new(path)).await;
                if !leftovers.is_empty() {
                    file_failed = true;
                    failed_files.extend(leftovers.iter().map(|path| path.display().to_string()));
                }
            }
            for file in state.db.media_files(&session.id)? {
                if file.state != "present"
                    || Some(file.path.as_str()) == session.file_path.as_deref()
                {
                    continue;
                }
                let path = std::path::Path::new(&file.path);
                if path.is_file() {
                    if let Err(err) = tokio::fs::remove_file(path).await {
                        tracing::warn!(target: "hivecast_lib::commands", session_id = %session.id, file = %path.display(), error = %err, "删除派生文件失败");
                        failed_files.push(path.display().to_string());
                        file_failed = true;
                        continue;
                    }
                }
                let leftovers = remove_session_sidecars(path).await;
                if !leftovers.is_empty() {
                    file_failed = true;
                    failed_files.extend(leftovers.iter().map(|path| path.display().to_string()));
                }
            }
        }
        if file_failed {
            if first_error.is_none() && !failed_files.is_empty() {
                first_error = Some(AppError::User(format!(
                    "录像记录已保留，以下文件删除失败，请重试：{}",
                    failed_files.join("、")
                )));
            }
            continue;
        }
        state.thumbs.remove(&session.id);
        if let Err(err) = state.db.delete_session(&session.id) {
            tracing::warn!(target: "hivecast_lib::commands", session_id = %session.id, error = %err, "删除录像记录失败");
            if first_error.is_none() {
                first_error = Some(err);
            }
            continue;
        }
        removed += 1;
    }

    tracing::info!(target: "hivecast_lib::commands", group_id = %group_id, clips = removed, total = sessions.len(), delete_file, "删除整场录像");
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    match first_error {
        Some(err) => Err(err),
        None => Ok(removed),
    }
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
    tracing::debug!(target: "hivecast_lib::commands", session_id = %session_id, file = %path, "在文件管理器中定位录像");
    reveal_in_file_manager(std::path::Path::new(&path))
}

/// 在文件管理器里定位源封装母带（转封装后主文件已经是派生文件）。
#[tauri::command]
pub async fn media_reveal_source(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), AppError> {
    let session = state
        .db
        .get_session(&session_id)?
        .ok_or_else(|| AppError::User("录像不存在".into()))?;
    let path = session
        .source_path
        .filter(|p| std::path::Path::new(p).is_file())
        .ok_or_else(|| AppError::User("源封装文件已不存在".into()))?;
    tracing::debug!(target: "hivecast_lib::commands", session_id = %session_id, file = %path, "在文件管理器中定位源封装");
    reveal_in_file_manager(std::path::Path::new(&path))
}

/// 在文件管理器里定位这条录像登记过的任意文件（派生文件也适用）。
#[tauri::command]
pub async fn media_reveal_file(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<(), AppError> {
    let registered = state.db.media_files(&session_id)?;
    let target = registered
        .iter()
        .find(|file| file.path == path && file.state != "deleted")
        .ok_or_else(|| AppError::User("文件未登记在这条录像下".into()))?;
    if !std::path::Path::new(&target.path).is_file() {
        return Err(AppError::User("文件已不存在".into()));
    }
    tracing::debug!(target: "hivecast_lib::commands", session_id = %session_id, file = %target.path, "在文件管理器中定位媒体文件");
    reveal_in_file_manager(std::path::Path::new(&target.path))
}

#[tauri::command]
pub async fn media_play(
    state: State<'_, AppState>,
    session_id: String,
    position_ms: Option<i64>,
    source: Option<bool>,
) -> Result<Playback, AppError> {
    let source = source.unwrap_or(false);
    tracing::info!(target: "hivecast_lib::commands", session_id = %session_id, position_ms = ?position_ms, source, "本地回放");
    state
        .local
        .open(&state.db, &session_id, position_ms, source)
        .await
}

/// 停止一条回放；只影响这一条，其余各格继续放。
#[tauri::command]
pub async fn media_stop(state: State<'_, AppState>, session_id: String) -> Result<(), AppError> {
    tracing::debug!(target: "hivecast_lib::commands", session_id = %session_id, "停止本地回放");
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

// ---- 任务中心 ----

/// 任务中心全链路快照：把录制、收尾、后处理、历史场次聚合成 Run。
#[tauri::command]
pub fn task_center(
    state: State<'_, AppState>,
    history: Option<usize>,
) -> Result<TaskCenter, AppError> {
    crate::task_center::build(
        &state.db,
        &state.recorder,
        &state.jobs,
        &state.monitor_status,
        state.info.ffmpeg_path.as_deref(),
        &state.media_root,
        history.unwrap_or(100),
    )
}

/// 任务中心详情的时间线：只读当前 Run，避免首屏把上千场事件全部拉进内存。
/// session:group 会展开为同组所有片段的 session 事件；按 (at, id) 游标向更早翻页。
#[tauri::command]
pub fn task_run_events(
    state: State<'_, AppState>,
    run_id: String,
    before_at: Option<i64>,
    before_id: Option<i64>,
    limit: Option<usize>,
) -> Result<hivecast_ipc::TaskEventPage, AppError> {
    let mut event_run_ids = vec![run_id.clone()];
    if let Some(group_id) = run_id.strip_prefix("session:") {
        for member in state.db.session_group_ids(group_id)? {
            let alias = format!("session:{member}");
            if !event_run_ids.contains(&alias) {
                event_run_ids.push(alias);
            }
        }
    }
    let before = before_at.zip(before_id);
    state
        .db
        .task_event_page(&event_run_ids, before, limit.unwrap_or(200))
}

/// 跨 Run 检索阶段事件，供任务页搜索；结果带房间信息，不受历史窗口限制。
#[tauri::command]
pub fn task_event_search(
    state: State<'_, AppState>,
    query: Option<String>,
    only_failed: Option<bool>,
    platform: Option<String>,
    limit: Option<usize>,
) -> Result<TaskEventSearch, AppError> {
    state.db.task_event_search(
        query.as_deref().unwrap_or_default(),
        only_failed.unwrap_or(false),
        platform.as_deref(),
        limit.unwrap_or(200),
    )
}

/// 导出的是给人看的 CSV，阶段/状态不能再写内部枚举。
/// 与跨 Run 检索共用 labels：两处各写一套映射迟早会漂移。
fn csv_stage(lang: Option<&str>, stage: &str) -> String {
    crate::labels::stage_label_for(lang, stage)
}

fn csv_state(lang: Option<&str>, state: &str) -> String {
    crate::labels::state_label_for(lang, state)
}

fn csv_header(lang: Option<&str>) -> &'static str {
    if crate::labels::is_english(lang) {
        "Time,Platform,Room,Streamer,Title,Stage,State,Progress,Message,Task,Run"
    } else {
        "时间,平台,房间号,主播,直播间标题,阶段,状态,进度,消息,任务,Run"
    }
}

/// RFC 4180 引号转义 + Excel 公式注入防护。
///
/// 以 = + - @ 或制表/回车开头的单元格会被 Excel 当成公式执行；标题、主播名、
/// 错误消息都来自平台，必须在写文件前统一加前导单引号变成纯文本。
fn csv_cell(value: &str) -> String {
    let unsafe_prefix = value
        .trim_start_matches([' ', '\t'])
        .chars()
        .next()
        .is_some_and(|ch| matches!(ch, '=' | '+' | '-' | '@' | '\t' | '\r' | '\n'));
    let value = if unsafe_prefix {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// 把当前检索条件下的阶段事件导出为 CSV，返回写入条数。
#[tauri::command]
pub fn task_events_export(
    state: State<'_, AppState>,
    path: String,
    query: Option<String>,
    only_failed: Option<bool>,
    platform: Option<String>,
    lang: Option<String>,
) -> Result<hivecast_ipc::TaskEventExport, AppError> {
    use std::io::Write;

    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err(AppError::User("导出路径为空".into()));
    }
    if target.is_dir() {
        return Err(AppError::User(
            "导出路径是一个目录，请选择 .csv 文件".into(),
        ));
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(AppError::User(format!(
                "导出目录不存在：{}",
                parent.display()
            )));
        }
    }
    let result = state.db.task_event_search(
        query.as_deref().unwrap_or_default(),
        only_failed.unwrap_or(false),
        platform.as_deref(),
        50_000,
    )?;
    let mut file = std::fs::File::create(&target)
        .map_err(|err| AppError::User(format!("无法创建导出文件：{err}")))?;
    // 带 BOM，Excel 直接打开不会把中文表头显示成乱码。
    file.write_all("\u{feff}".as_bytes())
        .map_err(|err| AppError::User(format!("写入导出文件失败：{err}")))?;
    writeln!(file, "{}", csv_header(lang.as_deref()))
        .map_err(|err| AppError::User(format!("写入导出文件失败：{err}")))?;
    for hit in &result.hits {
        let time = crate::clock::format_local_opt(hit.at, "%Y-%m-%d %H:%M:%S")
            .unwrap_or_else(|| hit.at.to_string());
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{}",
            csv_cell(&time),
            csv_cell(hit.platform.as_deref().unwrap_or_default()),
            csv_cell(
                hit.room_key
                    .as_deref()
                    .or(hit.room_id.as_deref())
                    .unwrap_or_default()
            ),
            csv_cell(hit.host_name.as_deref().unwrap_or_default()),
            csv_cell(hit.room_title.as_deref().unwrap_or_default()),
            csv_cell(&csv_stage(lang.as_deref(), &hit.stage)),
            csv_cell(&csv_state(lang.as_deref(), &hit.state)),
            hit.percent,
            csv_cell(hit.message.as_deref().unwrap_or_default()),
            csv_cell(&hit.task_id),
            csv_cell(hit.run_id.as_deref().unwrap_or_default()),
        )
        .map_err(|err| AppError::User(format!("写入导出文件失败：{err}")))?;
    }
    if result.total > result.hits.len() as i64 {
        tracing::warn!(
            path = %target.display(),
            written = result.hits.len(),
            total = result.total,
            "任务事件导出达到上限，只写入最近的事件"
        );
    }
    tracing::info!(path = %target.display(), count = result.hits.len(), total = result.total, "任务事件已导出");
    Ok(hivecast_ipc::TaskEventExport {
        written: result.hits.len() as i64,
        total: result.total,
    })
}

/// 任务中心的运行中列表 = 录制（含开录中）+ 后处理/合并/弹幕导出；历史来自 task_history。
#[tauri::command]
pub fn task_board(state: State<'_, AppState>, limit: Option<usize>) -> TaskBoard {
    let mut board = state.jobs.board(limit.unwrap_or(100).clamp(1, 500));
    let mut live = Vec::new();
    for recording in state.recorder.active_all() {
        let room = state.db.get_room(&recording.room_id).ok().flatten();
        let title = room
            .as_ref()
            .map(|room| room.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| recording.room_id.clone());
        live.push(TaskRecord {
            id: format!("record:{}", recording.session_id),
            kind: "record".into(),
            title,
            detail: Some(recording.quality),
            state: "running".into(),
            percent: 0,
            stage: Some("recording".into()),
            message: None,
            session_id: Some(recording.session_id),
            room_id: Some(recording.room_id),
            started_at: recording.started_at,
            queued_at: Some(recording.started_at),
            started_running_at: Some(recording.started_at),
            finished_at: None,
        });
    }
    for room_id in state.recorder.starting_rooms() {
        let title = state
            .db
            .get_room(&room_id)
            .ok()
            .flatten()
            .map(|room| room.title)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| room_id.clone());
        live.push(TaskRecord {
            id: format!("record:starting:{room_id}"),
            kind: "record".into(),
            title,
            detail: Some("开录中".into()),
            state: "running".into(),
            percent: 0,
            stage: Some("starting".into()),
            message: None,
            session_id: None,
            room_id: Some(room_id),
            started_at: now_ts(),
            queued_at: Some(now_ts()),
            started_running_at: None,
            finished_at: None,
        });
    }
    live.append(&mut board.running);
    board.running = live;
    board
}

/// 一条录像登记过的全部文件（主文件、源封装、历史派生）。
#[tauri::command]
pub fn media_files(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<MediaFile>, AppError> {
    state.db.media_files(&session_id)
}

/// 当前平台的回收站能力与位置。
#[tauri::command]
pub fn trash_info() -> TrashInfo {
    crate::post::trash_info()
}

/// 在文件管理器里打开回收站；Linux 先确保目录存在（可能还没删过东西）。
#[tauri::command]
pub fn trash_open() -> Result<(), AppError> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg("shell:RecycleBinFolder")
            .spawn()
            .map_err(|err| AppError::Internal(format!("打开回收站失败：{err}")))?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let dir = crate::post::trash_dir()
            .ok_or_else(|| AppError::User("当前系统没有可打开的回收站目录".into()))?;
        std::fs::create_dir_all(&dir).ok();
        // macOS 的 ~/.Trash 是隐藏目录，用 open -R 只会定位到用户主目录里的隐藏项；
        // 直接 open 才会让访达打开「废纸篓」窗口。
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(&dir)
                .spawn()
                .map_err(|err| AppError::Internal(format!("打开回收站失败：{err}")))?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            reveal_in_file_manager(&dir)
        }
    }
}

// ---- 关闭与退出 ----

/// 当前还没结束的录制与后处理任务，前端在退出提示里逐条列出。
#[tauri::command]
pub fn active_tasks(state: State<'_, AppState>) -> Vec<ActiveTask> {
    crate::lifecycle::active_tasks(&state)
}

/// 用户确认「现在就退出」；只有任务清空后前端才会走到这里。
#[tauri::command]
pub async fn close_quit_now(app: AppHandle) {
    tracing::info!(target: "hivecast_lib::commands", "用户确认立即退出");
    // 先把后处理停下来：run 收到取消后会杀掉 ffmpeg 并删掉半成品临时文件。
    // 最多等 6 秒，超时也退出，不能让一个卡住的编码器挡住关闭。
    crate::post::shutdown().await;
    crate::lifecycle::quit_now(&app);
}

/// 用户勾了「任务完成后自动退出」。
#[tauri::command]
pub fn close_arm_quit(app: AppHandle) {
    tracing::info!(target: "hivecast_lib::commands", "用户选择任务完成后自动退出");
    crate::lifecycle::arm_auto_quit(&app);
}

/// 撤销「任务完成后自动退出」，应用继续留着。
#[tauri::command]
pub fn close_cancel_quit() {
    tracing::info!(target: "hivecast_lib::commands", "用户取消任务完成后自动退出");
    crate::lifecycle::cancel_auto_quit();
}
