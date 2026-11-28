//! 观看端会话（M2）：每个槽位一条独立的「取流 → 转封装 → 本地 HTTP」管道。
//!
//! 关键约束（docs/19 §3.1）：
//! - 槽位之间不共享任何状态：关掉 A 的声音、切 B 的画质都不影响别人；
//! - 观看与录制是两条独立租约：本模块从不触碰 recorder；
//! - 每次（重）启动都用新的 session 目录，前端拿新 URL 重挂 hls，旧目录立刻删除。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hivecast_ipc::{RoomRecord, ViewSlotInfo};
use tauri::{AppHandle, Emitter};
use tokio::process::Child;

use crate::broker::StreamBroker;
use crate::danmaku::DanmakuHub;
use crate::error::AppError;
use crate::ffmpeg;
use crate::platform::{Codec, RoomRef, StreamCandidate, StreamFormat, StreamGroup};
use crate::process::{send_graceful, wait_exit};
use crate::stream::StreamServer;

pub const EVENT_VIEW_CHANGED: &str = "view://changed";
pub const MAX_SLOTS: u8 = 4;

/// 轮询子进程退出的间隔。
const POLL: Duration = Duration::from_millis(500);
/// 从拉起 ffmpeg 到出现第一个分片的上限。
const READY_TIMEOUT: Duration = Duration::from_secs(12);
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(8);
const MAX_RETRIES: u32 = 10;
/// 停止时等待 ffmpeg flush 的上限。
const STOP_TIMEOUT: Duration = Duration::from_secs(4);

struct SlotState {
    status: String,
    session_id: Option<String>,
    dir: Option<PathBuf>,
    child: Option<Child>,
    url: Option<String>,
    requested_qn: u32,
    actual_qn: u32,
    quality: Option<String>,
    line: Option<String>,
    /// 用户手选的线路；None 表示自动挑
    preferred_line: Option<String>,
    error: Option<String>,
    /// 重连倒计时的目标时刻（epoch 毫秒）；不在等待重连时为 None。
    retry_at: Option<i64>,
    /// 每次切画质/重试自增；监督循环据此判断该重启了。
    generation: u64,
    closed: bool,
    /// 监督循环是否还在跑；retry 用它判断要不要另起一个。
    alive: bool,
}

impl SlotState {
    fn new(qn: u32) -> Self {
        Self {
            status: "starting".into(),
            session_id: None,
            dir: None,
            child: None,
            url: None,
            requested_qn: qn,
            actual_qn: 0,
            quality: None,
            line: None,
            preferred_line: None,
            error: None,
            retry_at: None,
            generation: 0,
            closed: false,
            alive: false,
        }
    }

    fn info(&self, slot: u8, room_id: &str) -> ViewSlotInfo {
        ViewSlotInfo {
            slot,
            room_id: room_id.to_string(),
            status: self.status.clone(),
            url: self.url.clone(),
            requested_qn: self.requested_qn,
            actual_qn: self.actual_qn,
            quality: self.quality.clone(),
            line: self.line.clone(),
            preferred_line: self.preferred_line.clone(),
            error: self.error.clone(),
            retry_at: self.retry_at,
        }
    }
}

struct Slot {
    room: RoomRecord,
    state: Mutex<SlotState>,
}

impl Slot {
    fn new(room: RoomRecord) -> Self {
        Self {
            state: Mutex::new(SlotState::new(room.view_qn)),
            room,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().expect("观看状态被污染")
    }

    fn info(&self, slot: u8) -> ViewSlotInfo {
        self.lock().info(slot, &self.room.id)
    }
}

/// 一条管道走完之后的结果，决定监督循环的下一步。
/// 槽位放弃重连时播报一次：前端据此弹系统通知（观看失败）。
fn notify_failure(app: &AppHandle, room: &RoomRecord, detail: &str) {
    let notice = hivecast_ipc::AppNotice {
        kind: "view_failed".into(),
        room_id: room.id.clone(),
        host_name: room.host_name.clone(),
        detail: Some(detail.to_string()),
        suppress: false,
    };
    tracing::warn!(room_id = %room.id, room = %room.host_name, detail, "观看失败，通知前端");
    if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
        tracing::warn!(room_id = %room.id, error = %err, "观看失败通知发送失败");
    }
}

enum Cycle {
    /// 用户关掉了槽位
    Closed,
    /// 画质/线路变了，立刻用新参数重启
    Restart,
    /// ffmpeg 自己退出了，需要重连
    Exited,
}

