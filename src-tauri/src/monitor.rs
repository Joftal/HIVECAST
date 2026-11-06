//! 监控循环：按可配置间隔刷新房间状态，并驱动「开播自动录 / 下播自动停」。
//!
//! 节奏：间隔来自设置（默认 30 秒）。0 = 关闭自动检测，只能手动刷新。
//! 每个房间独立排期；遇到限流/网络错误时该房间下一次检测翻倍退避，
//! 成功即清零，避免被平台限流后还在原地猛打。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::broker::StreamBroker;
use crate::db::{now_ts, Db};
use crate::disk;
use crate::platform::{PlatformError, RoomRef};
use crate::recorder::{Recorder, EVENT_ROOMS_CHANGED};
use crate::settings;

/// 同时在途的房间检测数：再多就只是在替平台限流做压力测试。
const CONCURRENCY: usize = 4;
/// 单次检测失败后的最大退避倍数（相对基础间隔）。
const BACKOFF_MAX_SHIFT: u32 = 5;

/// 每个房间的下次检测时间与连续失败次数。
#[derive(Default, Clone, Copy)]
struct Schedule {
    next_at: i64,
    failures: u32,
}

pub fn spawn(app: AppHandle, db: Arc<Db>, broker: Arc<StreamBroker>, recorder: Arc<Recorder>) {
    tokio::spawn(async move {
        // 启动后先等 2 秒，避免和窗口首帧抢资源。
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut schedule: HashMap<String, Schedule> = HashMap::new();
        loop {
            tick(&app, &db, &broker, &recorder, &mut schedule).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

pub fn room_ref(room: &hivecast_ipc::RoomRecord) -> RoomRef {
    RoomRef {
        platform: room.platform.clone(),
        room_key: room.room_key.clone(),
        canonical_url: room.canonical_url.clone(),
        input: room.canonical_url.clone(),
    }
}

async fn tick(
    app: &AppHandle,
    db: &Arc<Db>,
    broker: &Arc<StreamBroker>,
    recorder: &Arc<Recorder>,
    schedule: &mut HashMap<String, Schedule>,
) {
    let rooms = match db.list_rooms() {
        Ok(rooms) => rooms,
        Err(err) => {
            tracing::warn!(%err, "读取房间列表失败");
            return;
        }
    };

    // 磁盘红线：录制中的房间要主动收尾，否则会一路写到系统盘满。
    if disk::is_low(&recorder.media_root()) {
        for room in &rooms {
            if recorder.is_recording(&room.id) {
                tracing::warn!(room = %room.host_name, "磁盘剩余空间低于红线，停止录制");
                let _ = recorder.stop(app, &room.id, "disk_full").await;
            }
        }
    }

    let settings = settings::load(db);
    let base = settings.monitor_interval_sec as i64;
    if base <= 0 {
        // 关闭自动检测：清空排期，恢复时所有房间立即检测一次。
        schedule.clear();
        return;
    }

    // 房间被删掉后清掉它的排期，避免长跑后 map 越来越大。
    let alive: HashSet<&str> = rooms.iter().map(|room| room.id.as_str()).collect();
    schedule.retain(|id, _| alive.contains(id.as_str()));

    let now = now_ts();
    let mut due = Vec::new();
    for room in rooms {
        // 手动暂停监控的房间完全不参与轮询；已有录制继续，直到用户停止或平台断流。
        if room.monitor_paused {
            schedule.remove(&room.id);
            continue;
        }
        let entry = schedule.entry(room.id.clone()).or_default();
        if entry.next_at <= now {
            due.push(room);
        }
    }
    if due.is_empty() {
        return;
    }

    let mut set = tokio::task::JoinSet::new();
    // 用信号量限制在途请求数，而不是一次把 due 全扔出去。
    let permits = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
    for room in due {
        let app = app.clone();
        let db = db.clone();
        let broker = broker.clone();
        let recorder = recorder.clone();
        let permits = permits.clone();
        set.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let outcome = check_room(&app, &db, &broker, &recorder, &room).await;
            (room.id, outcome)
        });
    }
    while let Some(joined) = set.join_next().await {
        let Ok((id, outcome)) = joined else {
            continue;
        };
        let entry = schedule.entry(id).or_default();
        let wait = match outcome {
            Ok(()) => {
                entry.failures = 0;
                base
            }
            Err(throttled) => {
                entry.failures = entry.failures.saturating_add(1);
                if settings.monitor_backoff_enabled && throttled {
                    let shift = entry.failures.min(BACKOFF_MAX_SHIFT);
                    let grown = base.saturating_mul(1i64 << shift);
                    grown.min(settings.monitor_backoff_cap_sec as i64)
                } else {
                    base
                }
            }
        };
        entry.next_at = now_ts() + wait;
    }
}

/// 检测一个房间；返回 Err(是否限流) 让外层决定要不要退避。
async fn check_room(
    app: &AppHandle,
    db: &Db,
    broker: &StreamBroker,
    recorder: &Arc<Recorder>,
    room: &hivecast_ipc::RoomRecord,
) -> Result<(), bool> {
    let reference = room_ref(room);
    match broker.room_info_fresh(&reference).await {
        Ok(info) => {
            let changed = info.is_live != room.is_live
                || info.room_name != room.title
                || info.host_name != room.host_name;
            if let Err(err) = db.update_room_info(&room.id, &info) {
                tracing::warn!(%err, "更新房间信息失败");
            }
            if changed {
                let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            }

            if info.is_live {
                // 手动停录过的房间在本次直播内不再自动拉起，等主播下播后复位。
                if room.auto_record
                    && !recorder.is_recording(&room.id)
                    && !recorder.auto_suppressed(&room.id)
                    && disk::can_start(&recorder.media_root())
                {
                    if let Err(err) = recorder.start(app, broker, room).await {
                        tracing::warn!(room = %room.host_name, %err, "自动开录失败");
                        recorder.set_error(&room.id, err.to_string());
                        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                    }
                }
            } else {
                recorder.resume_auto(&room.id);
                if recorder.is_recording(&room.id) {
                    let _ = recorder.stop(app, &room.id, "stream_ended").await;
                }
            }
            Ok(())
        }
        Err(err) => {
            let _ = db.touch_room(&room.id);
            let hint = match &err {
                PlatformError::RoomNotFound(_) => "房间不存在或已注销".to_string(),
                PlatformError::NotLive => "房间未开播".to_string(),
                other => other.to_string(),
            };
            tracing::debug!(room = %room.host_name, %hint, "刷新房间失败");
            if matches!(err, PlatformError::NotLive) {
                // 未开播是正常状态，不算失败，也不该退避。
                return Ok(());
            }
            recorder.set_error(&room.id, hint);
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            Err(err.is_throttle())
        }
    }
}
