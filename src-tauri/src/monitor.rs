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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Semaphore};

use crate::broker::StreamBroker;
use crate::db::Db;
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
/// 设置与房间列表的读库缓存（B7）：每秒一拍、每次都查一遍 SQLite 没有意义。
/// 2 秒的延迟对「改间隔 / 加房间 / 暂停监控」完全无感，读库却从 1 次/秒降到 0.5 次/秒。
const SETTINGS_TTL: Duration = Duration::from_secs(2);
const ROOMS_TTL: Duration = Duration::from_secs(2);
/// 首次检测最多往后拖这么久（G1）。
///
/// 以前首个排期直接散在「一整个检测间隔」里：间隔调成 30 分钟，
/// 重启后房间可能要等半小时才第一次检测，界面一直显示「未检测」。
const FIRST_CHECK_MAX_DELAY: i64 = 30;
/// 平台信息没有结构性变化时，写库的最小间隔（G2）。
/// 在线人数这类字段每拍都在跳，但没必要每秒整行 UPDATE 一次。
const ROOM_TOUCH_INTERVAL: i64 = 10;

/// 每个房间的下次检测时间与失败计数。
#[derive(Clone, Copy)]
struct Schedule {
    /// 未加抖动的固定网格点。抖动只加在 next_at 上，网格本身严格按间隔推进，
    /// 否则每次重排都叠一次 ±10%，排期会慢慢漂移（C6）。
    grid_at: Instant,
    /// 下次检测时刻。用单调时钟：改系统时间不该让监控停摆或瞬间全部到期（R4）。
    next_at: Instant,
    /// 连续限流次数；成功即清零，普通错误不累加。
    throttle_streak: u32,
    /// 普通错误计数，只用于观测，不影响节奏。
    error_count: u32,
    /// 请求是否还在路上。
    inflight: bool,
    /// 当前 next_at 是不是退避算出来的（退避不跟随间隔设置收缩）。
    backoff: bool,
    /// 算出当前 next_at 时用的基础间隔（秒）。用来识别「用户把间隔调小了」，
    /// 不能拿 next_at 和 now+interval 直接比：成功排期里带着 +10% 抖动，
    /// 正抖动会让长间隔每次都被误判成「旧排期太远」而被重置（G7）。
    interval_secs: i64,
    /// 最近一次结果返回的墙钟时间与分类，详情抽屉做诊断展示。
    last_checked_at: Option<i64>,
    last_outcome: Option<&'static str>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            grid_at: Instant::now(),
            next_at: Instant::now(),
            throttle_streak: 0,
            error_count: 0,
            inflight: false,
            backoff: false,
            interval_secs: 0,
            last_checked_at: None,
            last_outcome: None,
        }
    }
}

/// 监控排期快照：命令层读取，详情抽屉轮询显示下次检查与退避。
pub type MonitorStatusMap = Arc<Mutex<HashMap<String, hivecast_ipc::MonitorStatus>>>;

/// 间隔被调小后，旧排期不要继续拖几分钟。只在「变小」时重排：
/// 调大间隔时让当前这一拍按旧节奏走完即可，下一拍自然按新间隔推进。
/// 退避条目也要算进来：退避按旧间隔算出的小时级等待，用户调小间隔后
/// 照样得等完（H8）。
fn needs_reschedule(entry: &Schedule, base: i64, now: Instant) -> bool {
    base > 0 && base < entry.interval_secs && entry.next_at > now
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
    /// 房间本身的问题（不存在、解析失败等）：按失败退避，不每拍硬打平台。
    Failed,
}