pub struct ViewManager {
    root: PathBuf,
    server: Arc<StreamServer>,
    slots: Mutex<HashMap<u8, Arc<Slot>>>,
    /// 串行化 open：StrictMode 下同一个 effect 会跑两遍，并发 open 会占掉两个格子。
    open_gate: tokio::sync::Mutex<()>,
    /// 弹幕扇出：槽位存在期间持有引用，关掉最后一个消费者才断 WS。
    danmaku: Arc<DanmakuHub>,
}

impl ViewManager {
    pub fn new(root: PathBuf, server: Arc<StreamServer>, danmaku: Arc<DanmakuHub>) -> Arc<Self> {
        Arc::new(Self {
            root,
            server,
            slots: Mutex::new(HashMap::new()),
            open_gate: tokio::sync::Mutex::new(()),
            danmaku,
        })
    }

    pub fn snapshot(&self) -> Vec<ViewSlotInfo> {
        let slots = self.slots.lock().expect("观看槽位被污染");
        let mut list: Vec<(u8, ViewSlotInfo)> = slots
            .iter()
            .map(|(slot, handle)| (*slot, handle.info(*slot)))
            .collect();
        list.sort_by_key(|(slot, _)| *slot);
        list.into_iter().map(|(_, info)| info).collect()
    }

    /// 打开一个房间到空闲槽位；同一房间已开时只返回现有槽位，不重复建会话。
    pub async fn open(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        room: &RoomRecord,
    ) -> Result<ViewSlotInfo, AppError> {
        let _gate = self.open_gate.lock().await;
        if let Some((slot, handle)) = self.find_by_room(&room.id) {
            if handle.lock().alive {
                tracing::debug!(room_id = %room.id, slot, "观看已在运行，复用现有槽位");
                return Ok(handle.info(slot));
            }
            tracing::info!(room_id = %room.id, slot, "观看槽位已退出，重新拉起监督循环");
            // 上次失败的槽位：重新拉起，而不是再占一个格子。
            self.restart_supervisor(app, broker, slot, handle.clone(), room.clone());
            return Ok(handle.info(slot));
        }

        let (slot, handle) = {
            let mut slots = self.slots.lock().expect("观看槽位被污染");
            let slot = (0..MAX_SLOTS)
                .find(|index| !slots.contains_key(index))
                .ok_or_else(|| {
                    tracing::warn!(room_id = %room.id, "观看被拒绝：槽位已满");
                    AppError::User("观看槽位已满（最多同时看 4 路）".into())
                })?;
            let handle = Arc::new(Slot::new(room.clone()));
            slots.insert(slot, handle.clone());
            (slot, handle)
        };

        // 先占住弹幕引用再起管道：这样弹幕能覆盖到第一帧，而不是等 ffmpeg 就绪。
        self.danmaku.retain(room);
        self.emit(app);
        tracing::info!(room_id = %room.id, room = %room.host_name, slot, "打开观看");
        self.restart_supervisor(app, broker, slot, handle.clone(), room.clone());
        Ok(handle.info(slot))
    }

    /// 原子替换某个槽位的房间：停旧起新只广播一次，不出现「关掉再打开」的中间态。
    pub async fn replace(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        slot: u8,
        room: &RoomRecord,
    ) -> Result<ViewSlotInfo, AppError> {
        let _gate = self.open_gate.lock().await;
        // 同一房间已经在别的槽位：先摘掉旧的，避免一个房间起两条管道。
        if let Some((other, handle)) = self.find_by_room(&room.id) {
            if other != slot {
                self.detach(app, other, &handle);
            }
        }
        let old = {
            let mut slots = self.slots.lock().expect("观看槽位被污染");
            slots.remove(&slot)
        };
        if let Some(old) = old {
            if old.room.id == room.id {
                self.slots
                    .lock()
                    .expect("观看槽位被污染")
                    .insert(slot, old.clone());
                tracing::debug!(room_id = %room.id, slot, "观看替换目标与当前一致，保持原槽位");
                return Ok(old.info(slot));
            }
            tracing::info!(room_id = %old.room.id, slot, "观看替换：摘掉旧房间");
            self.detach(app, slot, &old);
        }

        let handle = Arc::new(Slot::new(room.clone()));
        self.slots
            .lock()
            .expect("观看槽位被污染")
            .insert(slot, handle.clone());
        self.danmaku.retain(room);
        self.emit(app);
        tracing::info!(room_id = %room.id, room = %room.host_name, slot, "替换观看槽位");
        self.restart_supervisor(app, broker, slot, handle.clone(), room.clone());
        Ok(handle.info(slot))
    }

