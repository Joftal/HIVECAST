//! 监控循环：定时刷新房间状态，并驱动「开播自动录 / 下播自动停」。
//!
//! 轮询节奏：直播中 15 秒、离线 30 秒；实际请求还要过 StreamBroker 的
//! 1.5 秒房间闸门，所以多个房间不会同时打爆平台。

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::broker::StreamBroker;
use crate::db::{now_ts, Db};
use crate::disk;
use crate::platform::{PlatformError, RoomRef};
use crate::recorder::{Recorder, EVENT_ROOMS_CHANGED};

const LIVE_INTERVAL: i64 = 15;
const OFFLINE_INTERVAL: i64 = 30;

pub fn spawn(app: AppHandle, db: Arc<Db>, broker: Arc<StreamBroker>, recorder: Arc<Recorder>) {
    tokio::spawn(async move {
        // 启动后先等 2 秒，避免和窗口首帧抢资源。
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            tick(&app, &db, &broker, &recorder).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
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

async fn tick(app: &AppHandle, db: &Db, broker: &StreamBroker, recorder: &Arc<Recorder>) {
    let rooms = match db.list_rooms() {
        Ok(rooms) => rooms,
        Err(err) => {
            tracing::warn!(%err, "读取房间列表失败");
            return;
        }
    };

    // 磁盘红线：录制中的房间要主动收尾，否则会一路写到系统盘满。
    if disk::is_low(recorder.media_root()) {
        for room in &rooms {
            if recorder.is_recording(&room.id) {
                tracing::warn!(room = %room.host_name, "磁盘剩余空间低于红线，停止录制");
                let _ = recorder.stop(app, &room.id, "disk_full").await;
            }
        }
    }

    for room in rooms {
        let interval = if room.is_live {
            LIVE_INTERVAL
        } else {
            OFFLINE_INTERVAL
        };
        if let Some(last) = room.last_checked_at {
            if now_ts() - last < interval {
                continue;
            }
        }

        let reference = room_ref(&room);
        match broker.room_info(&reference).await {
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
                        && disk::can_start(recorder.media_root())
                    {
                        if let Err(err) = recorder.start(app, broker, &room).await {
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
            }
            Err(err) => {
                let _ = db.touch_room(&room.id);
                let hint = match &err {
                    PlatformError::RoomNotFound(_) => "房间不存在或已注销".to_string(),
                    PlatformError::NotLive => "房间未开播".to_string(),
                    other => other.to_string(),
                };
                tracing::debug!(room = %room.host_name, %hint, "刷新房间失败");
                if !matches!(err, PlatformError::NotLive) {
                    recorder.set_error(&room.id, hint);
                    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                }
            }
        }
    }
}
