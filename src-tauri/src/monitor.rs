//! 监控循环：按可配置间隔刷新房间状态，并驱动「开播自动录 / 下播自动停」。
//!
//! 节奏来自设置（默认 30 秒，0 = 关闭自动检测）。调度与请求彻底解耦：
//! 每一拍只把到期的房间扔进后台任务，绝不等请求回来，因此某个平台抽风
//! 只会拖慢它自己的房间，不会把整轮调度卡住（HTTP 超时是 10 秒）。
//!
//! 排期用固定网格（上次排期 + 间隔），再叠加 ±10% 抖动，避免所有房间
//! 长期同相位、每隔一个间隔一起打一次尖峰。首次排期按房间 id 哈希散开。
//!
//! 退避只对「真限流」计数：broker 冷却期内返回 Throttled（带 retry_after），
//! 监控按它重排、不计失败，避免一次 429 被放大成五六次失败。
//! 房间不存在、解析失败这类错误只更新提示，不改节奏。

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Semaphore};

use crate::broker::StreamBroker;
use crate::db::{now_ts, Db};
use crate::disk;
use crate::platform::{PlatformError, RoomRef};
use crate::recorder::{Recorder, EVENT_ROOMS_CHANGED};
use crate::settings;

/// 全局在途上限：跨平台的总闸门，防止房间特别多时把本机连接数打满。
const GLOBAL_CONCURRENCY: usize = 12;
/// 单平台在途上限：限流是按平台算的，不同平台之间不该互相排队。
const PLATFORM_CONCURRENCY: usize = 4;
/// 连续限流时的最大退避倍数（相对基础间隔）。
const BACKOFF_MAX_SHIFT: u32 = 5;
/// 排期抖动幅度（占基础间隔的百分比）。
const JITTER_PCT: i64 = 10;
/// 退避抖动幅度（占退避时长的百分比），用来错开同时被限流的房间。
const BACKOFF_JITTER_PCT: i64 = 20;
/// 调度节拍：每秒检查一次谁到期。
const TICK: Duration = Duration::from_secs(1);

static JITTER_SALT: AtomicU64 = AtomicU64::new(0);

/// 每个房间的下次检测时间与失败计数。
#[derive(Default, Clone, Copy)]
struct Schedule {
    next_at: i64,
    /// 连续限流次数；成功即清零，普通错误不累加。
    throttle_streak: u32,
    /// 普通错误计数，只用于观测，不影响节奏。
    error_count: u32,
    /// 请求是否还在路上。
    inflight: bool,
    /// 当前 next_at 是不是退避算出来的（退避不跟随间隔设置收缩）。
    backoff: bool,
}

/// 一次检测的结果分类，决定下一拍怎么排期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// 正常返回（含「未开播」这种正常状态）。
    Ok,
    /// 平台限流 / 网络抖动：计入退避。
    Throttled,
    /// broker 冷却中：按给定秒数重排，不计失败。
    Retry(i64),
    /// 房间本身的问题（不存在、解析失败等）：只更新提示，不改节奏。
    Failed,
}