    /// 停掉一个槽位：从表里摘除、释放弹幕、通知监督循环退出，收尾放到后台。
    fn detach(self: &Arc<Self>, _app: &AppHandle, slot: u8, handle: &Arc<Slot>) {
        tracing::debug!(room_id = %handle.room.id, slot, "摘除观看槽位并排后台收尾");
        self.slots.lock().expect("观看槽位被污染").remove(&slot);
        self.danmaku.release(&handle.room.id);
        {
            let mut state = handle.lock();
            state.closed = true;
            state.generation += 1;
            if let Some(child) = state.child.as_mut() {
                send_graceful(child);
            }
        }
        let manager = self.clone();
        let handle = handle.clone();
        tokio::spawn(async move {
            manager.teardown(&handle).await;
        });
    }

    pub async fn close(&self, app: &AppHandle, slot: u8) -> Result<(), AppError> {
        let handle = {
            let mut slots = self.slots.lock().expect("观看槽位被污染");
            slots.remove(&slot)
        };
        let Some(handle) = handle else {
            tracing::debug!(slot, "关闭观看：槽位本来就不存在");
            return Ok(());
        };
        tracing::info!(room_id = %handle.room.id, slot, "关闭观看槽位");
        self.danmaku.release(&handle.room.id);

        {
            let mut state = handle.lock();
            state.closed = true;
            state.generation += 1;
            if let Some(child) = state.child.as_mut() {
                send_graceful(child);
            }
        }
        self.emit(app);

        // 等监督循环把子进程收掉，最多 3 秒；收不掉也没关系，后台会继续清理。
        for _ in 0..30 {
            if handle.lock().child.is_none() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::debug!(slot, "关闭观看：3 秒内子进程尚未收尾，交给后台继续清理");
        Ok(())
    }

    /// 切换某槽位的画质。只影响这个槽位的租约，录制侧完全不受影响。
    pub fn set_quality(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        slot: u8,
        qn: u32,
    ) -> Result<ViewSlotInfo, AppError> {
        let handle = self.handle(slot)?;
        let needs_spawn = {
            let mut state = handle.lock();
            if state.requested_qn == qn {
                // 监督循环已经退出时允许用同一个档位重新拉起，不再静默返回错误态。
                if state.alive || state.closed {
                    return Ok(state.info(slot, &handle.room.id));
                }
                state.generation += 1;
                state.status = "starting".into();
                state.error = None;
                true
            } else {
                tracing::info!(slot, room_id = %handle.room.id, from = state.requested_qn, to = qn, "切换观看画质");
                state.requested_qn = qn;
                state.generation += 1;
                state.status = "starting".into();
                state.error = None;
                !state.alive
            }
        };
        self.emit(app);
        if needs_spawn {
            self.restart_supervisor(app, broker, slot, handle.clone(), handle.room.clone());
        }
        Ok(handle.info(slot))
    }

    /// 切换某槽位使用的线路。None 表示回到自动挑线路。
    pub fn set_line(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        slot: u8,
        line: Option<String>,
    ) -> Result<ViewSlotInfo, AppError> {
        let handle = self.handle(slot)?;
        let needs_spawn = {
            let mut state = handle.lock();
            if state.preferred_line == line {
                return Ok(state.info(slot, &handle.room.id));
            }
            tracing::info!(slot, room_id = %handle.room.id, line = ?line, "切换观看线路");
            state.preferred_line = line;
            state.generation += 1;
            state.status = "starting".into();
            state.error = None;
            !state.alive
        };
        self.emit(app);
        if needs_spawn {
            self.restart_supervisor(app, broker, slot, handle.clone(), handle.room.clone());
        }
        Ok(handle.info(slot))
    }

    /// 失败后手动重试：监督循环已经退出就重新拉起，否则只让它按新参数重启。
    pub async fn retry(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        slot: u8,
    ) -> Result<ViewSlotInfo, AppError> {
        let handle = self.handle(slot)?;
        tracing::info!(slot, room_id = %handle.room.id, "手动重试观看");
        let needs_spawn = {
            let mut state = handle.lock();
            state.generation += 1;
            state.error = None;
            state.status = "starting".into();
            !state.alive
        };
        self.emit(app);
        if needs_spawn {
            self.restart_supervisor(app, broker, slot, handle.clone(), handle.room.clone());
        }
        Ok(handle.info(slot))
    }

    /// 房间被删除时，把占用它的槽位一起关掉。
    pub async fn close_room(&self, app: &AppHandle, room_id: &str) {
        let targets: Vec<u8> = {
            let slots = self.slots.lock().expect("观看槽位被污染");
            slots
                .iter()
                .filter(|(_, handle)| handle.room.id == room_id)
                .map(|(slot, _)| *slot)
                .collect()
        };
        if !targets.is_empty() {
            tracing::info!(room_id, slots = targets.len(), "房间已删除，关闭其观看槽位");
        }
        for slot in targets {
            let _ = self.close(app, slot).await;
        }
    }

    /// 切走观看页：停掉所有转封装与分片缓存，但不动房间与布局（前端持久化）。
    /// 与 open/replace 共用 open_gate，快速来回切 Tab 也不会把新旧会话搅在一起。
    pub async fn suspend_all(self: &Arc<Self>, app: &AppHandle) {
        let _gate = self.open_gate.lock().await;
        let handles: Vec<Arc<Slot>> = {
            let mut slots = self.slots.lock().expect("观看槽位被污染");
            slots.drain().map(|(_, handle)| handle).collect()
        };
        if handles.is_empty() {
            tracing::debug!("离开观看页：没有需要收尾的槽位");
            return;
        }
        tracing::info!(count = handles.len(), "离开观看页，收尾全部槽位");
        for handle in &handles {
            self.danmaku.release(&handle.room.id);
            let mut state = handle.lock();
            state.closed = true;
            state.generation += 1;
            if let Some(child) = state.child.as_mut() {
                send_graceful(child);
            }
        }
        self.emit(app);
        // 四路并行收尾：每路最多等 4 秒，串行会让「离开观看页」卡十几秒。
        futures_util::future::join_all(handles.iter().map(|handle| self.teardown(handle))).await;
    }

    /// 退出应用：收掉所有槽位的 ffmpeg 与缓存目录。
    pub async fn shutdown_all(&self, app: &AppHandle) {
        let handles: Vec<Arc<Slot>> = {
            let mut slots = self.slots.lock().expect("观看槽位被污染");
            slots.drain().map(|(_, handle)| handle).collect()
        };
        if !handles.is_empty() {
            tracing::info!(count = handles.len(), "退出前收尾全部观看槽位");
        }

        for handle in &handles {
            self.danmaku.release(&handle.room.id);
            let mut state = handle.lock();
            state.closed = true;
            state.generation += 1;
            if let Some(child) = state.child.as_mut() {
                send_graceful(child);
            }
        }
        for handle in handles {
            self.teardown(&handle).await;
        }
        self.emit(app);
    }

    /// 启动时清掉上次异常退出留下的分片目录。
    pub async fn clean_cache(&self) {
        let root = self.root.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            std::fs::create_dir_all(&root)?;
            crate::session_cache::clean(&root)
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(dir = %self.root.display(), ?result, "清理观看缓存失败");
        }
    }