pub fn spawn(
    app: AppHandle,
    db: Arc<Db>,
    broker: Arc<StreamBroker>,
    recorder: Arc<Recorder>,
    statuses: MonitorStatusMap,
) {
    tokio::spawn(async move {
        // 启动后先等 2 秒，避免和窗口首帧抢资源。
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut schedule: HashMap<String, Schedule> = HashMap::new();
        let mut platforms: HashMap<String, Arc<Semaphore>> = HashMap::new();
        let global = Arc::new(Semaphore::new(GLOBAL_CONCURRENCY));
        let (tx, mut rx) = mpsc::unbounded_channel::<(String, Outcome)>();
        let mut settings = settings::load(&db);
        let mut settings_at = Instant::now();
        // 房间列表缓存：单独的 Option 而不是包在 tick 里，跨拍复用（B7）。
        let mut rooms_cache: Option<(Instant, Vec<hivecast_ipc::RoomRecord>)> = None;
        // B 站批量状态的自有节奏：所有可批量的房间共用一次请求，
        // 不能跟着每个房间错开的 next_at 走，否则一拍一个请求反而更多。
        let mut batch_at: Option<Instant> = None;
        // 记住 batch_at 是按哪个间隔排的；用户改监控间隔后要按新间隔重排。
        let mut batch_interval: Option<i64> = None;
        loop {
            if settings_at.elapsed() >= SETTINGS_TTL {
                settings = settings::load(&db);
                settings_at = Instant::now();
            }
            let base_secs = settings.monitor_interval_sec as i64;
            let cap_secs = settings.monitor_backoff_cap_sec as i64;
            let backoff_on = settings.monitor_backoff_enabled;
            // 间隔被调成 0（关闭自动检测）时排期不清理、在途结果也照收：
            // 否则 inflight 标记永远留着，重开检测时会以为「这一路已经在查」
            // 从而跳过，或者同一房间连着被查两次（F4）。
            // 落结果用最后一次有效的间隔，避免 base=0 把网格算成一拍 0 秒。
            let base = Duration::from_secs(base_secs.max(1) as u64);
            let cap = Duration::from_secs(cap_secs.max(1) as u64);
            drain(&mut rx, &mut schedule, base, cap, backoff_on);
            tick(
                &app,
                &db,
                &broker,
                &recorder,
                &mut schedule,
                &mut platforms,
                &global,
                &tx,
                &mut rooms_cache,
                &mut batch_at,
                &mut batch_interval,
                base_secs,
            );
            snapshot_statuses(&schedule, &statuses);
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

/// 每秒把排期快照写给命令层；详情抽屉只读这一份，不直接碰调度内部。
fn snapshot_statuses(schedule: &HashMap<String, Schedule>, statuses: &MonitorStatusMap) {
    let now = Instant::now();
    let mut guard = statuses.lock().expect("监控状态锁被污染");
    guard.clear();
    for (id, entry) in schedule {
        guard.insert(
            id.clone(),
            hivecast_ipc::MonitorStatus {
                room_id: id.clone(),
                next_check_in_secs: entry.next_at.saturating_duration_since(now).as_secs() as i64,
                inflight: entry.inflight,
                backoff: entry.backoff,
                throttle_streak: entry.throttle_streak,
                error_count: entry.error_count,
                last_checked_at: entry.last_checked_at,
                last_outcome: entry.last_outcome.map(str::to_string),
            },
        );
    }
}

/// 把已经回来的结果落进排期表。只做内存操作，不会阻塞。
fn drain(
    rx: &mut mpsc::UnboundedReceiver<(String, Outcome)>,
    schedule: &mut HashMap<String, Schedule>,
    base: Duration,
    cap: Duration,
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
    base: Duration,
    cap: Duration,
    backoff_on: bool,
) {
    let now = Instant::now();
    entry.inflight = false;
    entry.interval_secs = base.as_secs() as i64;
    entry.last_checked_at = Some(crate::db::now_ts());
    entry.last_outcome = Some(match outcome {
        Outcome::Ok => "ok",
        Outcome::Throttled => "throttled",
        Outcome::Retry(_) => "retry",
        Outcome::Failed => "failed",
    });
    match outcome {
        Outcome::Ok => {
            entry.throttle_streak = 0;
            entry.error_count = 0;
            entry.backoff = false;
            // 网格点只按间隔推进；抖动只影响这一次的实际触发时刻（C6）。
            let grid = grid_next(entry.grid_at, now, base);
            entry.grid_at = grid;
            entry.next_at = add_jitter(grid, id, base.mul_f64(JITTER_PCT as f64 / 100.0))
                .max(now + Duration::from_secs(1));
        }
        Outcome::Throttled => {
            entry.throttle_streak = entry.throttle_streak.saturating_add(1);
            entry.error_count = entry.error_count.saturating_add(1);
            let wait = backoff_wait(base, entry.throttle_streak, cap, backoff_on);
            let fire_at = add_jitter(
                now + wait,
                id,
                wait.mul_f64(BACKOFF_JITTER_PCT as f64 / 100.0),
            );
            entry.next_at = fire_at.max(now + Duration::from_secs(1));
            entry.grid_at = entry.next_at;
            entry.backoff = backoff_on;
        }
        Outcome::Retry(secs) => {
            // broker 已经知道什么时候能恢复，照着排就行，不算失败。
            entry.next_at = now + Duration::from_secs(secs.max(1) as u64);
            entry.grid_at = entry.next_at;
            entry.backoff = true;
        }
        Outcome::Failed => {
            entry.error_count = entry.error_count.saturating_add(1);
            // 房间本身出错也退避：不存在的房间不该按检测间隔一直打平台接口，
            // 那只会把同样一条错误刷进日志、刷到前端（F6）。
            let wait = backoff_wait(base, entry.error_count, cap, backoff_on);
            let fire_at = add_jitter(
                now + wait,
                id,
                wait.mul_f64(BACKOFF_JITTER_PCT as f64 / 100.0),
            );
            entry.next_at = fire_at.max(now + Duration::from_secs(1));
            entry.grid_at = entry.next_at;
            entry.backoff = backoff_on;
        }
    }
}

/// 固定网格：在上一拍上加一个间隔。落后太多（例如机器休眠）就从现在重算，
/// 避免恢复瞬间把欠下的检测一口气全补出去。
fn grid_next(previous: Instant, now: Instant, base: Duration) -> Instant {
    let next = previous + base;
    if next <= now {
        now + base
    } else {
        next
    }
}

/// 限流退避：基础间隔 × 2^streak，封顶 cap；关闭退避时维持基础间隔。
fn backoff_wait(base: Duration, streak: u32, cap: Duration, enabled: bool) -> Duration {
    if !enabled {
        return base;
    }
    let shift = streak.min(BACKOFF_MAX_SHIFT);
    let factor = 1u32 << shift;
    base.checked_mul(factor).unwrap_or(cap).min(cap.max(base))
}

/// 给时间点叠加 ±span 的确定性抖动；负偏移不会把时间推到过去。
fn add_jitter(at: Instant, id: &str, span: Duration) -> Instant {
    let ms = span.as_millis().min(i64::MAX as u128) as i64;
    let offset = jitter(id, ms);
    if offset >= 0 {
        at + Duration::from_millis(offset as u64)
    } else {
        at - Duration::from_millis((-offset) as u64)
    }
}

/// 首次排期把房间散在第一个间隔里，避免启动时所有房间同时到期。
///
/// 但最多散 `FIRST_CHECK_MAX_DELAY` 秒：间隔被设成几十分钟时，
/// 也要保证重启后很快能看到每个房间的真实状态（G1）。
fn spread(id: &str, base: i64) -> i64 {
    if base <= 1 {
        return 0;
    }
    let cap = base.min(FIRST_CHECK_MAX_DELAY);
    (hash64(id, 0) % cap as u64) as i64
}

/// 确定性抖动：只由房间 id 决定，同一房间每次重排落在同一偏移上。
///
/// 以前用全局自增盐，所谓「确定性」并不成立（同一房间每次都不同），
/// 而且盐在每拍重排时都会变，排期无法复现、也没法单测（C6）。
fn jitter(id: &str, span: i64) -> i64 {
    if span <= 0 {
        return 0;
    }
    (hash64(id, 0) % (span as u64 * 2 + 1)) as i64 - span
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
    rooms_cache: &mut Option<(Instant, Vec<hivecast_ipc::RoomRecord>)>,
    batch_at: &mut Option<Instant>,
    batch_interval: &mut Option<i64>,
    base: i64,
) {
    // 应用正在退出：监控每秒一拍，若继续检测就会在收尾窗口里再拉起一路，
    // 第二次收尾把它杀成 0 字节会话（C1）。
    if recorder.is_shutting_down() {
        return;
    }
    // 房间列表最多 2 秒读一次：调度本身仍按 1 秒一拍，只是不再每次都查库（B7）。
    let fresh = rooms_cache
        .as_ref()
        .is_some_and(|(at, _)| at.elapsed() < ROOMS_TTL);
    if !fresh {
        match db.list_rooms() {
            Ok(rooms) => *rooms_cache = Some((Instant::now(), rooms)),
            Err(err) => {
                tracing::warn!(%err, "读取房间列表失败");
                // 读库失败时用上一拍快照兜底；一次都没有才放弃这一拍。
                if rooms_cache.is_none() {
                    return;
                }
            }
        }
    }
    let Some((_, cached_rooms)) = rooms_cache.as_ref() else {
        return;
    };
    let rooms = cached_rooms.as_slice();

    // 磁盘红线：按每一路自己的落盘目录检查。用户中途换过媒体目录时，
    // 旧目录写满仍然要停掉还在写它的那一路，否则只能等 ffmpeg 报错（R5）。
    // 一次收齐所有异常目录上的录制再停：按卷去重时只停「遍历到的第一路」，
    // 同一块盘上的另外几路要等下一拍（C3），压盘时白写好几秒。
    //
    // 这里按「目录」查而不是按卷键（G3）：外接盘拔掉后 volume_key 会退化到根盘，
    // 拿卷键判定会把掉线的盘当成系统盘——系统盘还有空间，于是守卫完全失明。
    // 同一个目录的多路只查一次，5 秒缓存另算。
    let media_root = recorder.media_root();
    let mut dir_status: HashMap<std::path::PathBuf, disk::DiskStatus> = HashMap::new();
    let mut to_stop: Vec<(String, String, std::path::PathBuf, &'static str)> = Vec::new();
    for (id, host, file) in recorder.active_targets() {
        // 已经在收尾的这路不用再查：ffmpeg 退出前每秒重复发一次 stop 只会刷日志（B6）。
        if recorder.stopping(&id) {
            continue;
        }
        let dir = target_dir(&file, &media_root);
        let state = *dir_status
            .entry(dir.clone())
            .or_insert_with(|| disk::status(&dir));
        match state {
            disk::DiskStatus::Ready => {}
            disk::DiskStatus::Low => to_stop.push((id, host, dir, "disk_full")),
            disk::DiskStatus::Missing => to_stop.push((id, host, dir, "disk_lost")),
        }
    }
    for (id, host, dir, reason) in to_stop {
        if reason == "disk_lost" {
            tracing::warn!(
                room = %host,
                dir = %dir.display(),
                "录制目录不可用，停止录制"
            );
        } else {
            tracing::warn!(
                room = %host,
                dir = %dir.display(),
                "磁盘剩余空间低于红线，停止录制"
            );
        }
        let app = app.clone();
        let recorder = recorder.clone();
        tokio::spawn(async move {
            let _ = recorder.stop(&app, &id, reason).await;
        });
    }

    if base <= 0 {
        // 关闭自动检测时丢掉批量节奏；重新开启后按新间隔重新散开，
        // 不沿用关闭前的旧 batch_at。
        *batch_interval = None;
        return;
    }

    // 房间被删掉后清掉它的排期，避免长跑后 map 越来越大。
    let alive: HashSet<&str> = rooms.iter().map(|room| room.id.as_str()).collect();
    schedule.retain(|id, _| alive.contains(id.as_str()));

    let now = Instant::now();
    // 批量节奏独立于单房间排期：首次按平台散开，之后严格每间隔一次。
    // 间隔被用户改过（30s → 5m 或反过来）时不能继续按旧节奏：否则调大后
    // 仍会按旧间隔打接口，调小后房间要等旧的长间隔才被查。
    if *batch_interval != Some(base) {
        *batch_interval = Some(base);
        *batch_at = Some(now + Duration::from_secs(spread("batch:bilibili", base) as u64));
    }
    let first_batch = *batch_at
        .get_or_insert_with(|| now + Duration::from_secs(spread("batch:bilibili", base) as u64));
    let batch_due = now >= first_batch;
    let batch_permits = platforms
        .entry("bilibili".into())
        .or_insert_with(|| Arc::new(Semaphore::new(PLATFORM_CONCURRENCY)))
        .clone();
    let mut batch_rooms: Vec<hivecast_ipc::RoomRecord> = Vec::new();
    // 已经有一批还在跑时，不要在空批次的每一拍把 batch_at 往后推；
    // 否则长批次结束后的下一批会被硬生生拖过一个完整间隔。
    let mut batch_inflight = false;

    for room in rooms {
        // 手动暂停监控的房间完全不参与轮询；已有录制继续，直到用户停止或平台断流。
        if room.monitor_paused {
            schedule.remove(&room.id);
            continue;
        }
        let entry = schedule.entry(room.id.clone()).or_insert_with(|| {
            let first = now + Duration::from_secs(spread(&room.id, base) as u64);
            Schedule {
                grid_at: first,
                next_at: first,
                interval_secs: base,
                ..Default::default()
            }
        });
        // 间隔被调小后，旧排期不要继续拖几分钟；网格点一起重置，避免下一拍
        // 又按旧网格 + 新间隔算出一个更远的时刻。
        if needs_reschedule(entry, base, now) {
            if entry.backoff {
                // 退避期间调小间隔：直接按新间隔重排，并退出退避状态，
                // 否则一次失败就把这个房间锁在旧的长等待里（H8）。
                let first = now + Duration::from_secs(base as u64);
                entry.grid_at = first;
                entry.next_at = first;
                entry.backoff = false;
                entry.interval_secs = base;
            } else {
                let first = now + Duration::from_secs(spread(&room.id, base) as u64);
                entry.grid_at = first;
                entry.next_at = first;
                entry.interval_secs = base;
            }
        }

        // B 站且已知 UID 的房间走批量状态：所有房间共用一次请求，
        // 不在批量拍上时保持排期但不单独出门。
        let batchable = room.platform == "bilibili"
            && room
                .platform_uid
                .as_deref()
                .is_some_and(|uid| !uid.trim().is_empty());
        if batchable {
            if !batch_due || entry.inflight {
                batch_inflight |= entry.inflight;
                continue;
            }
            entry.inflight = true;
            batch_rooms.push(room.clone());
            continue;
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
        // 快照是借来的（缓存里的切片），扔进后台任务要自己的副本。
        let room = room.clone();
        tokio::spawn(async move {
            // 再套一层 spawn：检测任务 panic 时也要把结果发回去，否则 inflight
            // 永远清不掉，这个房间从此不再被检测（B8）。
            let check = tokio::spawn(async move {
                run_check(&app, &db, &broker, &recorder, &room, permits, global).await
            });
            let outcome = match check.await {
                Ok(outcome) => outcome,
                Err(err) => {
                    tracing::warn!(%err, "房间检测任务异常退出");
                    Outcome::Failed
                }
            };
            let _ = tx.send((id, outcome));
        });
    }

    if batch_due && (!batch_rooms.is_empty() || !batch_inflight) {
        *batch_at = Some(
            add_jitter(
                now + Duration::from_secs(base as u64),
                "batch:bilibili",
                Duration::from_secs(base as u64).mul_f64(JITTER_PCT as f64 / 100.0),
            )
            .max(now + Duration::from_secs(1)),
        );
        if !batch_rooms.is_empty() {
            tracing::info!(
                target: "hivecast_lib::monitor",
                rooms = batch_rooms.len(),
                interval_sec = base,
                "B 站批量状态检查开始"
            );
            let ids: Vec<String> = batch_rooms.iter().map(|room| room.id.clone()).collect();
            let app = app.clone();
            let db = db.clone();
            let broker = broker.clone();
            let recorder = recorder.clone();
            let global = global.clone();
            let tx = tx.clone();
            let completed = Arc::new(Mutex::new(HashSet::new()));
            let completed_for_task = completed.clone();
            tokio::spawn(async move {
                // 整批 panic 也要给每个房间回结果，否则 inflight 会卡死；
                // 但只补没回传的，已经成功的房间不能因 panic 再吃一次 Failed。
                let inner_tx = tx.clone();
                let guard = tokio::spawn(async move {
                    run_batch_check(
                        &app,
                        &db,
                        &broker,
                        &recorder,
                        batch_rooms,
                        batch_permits,
                        global,
                        inner_tx,
                        completed_for_task,
                    )
                    .await;
                });
                if let Err(err) = guard.await {
                    tracing::warn!(%err, "B 站批量检测任务异常退出");
                    let sent = completed_ids(&completed);
                    for id in ids {
                        if !sent.contains(&id) {
                            let _ = tx.send((id, Outcome::Failed));
                        }
                    }
                }
            });
        }
    }
}

/// 记录已经回传结果的房间，批量任务 panic 时只补没回传的。
///
/// 直接给整批所有 id 补 Failed 会把已经正常落库的房间再算一次失败，
/// 连带把退避计数翻倍，所以成功发送前先记一笔。
fn send_batch_outcome(
    tx: &mpsc::UnboundedSender<(String, Outcome)>,
    completed: &Mutex<HashSet<String>>,
    id: String,
    outcome: Outcome,
) {
    completed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone());
    let _ = tx.send((id, outcome));
}

fn completed_ids(completed: &Mutex<HashSet<String>>) -> std::sync::MutexGuard<'_, HashSet<String>> {
    completed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一次 B 站批量状态请求，服务所有到期房间。
///
/// 批量接口只覆盖「有直播间的关注」：某个 UID 没返回时不能当离线，
/// 必须回落到单房间检查；整批失败时优先保住正在录制/上次在播的房间，
/// 其余房间只延后重试，绝不凭一次失败改状态。
#[allow(clippy::too_many_arguments)]
async fn run_batch_check(
    app: &AppHandle,
    db: &Db,
    broker: &StreamBroker,
    recorder: &Arc<Recorder>,
    rooms: Vec<hivecast_ipc::RoomRecord>,
    permits: Arc<Semaphore>,
    global: Arc<Semaphore>,
    tx: mpsc::UnboundedSender<(String, Outcome)>,
    completed: Arc<Mutex<HashSet<String>>>,
) {
    let started = Instant::now();
    let total = rooms.len();
    let uids: Vec<String> = rooms
        .iter()
        .filter_map(|room| room.platform_uid.clone())
        .collect();
    match crate::platform::bilibili::fetch_live_statuses(&uids).await {
        Ok(statuses) => {
            let returned = statuses.len();
            let live = statuses.iter().filter(|status| status.is_live).count();
            let mut missing = 0usize;
            let by_uid: HashMap<String, hivecast_ipc::LiveStatus> = statuses
                .into_iter()
                .map(|status| (status.uid.clone(), status))
                .collect();
            for room in rooms {
                let outcome = match room.platform_uid.as_ref().and_then(|uid| by_uid.get(uid)) {
                    Some(status) => {
                        let info = Arc::new(batch_room_info(status, &room));
                        check_room_info(app, db, broker, recorder, &room, info).await
                    }
                    // 批量没返回这个 UID：可能是没有直播间/接口字段缺失，
                    // 不能按离线处理，回落到单房间接口拿准确状态。
                    None => {
                        missing += 1;
                        tracing::debug!(
                            target: "hivecast_lib::monitor",
                            room = %room.host_name,
                            room_id = %room.id,
                            uid = room.platform_uid.as_deref().unwrap_or_default(),
                            "批量状态缺少该 UID，回退单房间检查"
                        );
                        run_check(
                            app,
                            db,
                            broker,
                            recorder,
                            &room,
                            permits.clone(),
                            global.clone(),
                        )
                        .await
                    }
                };
                send_batch_outcome(&tx, &completed, room.id, outcome);
            }
            tracing::info!(
                target: "hivecast_lib::monitor",
                rooms = total,
                uids = uids.len(),
                returned,
                live,
                missing,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "B 站批量状态检查完成"
            );
        }
        Err(err) => {
            let critical: Vec<_> = rooms
                .iter()
                .filter(|room| room.is_live || recorder.is_recording(&room.id))
                .cloned()
                .collect();
            tracing::warn!(
                error = %err,
                rooms = rooms.len(),
                critical = critical.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "B 站批量状态失败，降级单房间检查"
            );
            let critical_ids: HashSet<String> =
                critical.iter().map(|room| room.id.clone()).collect();
            for room in critical {
                let outcome = run_check(
                    app,
                    db,
                    broker,
                    recorder,
                    &room,
                    permits.clone(),
                    global.clone(),
                )
                .await;
                send_batch_outcome(&tx, &completed, room.id, outcome);
            }
            for room in rooms {
                if !critical_ids.contains(&room.id) {
                    // 不更新状态，也不退避成小时级；下个批量拍继续尝试。
                    send_batch_outcome(
                        &tx,
                        &completed,
                        room.id,
                        Outcome::Retry(base_batch_retry()),
                    );
                }
            }
        }
    }
}

/// 批量失败后的普通房间延后时间：给接口恢复留一点时间，又不至于拖过一个检测周期。
fn base_batch_retry() -> i64 {
    10
}

/// 把批量状态映射成监控统一使用的房间信息。
///
/// 批量状态里的 live_time 是 Unix 秒；优先用平台真实开播时间，缺失时才沿用
/// 本地上一轮的值。这样快速下播重开能识别成新一场，不会沿用旧开播时间。
fn batch_room_info(
    status: &hivecast_ipc::LiveStatus,
    room: &hivecast_ipc::RoomRecord,
) -> crate::platform::RoomInfo {
    let text = |value: &str, fallback: &str| {
        if value.trim().is_empty() {
            fallback.to_string()
        } else {
            value.to_string()
        }
    };
    crate::platform::RoomInfo {
        host_name: text(&status.uname, &room.host_name),
        room_name: text(&status.title, &room.title),
        avatar_url: status
            .avatar_url
            .clone()
            .or_else(|| room.avatar_url.clone()),
        cover_url: status.cover_url.clone().or_else(|| room.cover_url.clone()),
        area_name: status.area_name.clone().or_else(|| room.area_name.clone()),
        is_live: status.is_live,
        started_at: status
            .started_at
            .or_else(|| if room.is_live { room.started_at } else { None }),
        online: status.online,
        stable_id: status
            .room_id
            .clone()
            .or_else(|| Some(room.room_key.clone())),
        extra: serde_json::json!({
            "uid": status.uid,
            "roomId": status.room_id,
            "shortId": status.short_id,
            "liveStatus": if status.is_live { 1 } else { 0 },
        }),
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

/// 平台报离线时到底停不停录（R2）。
///
/// 两次成功检查都离线只是「平台这么说」，分片仍在写说明源还活着；
/// 只有当确认离线且文件确实不再增长时才收尾。
pub(crate) fn should_stop_for_offline(confirmed: bool, flowing: bool) -> bool {
    confirmed && !flowing
}

/// 单房间自动录制规则：标题包含/排除关键词。
pub(crate) fn auto_record_allowed(room: &hivecast_ipc::RoomRecord, room_name: &str) -> bool {
    let title = room_name.to_lowercase();
    let keywords = |value: &str| {
        value
            .split([',', '，'])
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    if let Some(include) = room
        .title_include
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let words = keywords(include);
        if !words.is_empty() && !words.iter().any(|word| title.contains(word)) {
            return false;
        }
    }
    if let Some(exclude) = room
        .title_exclude
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let words = keywords(exclude);
        if words.iter().any(|word| title.contains(word)) {
            return false;
        }
    }
    true
}

/// 这一路录制实际在往哪个目录写（R5）。
///
/// 正常都是录像文件所在目录；只有取不到父目录这种异常路径才退回当前媒体根。
fn target_dir(file: &std::path::Path, fallback: &std::path::Path) -> std::path::PathBuf {
    file.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| fallback.to_path_buf())
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
        Ok(info) => check_room_info(app, db, broker, recorder, room, info).await,
        Err(err) => check_room_error(app, db, recorder, room, err).await,
    }
}

/// 已经拿到平台房间信息后的统一收尾：写库、自动开录、离线确认。
///
/// 单房间轮询和 B 站批量状态都走这里，避免两条路径对「在播/离线」的判断分叉。
async fn check_room_info(
    app: &AppHandle,
    db: &Db,
    broker: &StreamBroker,
    recorder: &Arc<Recorder>,
    room: &hivecast_ipc::RoomRecord,
    info: Arc<crate::platform::RoomInfo>,
) -> Outcome {
    // 这一拍查通了：之前那条检测错误已经过时，清掉再推一次事件，
    // 否则监控页会一直挂着「房间不存在」之类的旧提示（F1）。
    if recorder.clear_detect_error(&room.id) {
        recorder.clear_error(&room.id);
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    }
    // 只有结构性字段变化才整行写库并推前端（G2）。
    // 在线人数这类数字每次检测都在跳，以前会让每个房间每秒一次整行 UPDATE，
    // WAL 被持续推大；现在它们 10 秒才落一次，界面最多滞后 10 秒。
    let structural = info.is_live != room.is_live
        || info.room_name != room.title
        || info.host_name != room.host_name
        || info.avatar_url != room.avatar_url
        || info.cover_url != room.cover_url
        || info.area_name != room.area_name
        || info.started_at != room.started_at;
    if structural {
        // 在播/离线翻转是用户最关心的状态变化，留一条 INFO 方便回溯。
        if info.is_live != room.is_live {
            tracing::info!(
                room = %room.host_name,
                room_id = %room.id,
                live = info.is_live,
                started_at = info.started_at,
                "房间开播状态变化"
            );
        }
        if let Err(err) = db.update_room_info(&room.id, &info) {
            tracing::warn!(%err, "更新房间信息失败");
        }
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    } else {
        // 在线人数这类高频字段走节流写库；但写进去了就得推事件，
        // 否则监控页的数字会一直冻在打开那一刻（H1）。
        match db.touch_room_throttled(&room.id, info.online, ROOM_TOUCH_INTERVAL) {
            Ok(changed) => {
                if changed && room.online != info.online {
                    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                }
            }
            Err(err) => tracing::debug!(%err, "节流更新房间信息失败"),
        }
    }

    if info.is_live {
        recorder.clear_offline(&room.id);
        // 上一拍确认过离线、这一拍又报在播：是新一场，解掉上一场的
        // 结束静默与手动停录抑制。没有开播时间的平台全靠这个信号（H7）。
        if recorder.new_session_after_offline(&room.id) {
            tracing::info!(room = %room.host_name, "离线后又报在播，按新一场处理");
        }
        // 手动停录过的房间在本次直播内不再自动拉起，等主播下播后复位；
        // 上一场刚自然结束、平台还显示在播时也先等一等（S2/S3）。
        if room.auto_record
            && !room.monitor_paused
            && auto_record_allowed(&room, &info.room_name)
            && !recorder.is_recording(&room.id)
            && !recorder.auto_suppressed(&room.id, info.started_at)
            && !recorder.auto_blocked(&room.id, info.started_at)
            && !recorder.in_failure_cooldown(&room.id)
            && !recorder.in_start_cooldown(&room.id)
            && !recorder.is_finalizing(&room.id)
        {
            let media_root = recorder.media_root();
            if !disk::can_start(&media_root) {
                // 磁盘不够时以前是「静默不录」，用户只看到一直没开始。
                // 现在把原因写进房间行内错误（同一个原因不重复刷）。
                let message = disk::start_blocked_message(&media_root);
                if recorder.last_error(&room.id).as_deref() != Some(message.as_str()) {
                    // 掉盘和真的没空间是两回事，日志前缀分开写。
                    let reason = if disk::status(&media_root) == disk::DiskStatus::Missing {
                        "录制目录不可用，暂不自动开录"
                    } else {
                        "空间不足，暂不自动开录"
                    };
                    tracing::warn!(room = %room.host_name, %message, "{reason}");
                    recorder.set_error(&room.id, message);
                    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                }
            } else if let Err(err) = recorder.start_checked(app, broker, room).await {
                // 并发已满会每个检测周期失败一次：只在原因变化时告警 + 推送，
                // 否则间隔 10 秒就是每 10 秒一条 WARN（C7）。
                let message = err.to_string();
                let changed = recorder.last_error(&room.id).as_deref() != Some(message.as_str());
                if changed {
                    tracing::warn!(room = %room.host_name, %err, "自动开录失败");
                    recorder.set_error(&room.id, message);
                    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                } else {
                    tracing::debug!(room = %room.host_name, %err, "自动开录仍失败");
                }
            }
        }
    } else {
        // 一次离线可能只是接口抖动 / 字段缺失，连续两次才认（S1）。
        let confirmed = recorder.confirm_offline(&room.id);
        if !confirmed {
            // 还没攒够两次：什么都别改。尤其是不能 resume_auto ——
            // 单次假离线会清掉「用户手动停录」的抑制，同一场直播里
            // 刚停的录就会被下一拍自动拉起（B2）。
            return Outcome::Ok;
        }
        // 确认下播才复位自动开录：下一场直播可以重新自动录。
        // 同时留个「见过离线」的记号，给下一次在播判断新一场用（H7）。
        recorder.note_offline_seen(&room.id);
        recorder.resume_auto(&room.id);
        if recorder.is_recording(&room.id) {
            // 确认了吗，还要看文件是不是仍在增长（R2）。
            if should_stop_for_offline(true, recorder.flowing(&room.id)) {
                // 告诉录制侧「这次是平台确认离线」，它才不会把收尾
                // 当成误停、也不会挡住紧接着的下一场开播（A8）。
                recorder.note_offline_stop(&room.id);
                let _ = recorder.stop(app, &room.id, "stream_ended").await;
            } else {
                // 平台说下播、文件却还在写：更像是状态接口误报。
                // 计数已清零，下一轮重新确认，不切断健康录像。
                tracing::warn!(
                    room = %room.host_name,
                    "平台报离线，但录像仍在增长，暂不停录"
                );
            }
        }
    }
    Outcome::Ok
}

/// 平台请求失败：只更新提示与排期分类，不猜测开播状态。
async fn check_room_error(
    app: &AppHandle,
    db: &Db,
    recorder: &Arc<Recorder>,
    room: &hivecast_ipc::RoomRecord,
    err: PlatformError,
) -> Outcome {
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
    // 同一句失败只告警、只推一次事件：坏房间会被退避到几分钟一次，
    // 但两次失败之间间隔可能很久，期间不该反复让前端全量重拉（F1）。
    if recorder.note_detect_error(&room.id, &hint) {
        tracing::warn!(room = %room.host_name, %hint, "刷新房间失败");
        recorder.set_error(&room.id, hint);
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    } else {
        tracing::debug!(room = %room.host_name, %hint, "刷新房间仍失败");
    }
    if err.is_throttle() {
        Outcome::Throttled
    } else {
        Outcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch_room() -> hivecast_ipc::RoomRecord {
        hivecast_ipc::RoomRecord {
            id: "room-1".into(),
            platform: "bilibili".into(),
            room_key: "old-key".into(),
            canonical_url: "https://live.bilibili.com/old-key".into(),
            title: "旧标题".into(),
            host_name: "旧主播".into(),
            avatar_url: None,
            cover_url: Some("https://old/cover.jpg".into()),
            area_name: Some("旧分区".into()),
            is_live: true,
            started_at: Some(100),
            online: Some(1),
            auto_record: true,
            monitor_paused: false,
            record_qn: 0,
            view_qn: 0,
            record_line: None,
            last_checked_at: None,
            last_session_id: None,
            last_session_started_at: Some(0),
            last_session_duration_ms: 0,
            last_session_status: None,
            last_session_reason: None,
            last_session_file_size: 0,
            last_session_quality: None,
            note: String::new(),
            tags: String::new(),
            title_include: None,
            title_exclude: None,
            split_max_duration_sec: None,
            split_max_size_mb: None,
            notify_live: None,
            notify_error: None,
            created_at: 0,
            updated_at: 0,
            platform_uid: Some("42".into()),
        }
    }

    #[test]
    fn batch_status_keeps_local_fallbacks_and_only_reuses_a_known_start_time() {
        let room = batch_room();
        let status = hivecast_ipc::LiveStatus {
            uid: "42".into(),
            room_id: Some("7734200".into()),
            short_id: Some("6".into()),
            uname: String::new(),
            title: String::new(),
            avatar_url: None,
            cover_url: Some("https://new/cover.jpg".into()),
            area_name: None,
            is_live: true,
            online: Some(9),
            started_at: Some(200),
        };
        let info = batch_room_info(&status, &room);
        assert_eq!(info.host_name, "旧主播");
        assert_eq!(info.room_name, "旧标题");
        assert_eq!(info.cover_url.as_deref(), Some("https://new/cover.jpg"));
        assert_eq!(info.area_name.as_deref(), Some("旧分区"));
        assert_eq!(info.stable_id.as_deref(), Some("7734200"));
        assert_eq!(
            info.started_at,
            Some(200),
            "批量状态带回新的开播时间时必须覆盖本地旧值"
        );

        let no_time = hivecast_ipc::LiveStatus {
            started_at: None,
            ..status.clone()
        };
        let info = batch_room_info(&no_time, &room);
        assert_eq!(
            info.started_at,
            Some(100),
            "批量没有给出开播时间时沿用本地上一轮"
        );

        let offline_room = hivecast_ipc::RoomRecord {
            is_live: false,
            started_at: None,
            ..batch_room()
        };
        let offline = hivecast_ipc::LiveStatus {
            is_live: false,
            online: None,
            cover_url: None,
            started_at: None,
            ..status
        };
        let info = batch_room_info(&offline, &offline_room);
        assert_eq!(info.started_at, None, "离线房间不能沿用上一场的开播时间");
        assert_eq!(info.cover_url.as_deref(), Some("https://old/cover.jpg"));
        assert_eq!(info.online, None);
    }

    fn entry(offset_secs: u64) -> Schedule {
        Schedule {
            grid_at: Instant::now() + Duration::from_secs(offset_secs),
            next_at: Instant::now() + Duration::from_secs(offset_secs),
            ..Default::default()
        }
    }

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn long_interval_survives_positive_jitter() {
        // 成功排期带 +10% 抖动：next_at 比 now+interval 还远，但那不是「间隔被调小」，
        // 不能把 1800 秒的排期重置成 30 秒内（G7）。
        let now = Instant::now();
        let base = 1800;
        let entry = Schedule {
            grid_at: now + Duration::from_secs(1800),
            next_at: now + Duration::from_secs(1810),
            interval_secs: base,
            ..Default::default()
        };
        assert!(!needs_reschedule(&entry, base, now));
        // 同样状态把间隔调小到 30 秒：应当重排。
        assert!(needs_reschedule(&entry, 30, now));
        // 退避中的排期同样要跟着间隔收缩：退避按旧间隔算出的等待可能长达小时级，
        // 不重排等于「调小间隔对坏房间不生效」（H8）。
        let backing_off = Schedule {
            backoff: true,
            ..entry
        };
        assert!(needs_reschedule(&backing_off, 30, now));
        // 但间隔没变小的时候，退避照旧。
        assert!(!needs_reschedule(&backing_off, base, now));
    }

    #[test]
    fn spread_stays_inside_one_interval() {
        for id in ["a", "b", "c", "rm-b6", "rm-dy9999"] {
            let offset = spread(id, 30);
            assert!((0..30).contains(&offset), "{id} -> {offset}");
        }
        assert_eq!(spread("a", 0), 0);
        assert_eq!(spread("a", 1), 0);
        // 间隔很大时首个排期也只往后拖 30 秒，重启后不用干等半小时（G1）。
        for id in ["a", "b", "c", "rm-b6", "rm-dy9999"] {
            let offset = spread(id, 1800);
            assert!((0..30).contains(&offset), "{id} -> {offset}");
        }
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
    fn jitter_is_deterministic_and_room_specific() {
        // 同一房间重复算多次必须是同一个值：排期可复现（C6）。
        let first = jitter("rm-audit-1", 3);
        for _ in 0..50 {
            assert_eq!(jitter("rm-audit-1", 3), first);
        }
        // 不同房间应该落在不同偏移上（至少不是全部相同）。
        let offsets: std::collections::HashSet<i64> = ["r1", "r2", "r3", "r4"]
            .iter()
            .map(|id| jitter(id, 10))
            .collect();
        assert!(offsets.len() > 1, "所有房间抖动相同，散不开：{offsets:?}");
    }

    #[test]
    fn jitter_does_not_accumulate_across_reschedules() {
        // 固定网格 + 抖动：20 次正常轮询后，网格点应正好前进 20 个间隔，
        // 实际触发时刻只比网格多一个抖动偏移（旧实现每次都叠加，会漂移，C6）。
        let mut entry = entry(0);
        let start = entry.grid_at;
        let base = secs(30);
        for _ in 0..20 {
            apply("rm-drift", &mut entry, Outcome::Ok, base, secs(60), true);
            // apply 用 now 做基准、且会把过去的时间钳到 now+1s，这里只验证
            // 网格增量严格等于间隔，不受时钟影响。
        }
        let advance = entry.grid_at.duration_since(start);
        assert_eq!(advance, base * 20);
        let offset = if entry.next_at >= entry.grid_at {
            entry.next_at.duration_since(entry.grid_at)
        } else {
            entry.grid_at.duration_since(entry.next_at)
        };
        assert!(offset <= secs(3), "抖动偏移过大或串到了网格上");
    }

    #[test]
    fn grid_next_keeps_cadence_and_avoids_burst() {
        let start = Instant::now();
        // 准时完成：在上一拍上加一个间隔。
        assert_eq!(grid_next(start, start, secs(30)), start + secs(30));
        // 落后两拍（例如休眠）：从现在重算，不补发。
        assert_eq!(
            grid_next(start, start + secs(100), secs(30)),
            start + secs(130)
        );
        // 系统时间被改（墙钟跳变）不影响：这里只用单调时钟。
        assert_eq!(
            grid_next(start, start + secs(1), secs(30)),
            start + secs(30)
        );
    }

    #[test]
    fn backoff_grows_caps_and_respects_switch() {
        assert_eq!(backoff_wait(secs(30), 1, secs(120), true), secs(60));
        assert_eq!(backoff_wait(secs(30), 2, secs(120), true), secs(120));
        assert_eq!(backoff_wait(secs(30), 9, secs(120), true), secs(120));
        assert_eq!(backoff_wait(secs(30), 3, secs(120), false), secs(30));
        // cap 小于基础间隔时不该把间隔压得比基础还短。
        assert_eq!(backoff_wait(secs(30), 2, secs(10), true), secs(30));
    }

    #[test]
    fn failed_outcome_backs_off() {
        // 房间本身出错（不存在、解析失败）也要退避：不能按基础间隔每拍硬打平台（F6）。
        let base = secs(30);
        let cap = secs(120);
        let mut s = entry(0);
        for (round, expected) in [(1u32, 60u64), (2, 120), (3, 120)] {
            let before = Instant::now();
            apply("rm-f6", &mut s, Outcome::Failed, base, cap, true);
            let wait = s.next_at.saturating_duration_since(before);
            // 抖动是 ±BACKOFF_JITTER_PCT，放宽一点再比。
            let low = secs(expected).mul_f64(0.85);
            let high = secs(expected).mul_f64(1.2);
            assert!(
                wait >= low && wait <= high,
                "第 {round} 次失败排到 {wait:?}，期望 {low:?}~{high:?}"
            );
            assert_eq!(s.error_count, round);
            assert!(s.backoff, "退避期间应标记为退避");
        }
        // 关掉退避就回到基础间隔。
        let before = Instant::now();
        apply("rm-f6", &mut s, Outcome::Failed, base, cap, false);
        let wait = s.next_at.saturating_duration_since(before);
        assert!(wait >= secs(25) && wait <= secs(35), "关闭退避后 {wait:?}");
    }

    #[test]
    fn throttle_only_counts_real_throttles() {
        let mut s = entry(100);
        apply(
            "rm-b6",
            &mut s,
            Outcome::Throttled,
            secs(30),
            secs(120),
            true,
        );
        assert_eq!(s.throttle_streak, 1);
        apply(
            "rm-b6",
            &mut s,
            Outcome::Throttled,
            secs(30),
            secs(120),
            true,
        );
        assert_eq!(s.throttle_streak, 2);
        // 普通错误不累加限流计数，也不放大退避。
        apply("rm-b6", &mut s, Outcome::Failed, secs(30), secs(120), true);
        assert_eq!(s.throttle_streak, 2);
        assert_eq!(s.error_count, 3);
        apply("rm-b6", &mut s, Outcome::Ok, secs(30), secs(120), true);
        assert_eq!(s.throttle_streak, 0);
        assert_eq!(s.error_count, 0);
    }

    #[test]
    fn broker_cooldown_does_not_count_as_failure() {
        let mut s = entry(100);
        apply(
            "rm-b6",
            &mut s,
            Outcome::Retry(5),
            secs(30),
            secs(120),
            true,
        );
        assert_eq!(s.throttle_streak, 0);
        assert_eq!(s.error_count, 0);
        assert!(s.next_at >= Instant::now() + secs(4));
    }

    #[test]
    fn offline_stops_only_when_the_file_stopped_growing() {
        // 确认了离线、文件也停了 → 收尾。
        assert!(should_stop_for_offline(true, false));
        // 确认了离线但分片仍在写 → 更像接口误报，继续录（R2）。
        assert!(!should_stop_for_offline(true, true));
        assert!(!should_stop_for_offline(false, false));
        assert!(!should_stop_for_offline(false, true));
    }

    #[test]
    fn target_dir_prefers_the_recording_location() {
        use std::path::Path;
        // 换过媒体目录后，旧路径下的录制仍然按旧目录检查磁盘（R5）。
        assert_eq!(
            target_dir(Path::new("/old/dir/a.mp4"), Path::new("/new")),
            Path::new("/old/dir")
        );
        assert_eq!(
            target_dir(Path::new("a.mp4"), Path::new("/new")),
            Path::new("/new")
        );
    }

    #[test]
    fn jitter_never_moves_far_or_backwards_in_time() {
        let now = Instant::now();
        for _ in 0..100 {
            let at = add_jitter(now, "rm-b6", secs(3));
            assert!(at >= now - secs(3) && at <= now + secs(3));
        }
        assert_eq!(add_jitter(now, "rm-b6", secs(0)), now);
    }

    #[test]
    fn in_flight_rooms_are_not_scheduled_twice() {
        let mut s = entry(0);
        s.inflight = true;
        assert!(s.inflight);
        apply("rm-b6", &mut s, Outcome::Ok, secs(30), secs(120), true);
        assert!(!s.inflight);
    }
}