pub fn spawn(app: AppHandle, db: Arc<Db>, broker: Arc<StreamBroker>, recorder: Arc<Recorder>) {
    tokio::spawn(async move {
        // 启动后先等 2 秒，避免和窗口首帧抢资源。
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut schedule: HashMap<String, Schedule> = HashMap::new();
        let mut platforms: HashMap<String, Arc<Semaphore>> = HashMap::new();
        let global = Arc::new(Semaphore::new(GLOBAL_CONCURRENCY));
        let (tx, mut rx) = mpsc::unbounded_channel::<(String, Outcome)>();
        loop {
            let settings = settings::load(&db);
            let base = settings.monitor_interval_sec as i64;
            let cap = settings.monitor_backoff_cap_sec as i64;
            let backoff_on = settings.monitor_backoff_enabled;
            if base > 0 {
                drain(&mut rx, &mut schedule, base, cap, backoff_on);
            } else {
                // 关闭自动检测：清空排期，恢复时所有房间立即检测一次。
                schedule.clear();
                while rx.try_recv().is_ok() {}
            }
            tick(
                &app,
                &db,
                &broker,
                &recorder,
                &mut schedule,
                &mut platforms,
                &global,
                &tx,
                base,
            );
            tokio::time::sleep(TICK).await;
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

/// 把已经回来的结果落进排期表。只做内存操作，不会阻塞。
fn drain(
    rx: &mut mpsc::UnboundedReceiver<(String, Outcome)>,
    schedule: &mut HashMap<String, Schedule>,
    base: i64,
    cap: i64,
    backoff_on: bool,
) {
    while let Ok((id, outcome)) = rx.try_recv() {
        if let Some(entry) = schedule.get_mut(&id) {
            apply(&id, entry, outcome, base, cap, backoff_on);
        }
    }
}

fn apply(
    id: &str,
    entry: &mut Schedule,
    outcome: Outcome,
    base: i64,
    cap: i64,
    backoff_on: bool,
) {
    let now = now_ts();
    entry.inflight = false;
    match outcome {
        Outcome::Ok => {
            entry.throttle_streak = 0;
            entry.error_count = 0;
            entry.backoff = false;
            entry.next_at = grid_next(entry.next_at, now, base) + jitter(id, base * JITTER_PCT / 100);
        }
        Outcome::Throttled => {
            entry.throttle_streak = entry.throttle_streak.saturating_add(1);
            entry.error_count = entry.error_count.saturating_add(1);
            let wait = backoff_wait(base, entry.throttle_streak, cap, backoff_on);
            let wait = wait + jitter(id, wait * BACKOFF_JITTER_PCT / 100);
            entry.next_at = now + wait.max(1);
            entry.backoff = backoff_on;
        }
        Outcome::Retry(secs) => {
            // broker 已经知道什么时候能恢复，照着排就行，不算失败。
            entry.next_at = now + secs.max(1);
            entry.backoff = true;
        }
        Outcome::Failed => {
            entry.error_count = entry.error_count.saturating_add(1);
            entry.backoff = false;
            entry.next_at = now + base + jitter(id, base * JITTER_PCT / 100);
        }
    }
}

/// 固定网格：在上一拍上加一个间隔。落后太多（例如机器休眠）就从现在重算，
/// 避免恢复瞬间把欠下的检测一口气全补出去。
fn grid_next(previous: i64, now: i64, base: i64) -> i64 {
    let next = previous.saturating_add(base);
    if next <= now {
        now + base
    } else {
        next
    }
}

/// 限流退避：基础间隔 × 2^streak，封顶 cap；关闭退避时维持基础间隔。
fn backoff_wait(base: i64, streak: u32, cap: i64, enabled: bool) -> i64 {
    if !enabled {
        return base;
    }
    let shift = streak.min(BACKOFF_MAX_SHIFT);
    base.saturating_mul(1i64 << shift).min(cap.max(base))
}

/// 首次排期把房间散在第一个间隔里，避免启动时所有房间同时到期。
fn spread(id: &str, base: i64) -> i64 {
    if base <= 1 {
        return 0;
    }
    (hash64(id, 0) % base as u64) as i64
}

/// 确定性伪随机抖动：同一房间同一时刻可复现，便于单测与排障。
fn jitter(id: &str, span: i64) -> i64 {
    if span <= 0 {
        return 0;
    }
    let salt = JITTER_SALT.fetch_add(1, Ordering::Relaxed);
    (hash64(id, salt) % (span as u64 * 2 + 1)) as i64 - span
}

fn hash64(id: &str, salt: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    salt.hash(&mut hasher);
    hasher.finish()
}

#[allow(clippy::too_many_arguments)]
fn tick(
    app: &AppHandle,
    db: &Arc<Db>,
    broker: &Arc<StreamBroker>,
    recorder: &Arc<Recorder>,
    schedule: &mut HashMap<String, Schedule>,
    platforms: &mut HashMap<String, Arc<Semaphore>>,
    global: &Arc<Semaphore>,
    tx: &mpsc::UnboundedSender<(String, Outcome)>,
    base: i64,
) {
    let rooms = match db.list_rooms() {
        Ok(rooms) => rooms,
        Err(err) => {
            tracing::warn!(%err, "读取房间列表失败");
            return;
        }
    };

    // 磁盘红线：录制中的房间要主动收尾，否则会一路写到系统盘满。
    // 收尾可能要等 ffmpeg 退出，扔进后台，别拖住调度。
    if disk::is_low(&recorder.media_root()) {
        for room in &rooms {
            if recorder.is_recording(&room.id) {
                tracing::warn!(room = %room.host_name, "磁盘剩余空间低于红线，停止录制");
                let app = app.clone();
                let recorder = recorder.clone();
                let id = room.id.clone();
                tokio::spawn(async move {
                    let _ = recorder.stop(&app, &id, "disk_full").await;
                });
            }
        }
    }

    if base <= 0 {
        return;
    }

    // 房间被删掉后清掉它的排期，避免长跑后 map 越来越大。
    let alive: HashSet<&str> = rooms.iter().map(|room| room.id.as_str()).collect();
    schedule.retain(|id, _| alive.contains(id.as_str()));

    let now = now_ts();
    for room in rooms {
        // 手动暂停监控的房间完全不参与轮询；已有录制继续，直到用户停止或平台断流。
        if room.monitor_paused {
            schedule.remove(&room.id);
            continue;
        }
        let entry = schedule.entry(room.id.clone()).or_insert_with(|| Schedule {
            next_at: now + spread(&room.id, base),
            ..Default::default()
        });
        // 间隔被调小后，旧排期不要继续拖几分钟。
        if !entry.backoff && entry.next_at > now + base {
            entry.next_at = now + spread(&room.id, base);
        }
        if entry.inflight || entry.next_at > now {
            continue;
        }
        entry.inflight = true;

        let permits = platforms
            .entry(room.platform.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(PLATFORM_CONCURRENCY)))
            .clone();
        let app = app.clone();
        let db = db.clone();
        let broker = broker.clone();
        let recorder = recorder.clone();
        let global = global.clone();
        let tx = tx.clone();
        let id = room.id.clone();
        tokio::spawn(async move {
            let outcome = run_check(&app, &db, &broker, &recorder, &room, permits, global).await;
            let _ = tx.send((id, outcome));
        });
    }
}

/// 先拿平台名额再拿全局名额：平台名额才是真正的瓶颈，
/// 反过来的话一个平台的请求会占着全局名额等自己人。
async fn run_check(
    app: &AppHandle,
    db: &Db,
    broker: &StreamBroker,
    recorder: &Arc<Recorder>,
    room: &hivecast_ipc::RoomRecord,
    permits: Arc<Semaphore>,
    global: Arc<Semaphore>,
) -> Outcome {
    let Ok(_platform) = permits.acquire_owned().await else {
        return Outcome::Failed;
    };
    let Ok(_global) = global.acquire_owned().await else {
        return Outcome::Failed;
    };
    check_room(app, db, broker, recorder, room).await
}

/// 检测一个房间，并把结果归类成排期需要的四种情况。
async fn check_room(
    app: &AppHandle,
    db: &Db,
    broker: &StreamBroker,
    recorder: &Arc<Recorder>,
    room: &hivecast_ipc::RoomRecord,
) -> Outcome {
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
            Outcome::Ok
        }
        Err(err) => {
            let _ = db.touch_room(&room.id);
            // 未开播是正常状态，不算失败，也不该退避。
            if matches!(err, PlatformError::NotLive) {
                return Outcome::Ok;
            }
            // broker 冷却期内这次请求根本没出门，按它给的秒数重排即可。
            if let Some(secs) = err.retry_after() {
                return Outcome::Retry(secs as i64);
            }
            let hint = match &err {
                PlatformError::RoomNotFound(_) => "房间不存在或已注销".to_string(),
                other => other.to_string(),
            };
            tracing::debug!(room = %room.host_name, %hint, "刷新房间失败");
            recorder.set_error(&room.id, hint);
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            if err.is_throttle() {
                Outcome::Throttled
            } else {
                Outcome::Failed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(next_at: i64) -> Schedule {
        Schedule {
            next_at,
            ..Default::default()
        }
    }

    #[test]
    fn spread_stays_inside_one_interval() {
        for id in ["a", "b", "c", "rm-b6", "rm-dy9999"] {
            let offset = spread(id, 30);
            assert!((0..30).contains(&offset), "{id} -> {offset}");
        }
        assert_eq!(spread("a", 0), 0);
        assert_eq!(spread("a", 1), 0);
    }

    #[test]
    fn jitter_is_bounded() {
        for _ in 0..200 {
            let value = jitter("rm-b6", 3);
            assert!((-3..=3).contains(&value), "{value}");
        }
        assert_eq!(jitter("rm-b6", 0), 0);
    }

    #[test]
    fn grid_next_keeps_cadence_and_avoids_burst() {
        // 准时完成：在上一拍上加一个间隔。
        assert_eq!(grid_next(100, 100, 30), 130);
        // 落后两拍（例如休眠）：从现在重算，不补发。
        assert_eq!(grid_next(100, 200, 30), 230);
    }

    #[test]
    fn backoff_grows_caps_and_respects_switch() {
        assert_eq!(backoff_wait(30, 1, 120, true), 60);
        assert_eq!(backoff_wait(30, 2, 120, true), 120);
        assert_eq!(backoff_wait(30, 9, 120, true), 120);
        assert_eq!(backoff_wait(30, 3, 120, false), 30);
        // cap 小于基础间隔时不该把间隔压得比基础还短。
        assert_eq!(backoff_wait(30, 2, 10, true), 30);
    }

    #[test]
    fn throttle_only_counts_real_throttles() {
        let mut s = entry(100);
        apply("rm-b6", &mut s, Outcome::Throttled, 30, 120, true);
        assert_eq!(s.throttle_streak, 1);
        apply("rm-b6", &mut s, Outcome::Throttled, 30, 120, true);
        assert_eq!(s.throttle_streak, 2);
        // 普通错误不累加限流计数，也不放大退避。
        apply("rm-b6", &mut s, Outcome::Failed, 30, 120, true);
        assert_eq!(s.throttle_streak, 2);
        assert_eq!(s.error_count, 3);
        apply("rm-b6", &mut s, Outcome::Ok, 30, 120, true);
        assert_eq!(s.throttle_streak, 0);
        assert_eq!(s.error_count, 0);
    }

    #[test]
    fn broker_cooldown_does_not_count_as_failure() {
        let mut s = entry(100);
        apply("rm-b6", &mut s, Outcome::Retry(5), 30, 120, true);
        assert_eq!(s.throttle_streak, 0);
        assert_eq!(s.error_count, 0);
        assert!(s.next_at >= now_ts() + 4);
    }

    #[test]
    fn in_flight_rooms_are_not_scheduled_twice() {
        let mut s = entry(0);
        s.inflight = true;
        assert!(s.inflight);
        apply("rm-b6", &mut s, Outcome::Ok, 30, 120, true);
        assert!(!s.inflight);
    }
}