    fn handle(&self, slot: u8) -> Result<Arc<Slot>, AppError> {
        self.slots
            .lock()
            .expect("观看槽位被污染")
            .get(&slot)
            .cloned()
            .ok_or_else(|| AppError::User(format!("槽位 {slot} 没有在观看")))
    }

    fn find_by_room(&self, room_id: &str) -> Option<(u8, Arc<Slot>)> {
        let slots = self.slots.lock().expect("观看槽位被污染");
        slots
            .iter()
            .find(|(_, handle)| handle.room.id == room_id)
            .map(|(slot, handle)| (*slot, handle.clone()))
    }

    fn emit(&self, app: &AppHandle) {
        if let Err(err) = app.emit(EVENT_VIEW_CHANGED, self.snapshot()) {
            tracing::debug!(error = %err, "广播观看状态失败");
        }
    }

    fn restart_supervisor(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        slot: u8,
        handle: Arc<Slot>,
        room: RoomRecord,
    ) {
        {
            let mut state = handle.lock();
            if state.closed {
                return;
            }
            state.alive = true;
        }
        let manager = self.clone();
        let app = app.clone();
        tokio::spawn(async move {
            manager.supervise(app, broker, slot, handle, room).await;
        });
    }

    async fn supervise(
        self: Arc<Self>,
        app: AppHandle,
        broker: Arc<StreamBroker>,
        slot: u8,
        handle: Arc<Slot>,
        room: RoomRecord,
    ) {
        let mut attempts: u32 = 0;
        loop {
            let (closed, generation, qn, preferred) = {
                let state = handle.lock();
                (
                    state.closed,
                    state.generation,
                    state.requested_qn,
                    state.preferred_line.clone(),
                )
            };
            if closed {
                break;
            }

            match self
                .start_once(&app, &broker, &handle, &room, qn, preferred, generation)
                .await
            {
                Ok(()) => {
                    attempts = 0;
                    match self.wait_cycle(&handle, generation).await {
                        Cycle::Closed => {
                            self.teardown(&handle).await;
                            break;
                        }
                        Cycle::Restart => {
                            self.teardown(&handle).await;
                            continue;
                        }
                        Cycle::Exited => {
                            let closed = handle.lock().closed;
                            self.teardown(&handle).await;
                            if closed {
                                break;
                            }
                            attempts += 1;
                            if attempts > MAX_RETRIES {
                                let message = crate::i18n::text(
                                    "转封装多次失败，已停止重连",
                                    "Stream remuxing failed repeatedly; reconnecting has stopped.",
                                );
                                {
                                    let mut state = handle.lock();
                                    state.status = "error".into();
                                    state.error = Some(message.into());
                                }
                                self.emit(&app);
                                notify_failure(&app, &room, message);
                                break;
                            }
                            tracing::warn!(
                                slot,
                                room_id = %room.id,
                                attempt = attempts,
                                max = MAX_RETRIES,
                                "观看转封装中断，准备重连"
                            );
                            {
                                let mut state = handle.lock();
                                state.status = "reconnecting".into();
                                state.error = Some(if crate::i18n::is_english() {
                                    format!(
                                        "Remuxing was interrupted; reconnecting (attempt {attempts})"
                                    )
                                } else {
                                    format!("转封装中断，正在第 {attempts} 次重连")
                                });
                            }
                            let wait = backoff(attempts);
                            {
                                let mut state = handle.lock();
                                state.retry_at =
                                    Some(crate::clock::now_ms() + wait.as_millis() as i64);
                            }
                            self.emit(&app);
                            // 丢掉地址缓存，重连时拿一条新线路。
                            // 别用 invalidate：它会把录制侧共享的失败退避也清掉（A5）。
                            broker.invalidate_address(&room_ref(&room));
                            tokio::time::sleep(wait).await;
                            handle.lock().retry_at = None;
                        }
                    }
                }
                Err(err) => {
                    let closed = handle.lock().closed;
                    if closed {
                        self.teardown(&handle).await;
                        break;
                    }
                    attempts += 1;
                    let log_message = err.to_string();
                    let message = err.localized_message();
                    if attempts > MAX_RETRIES {
                        tracing::warn!(slot, room_id = %room.id, attempts, error = %log_message, "观看多次起播失败，放弃重连");
                        {
                            let mut state = handle.lock();
                            state.status = "error".into();
                            state.error = Some(message.clone());
                        }
                        self.emit(&app);
                        notify_failure(&app, &room, &message);
                        self.teardown(&handle).await;
                        break;
                    }
                    tracing::warn!(slot, room_id = %room.id, attempt = attempts, max = MAX_RETRIES, error = %log_message, "观看起播失败，准备重连");
                    {
                        let mut state = handle.lock();
                        state.status = "reconnecting".into();
                        state.error = Some(message);
                    }
                    let wait = backoff(attempts);
                    {
                        let mut state = handle.lock();
                        state.retry_at = Some(crate::clock::now_ms() + wait.as_millis() as i64);
                    }
                    self.emit(&app);
                    broker.invalidate_address(&room_ref(&room));
                    tokio::time::sleep(wait).await;
                    handle.lock().retry_at = None;
                }
            }
        }

        handle.lock().alive = false;
        tracing::debug!(slot, "观看监督循环退出");
    }

    // 启动参数来自槽位与房间快照，显式传参比临时结构体更清楚。
    #[allow(clippy::too_many_arguments)]
    async fn start_once(
        &self,
        app: &AppHandle,
        broker: &StreamBroker,
        handle: &Arc<Slot>,
        room: &RoomRecord,
        qn: u32,
        preferred: Option<String>,
        generation: u64,
    ) -> Result<(), AppError> {
        handle.lock().retry_at = None;
        if ffmpeg::resolve().is_none() {
            tracing::warn!(room_id = %room.id, "观看起播失败：找不到 ffmpeg");
            return Err(AppError::FfmpegMissing);
        }

        let plan = broker.plan(&room_ref(room), qn).await.map_err(|err| {
            tracing::warn!(room_id = %room.id, qn, error = %err, "观看解析线路失败");
            AppError::from(err)
        })?;
        let (candidate, group) =
            pick_view_candidate(&plan.groups, preferred.as_deref()).ok_or_else(|| {
                tracing::warn!(room_id = %room.id, qn, groups = plan.groups.len(), "观看没有可用线路");
                AppError::User("没有可用的线路".into())
            })?;

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.root.join(&session_id);
        tokio::fs::create_dir_all(&dir).await.map_err(|err| {
            tracing::warn!(room_id = %room.id, dir = %dir.display(), error = %err, "创建观看缓存目录失败");
            AppError::from_io(err, &dir)
        })?;

        let mut command = crate::process::background_async(ffmpeg::binary());
        // 让 init.mp4 / 分片 / m3u8 都以会话目录为工作目录：
        // ffmpeg 的 -hls_fmp4_init_filename 是相对 cwd 的，之前会写到应用目录，
        // 播放器按 m3u8 请求播放会话目录里的 init.mp4 就会 404。
        command
            .current_dir(&dir)
            .arg("-hide_banner")
            .arg("-nostdin")
            .arg("-loglevel")
            .arg("warning")
            .arg("-y")
            .arg("-rw_timeout")
            .arg("10000000");

        let mut headers = String::new();
        for (name, value) in &candidate.headers {
            headers.push_str(name);
            headers.push_str(": ");
            headers.push_str(value);
            headers.push_str("\r\n");
        }
        if !headers.is_empty() {
            command.arg("-headers").arg(headers);
        }

        command
            .arg("-i")
            .arg(candidate.url.as_deref().unwrap_or_default())
            .arg("-map")
            .arg("0:v:0")
            .arg("-map")
            .arg("0:a:0?")
            .arg("-c")
            .arg("copy")
            // B 站 HLS/TS 线路的音频是 AAC-ADTS；直接 copy 进 fMP4 会报
            // "Malformed AAC bitstream detected"，一个分片都写不出来。
            // 对 FLV 的 AAC 源该 bsf 是空操作，统一加上即可。
            .arg("-bsf:a")
            .arg("aac_adtstoasc")
            .arg("-f")
            .arg("hls")
            .arg("-hls_time")
            .arg("1")
            .arg("-hls_list_size")
            .arg("6")
            .arg("-hls_flags")
            .arg("delete_segments+append_list+omit_endlist")
            .arg("-hls_segment_type")
            .arg("fmp4")
            .arg("-hls_fmp4_init_filename")
            .arg("init.mp4")
            .arg("-hls_segment_filename")
            .arg("seg_%05d.m4s")
            .arg("index.m3u8")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(ffmpeg_log(&self.root));

        let child = command.spawn().map_err(|err| {
            tracing::warn!(
                room_id = %handle.room.id,
                quality = %candidate.name,
                line = %group.name,
                %err,
                "启动观看转封装失败"
            );
            AppError::Ffmpeg(format!("启动转封装失败：{err}"))
        })?;
        self.server.register(&session_id, dir.clone());

        {
            let mut state = handle.lock();
            state.child = Some(child);
            state.session_id = Some(session_id.clone());
            state.dir = Some(dir.clone());
            state.status = "starting".into();
            state.error = None;
            state.actual_qn = actual_qn(&candidate);
            state.quality = Some(candidate.name.clone());
            state.line = Some(group.name.clone());
            state.url = None;
        }
        self.emit(app);

        // 等第一个分片落盘再交给播放器，否则前端会拿到 404 的 m3u8 直接报错。
        match self.wait_ready(handle, &dir, generation).await {
            Ready::Ok => {
                let url = self.server.playlist_url(&session_id);
                {
                    let mut state = handle.lock();
                    if state.closed || state.generation != generation {
                        return Err(AppError::User("会话已取消".into()));
                    }
                    state.url = Some(url);
                    state.status = "playing".into();
                }
                self.emit(app);
                tracing::info!(
                    room_id = handle.room.id,
                    quality = %candidate.name,
                    line = %group.name,
                    "观看起播"
                );
                Ok(())
            }
            Ready::Closed => Err(AppError::User("会话已取消".into())),
            Ready::Timeout => {
                tracing::warn!(
                    room_id = %handle.room.id,
                    quality = %candidate.name,
                    line = %group.name,
                    "观看起播超时（12 秒内没有输出分片）"
                );
                let mut state = handle.lock();
                if let Some(child) = state.child.as_mut() {
                    send_graceful(child);
                }
                Err(AppError::Ffmpeg("起播超时（12 秒内没有输出分片）".into()))
            }
        }
    }

    async fn wait_ready(&self, handle: &Arc<Slot>, dir: &Path, generation: u64) -> Ready {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            {
                let mut state = handle.lock();
                if state.closed || state.generation != generation {
                    if let Some(child) = state.child.as_mut() {
                        send_graceful(child);
                    }
                    return Ready::Closed;
                }
                if let Some(child) = state.child.as_mut() {
                    if let Ok(Some(_)) = child.try_wait() {
                        return Ready::Timeout;
                    }
                }
            }
            if playlist_ready(dir).await {
                return Ready::Ok;
            }
            if Instant::now() >= deadline {
                return Ready::Timeout;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_cycle(&self, handle: &Arc<Slot>, generation: u64) -> Cycle {
        loop {
            tokio::time::sleep(POLL).await;
            let (closed, restart, exited) = {
                let mut state = handle.lock();
                let exited = match state.child.as_mut() {
                    Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                    None => true,
                };
                (state.closed, state.generation != generation, exited)
            };
            if closed {
                return Cycle::Closed;
            }
            if restart {
                return Cycle::Restart;
            }
            if exited {
                return Cycle::Exited;
            }
        }
    }

    /// 收掉当前子进程、注销会话、删缓存目录。
    async fn teardown(&self, handle: &Arc<Slot>) {
        let (session, dir, mut child) = {
            let mut state = handle.lock();
            (
                state.session_id.take(),
                state.dir.take(),
                state.child.take(),
            )
        };

        if let Some(child) = child.as_mut() {
            send_graceful(child);
            if wait_exit(child, STOP_TIMEOUT).await.is_none() {
                tracing::warn!(room_id = handle.room.id, "转封装进程未在超时内退出，已强杀");
            }
        }
        if let Some(session) = session.as_deref() {
            self.server.unregister(session);
        }
        if let Some(dir) = dir {
            if let Err(err) = tokio::fs::remove_dir_all(&dir).await {
                tracing::debug!(room_id = handle.room.id, dir = %dir.display(), error = %err, "删除观看分片目录失败");
            }
        }

        let mut state = handle.lock();
        state.url = None;
        state.session_id = None;
        state.dir = None;
    }
}

enum Ready {
    Ok,
    Closed,
    Timeout,
}

async fn playlist_ready(dir: &Path) -> bool {
    let Ok(text) = tokio::fs::read_to_string(dir.join("index.m3u8")).await else {
        return false;
    };
    if !text.contains("#EXTINF") {
        return false;
    }
    // fMP4 的 init.mp4 可能比 m3u8 晚一拍落盘；先交给播放器会让 hls.js
    // 立刻请求 init.mp4 并拿到 404。等它真正可读再起播。
    tokio::fs::metadata(dir.join("init.mp4"))
        .await
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
}

/// 转封装失败时唯一的现场：把 stderr 落到 <cache>/view/ffmpeg.log，随启动清空。
fn ffmpeg_log(root: &std::path::Path) -> Stdio {
    let path = root.join("ffmpeg.log");
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    }
}

fn actual_qn(candidate: &StreamCandidate) -> u32 {
    candidate
        .attrs
        .get("qn")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn room_ref(room: &RoomRecord) -> RoomRef {
    RoomRef {
        platform: room.platform.clone(),
        room_key: room.room_key.clone(),
        canonical_url: room.canonical_url.clone(),
        input: room.canonical_url.clone(),
    }
}

fn backoff(attempts: u32) -> Duration {
    let factor = 2u32.saturating_pow(attempts.saturating_sub(1).min(3));
    (RETRY_BASE * factor).min(RETRY_MAX)
}

/// 每个线路组内优先 H.264（hls.js 兼容性最好），其次是 FLV（断流最稳）。
fn pick_view_candidate(
    groups: &[StreamGroup],
    preferred: Option<&str>,
) -> Option<(StreamCandidate, StreamGroup)> {
    if let Some(name) = preferred {
        match groups.iter().find(|group| group.name == name) {
            Some(group) if group.candidates.iter().any(|item| item.url.is_some()) => {
                if let Some(candidate) = best_in_group(group) {
                    return Some((candidate, group.clone()));
                }
            }
            Some(_) => {
                tracing::warn!(line = name, "指定线路没有可用地址，改用其它线路")
            }
            None => tracing::warn!(line = name, "指定线路不存在，改用其它线路"),
        }
    }
    for group in groups {
        if let Some(candidate) = best_in_group(group) {
            return Some((candidate, group.clone()));
        }
    }
    None
}

fn best_in_group(group: &StreamGroup) -> Option<StreamCandidate> {
    let mut best: Option<&StreamCandidate> = None;
    for candidate in &group.candidates {
        if candidate.url.is_none() {
            continue;
        }
        match best {
            Some(current) if candidate_rank(current) >= candidate_rank(candidate) => {}
            _ => best = Some(candidate),
        }
    }
    best.cloned()
}

fn candidate_rank(candidate: &StreamCandidate) -> (u8, u8, u64) {
    let codec = u8::from(candidate.codec == Some(Codec::H264));
    let format = match candidate.format {
        StreamFormat::Flv => 3,
        StreamFormat::Hls => 2,
        StreamFormat::HttpTs => 1,
        _ => 0,
    };
    (codec, format, candidate.weight as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};

    fn candidate(format: StreamFormat, codec: Codec, qn: u32) -> StreamCandidate {
        let mut attrs = std::collections::BTreeMap::new();
        attrs.insert("qn".to_string(), qn.to_string());
        StreamCandidate {
            id: format!("{qn}"),
            name: format!("{qn}"),
            weight: qn as f64,
            format,
            codec: Some(codec),
            width: None,
            height: None,
            fps: None,
            bitrate_kbps: None,
            url: Some("http://example.com/live".into()),
            headers: Default::default(),
            attrs,
        }
    }

    #[test]
    fn prefers_h264_flv_over_hevc() {
        let group = StreamGroup {
            name: "line".into(),
            priority: 0,
            candidates: vec![
                candidate(StreamFormat::Flv, Codec::H265, 10000),
                candidate(StreamFormat::Flv, Codec::H264, 250),
            ],
        };
        let (picked, _) = pick_view_candidate(&[group], None).expect("有候选");
        assert_eq!(picked.codec, Some(Codec::H264));
        assert_eq!(actual_qn(&picked), 250);
    }

    #[test]
    fn skips_candidates_without_url() {
        let mut broken = candidate(StreamFormat::Flv, Codec::H264, 10000);
        broken.url = None;
        let group = StreamGroup {
            name: "line".into(),
            priority: 0,
            candidates: vec![broken],
        };
        assert!(pick_view_candidate(&[group], None).is_none());
    }

    #[test]
    fn backoff_is_capped() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(8));
        assert_eq!(backoff(9), RETRY_MAX);
    }

    fn line_group(name: &str, url: Option<&str>) -> StreamGroup {
        let mut item = candidate(StreamFormat::Flv, Codec::H264, 10000);
        item.url = url.map(str::to_string);
        StreamGroup {
            name: name.into(),
            priority: 0,
            candidates: vec![item],
        }
    }

    #[test]
    fn preferred_line_wins() {
        let groups = vec![
            line_group("A", Some("https://a/1.flv")),
            line_group("B", Some("https://b/1.flv")),
        ];
        let (picked, group) = pick_view_candidate(&groups, Some("B")).expect("应该挑到候选");
        assert_eq!(group.name, "B");
        assert_eq!(picked.url.as_deref(), Some("https://b/1.flv"));
    }

    #[test]
    fn preferred_line_without_url_falls_back() {
        let groups = vec![
            line_group("A", Some("https://a/1.flv")),
            line_group("C", None),
        ];
        let (_, group) = pick_view_candidate(&groups, Some("C")).expect("应该回退到可用线路");
        assert_eq!(group.name, "A");
    }

    #[test]
    fn unknown_preferred_line_falls_back() {
        let groups = vec![line_group("A", Some("https://a/1.flv"))];
        let (_, group) = pick_view_candidate(&groups, Some("Z")).expect("应该回退到可用线路");
        assert_eq!(group.name, "A");
    }
}
