//! 录制引擎：一个房间一个 ffmpeg 进程，分段 mpegts 直录，收尾合成 mp4。
//!
//! 为什么分段（M7.1）：进程被强杀 / 断电时，单文件 FLV 的索引与尾部会丢，
//! 整场录像可能直接不可播；segment muxer 每 5 分钟落一个独立 ts，
//! 崩溃最多丢最后一片，下次启动还能把已有分片合成 mp4。
//!
//! 状态机：idle → recording → (stopped | stream_ended | failed)。
//! 进程退出由 1 秒轮询发现（tokio::process 的 wait 不能与 stop 并存取同一子进程）。
//! 意外断流（不是用户停、也不是平台下播）会先在同一个会话里续录最多 3 次，
//! 分片接着编号，避免一次抖动就把整场切成两条历史；彻底失败才收尾。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;
use std::time::{Duration, Instant};

use hivecast_ipc::{AppNotice, RecordSession, RecordingInfo, RoomRecord};
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::Child;

use crate::broker::StreamBroker;
use crate::danmaku::{DanmakuHub, RecordWriter};
use crate::db::Db;
use crate::disk;
use crate::error::AppError;
use crate::ffmpeg;
use crate::platform::StreamGroup;
use crate::process::send_graceful;
use crate::segment;

/// 进程存活不足这个时长就退出，视为「秒退」，需要换线路重试。
const EARLY_EXIT: Duration = Duration::from_secs(12);
/// 连续失败后的冷却时间，避免对着同一个坏房间疯狂重试。
const FAILURE_COOLDOWN: Duration = Duration::from_secs(60);
/// 断流自动续录的最大次数：网络抖动通常一次就够，连续 3 次说明源头真断了。
const MAX_RESTARTS: u32 = 3;

pub const EVENT_ROOMS_CHANGED: &str = "rooms://changed";
/// 录制状态播报事件：前端收到后弹系统通知（文案走 i18n）。
pub const EVENT_APP_NOTICE: &str = "app://notice";

struct Active {
    recording: RecordingInfo,
    child: Child,
    /// 用户主动停止 / 平台下播时写入，用于收尾判定
    stop_reason: Option<String>,
    started: Instant,
    /// 通知里要显示的主播名，收尾时从内存取，避免再查一次库
    host_name: String,
    /// 本场录制的弹幕写入器（与视频同名 .jsonl）
    danmaku: Option<Arc<RecordWriter>>,
    danmaku_path: Option<String>,
    /// 已续录的次数，达到 MAX_RESTARTS 后不再重开
    restarts: u32,
}

pub struct Recorder {
    db: Arc<Db>,
    /// 媒体库根目录：设置页可以改，开录时现取，避免改完要重启。
    media_root: std::sync::RwLock<PathBuf>,
    /// 同时录制上限；0 表示不限。
    max_concurrent: std::sync::atomic::AtomicUsize,
    inner: Mutex<HashMap<String, Active>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// 最近一次失败原因，展示在监控页行内
    errors: Mutex<HashMap<String, String>>,
    /// 每房间一把开录锁：监控循环、添加房间、手动开录可能同时触发 start，
    /// 只有先拿到锁的那次能真正拉起 ffmpeg，其余走到里面会发现已有进程直接返回。
    start_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// 用户手动停录过的房间：在主播下播前不再自动开录，避免「停了又被拉起来」。
    suppressed: Mutex<HashSet<String>>,
    /// 弹幕扇出中心：录制与观看共用一条房间连接。测试里可以不挂。
    danmaku: Option<Arc<DanmakuHub>>,
    /// 缩略图生成器：录制一收尾就排队生成九宫格封面，媒体库打开时直接有图。
    thumbs: std::sync::RwLock<Option<Arc<crate::thumbs::Thumbs>>>,
}

impl Recorder {
    pub fn new(db: Arc<Db>, media_root: PathBuf, danmaku: Option<Arc<DanmakuHub>>) -> Self {
        Self {
            db,
            media_root: std::sync::RwLock::new(media_root),
            max_concurrent: std::sync::atomic::AtomicUsize::new(4),
            inner: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            start_locks: Mutex::new(HashMap::new()),
            suppressed: Mutex::new(HashSet::new()),
            danmaku,
            thumbs: std::sync::RwLock::new(None),
        }
    }

    /// 由 AppState 在启动时挂上缩略图生成器（录制模块不能反向依赖 state）。
    pub fn attach_thumbs(&self, thumbs: Arc<crate::thumbs::Thumbs>) {
        *self.thumbs.write().expect("缩略图状态被污染") = Some(thumbs);
    }

    /// 录制结束后立即生成封面：不等用户打开媒体库。
    fn queue_thumb(&self, app: &AppHandle, session_id: &str) {
        let Some(thumbs) = self
            .thumbs
            .read()
            .expect("缩略图状态被污染")
            .clone()
        else {
            return;
        };
        // 时长与最终路径都以库里的为准：收尾时可能已经从分片合成成 mp4。
        let Ok(Some(session)) = self.db.get_session(session_id) else {
            return;
        };
        let Some(path) = session.file_path.filter(|one| !one.is_empty()) else {
            return;
        };
        thumbs.ensure(app.clone(), vec![(session.id, path, session.duration_ms)]);
    }

    pub fn media_root(&self) -> PathBuf {
        self.media_root
            .read()
            .expect("媒体目录被污染")
            .clone()
    }

    /// 设置页改目录后立即对新录制生效；正在录制的进程仍写旧路径。
    pub fn set_media_root(&self, root: PathBuf) {
        *self.media_root.write().expect("媒体目录被污染") = root;
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_max_concurrent(&self, value: usize) {
        self.max_concurrent
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    /// 当前正在录制的路数。
    pub fn active_count(&self) -> usize {
        self.inner.lock().expect("录制状态被污染").len()
    }

    pub fn active(&self, room_id: &str) -> Option<RecordingInfo> {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .map(|active| active.recording.clone())
    }

    #[allow(dead_code)]
    pub fn active_all(&self) -> Vec<RecordingInfo> {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .values()
            .map(|active| active.recording.clone())
            .collect()
    }

    pub fn last_error(&self, room_id: &str) -> Option<String> {
        self.errors
            .lock()
            .expect("错误状态被污染")
            .get(room_id)
            .cloned()
    }

    pub fn set_error(&self, room_id: &str, message: impl Into<String>) {
        self.errors
            .lock()
            .expect("错误状态被污染")
            .insert(room_id.to_string(), message.into());
    }

    pub fn clear_error(&self, room_id: &str) {
        self.errors
            .lock()
            .expect("错误状态被污染")
            .remove(room_id);
    }

    /// 手动停录后是否仍处于「不要再自动开录」状态。
    pub fn auto_suppressed(&self, room_id: &str) -> bool {
        self.suppressed
            .lock()
            .expect("停录标记被污染")
            .contains(room_id)
    }

    pub fn resume_auto(&self, room_id: &str) {
        self.suppressed
            .lock()
            .expect("停录标记被污染")
            .remove(room_id);
    }

    pub fn is_recording(&self, room_id: &str) -> bool {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .contains_key(room_id)
    }

    /// 当前这段 ffmpeg 已经活了多久；没在录时返回 None。
    fn elapsed(&self, room_id: &str) -> Option<Duration> {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .map(|active| active.started.elapsed())
    }

    /// 开录。已有进程时直接返回现有快照（幂等）。
    pub async fn start(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: &StreamBroker,
        room: &RoomRecord,
    ) -> Result<RecordingInfo, AppError> {
        // 先拿房间级开录锁，再复查状态；否则并发调用会各自拉起一个 ffmpeg 写同一个文件。
        let gate = self
            .start_locks
            .lock()
            .expect("开录锁被污染")
            .entry(room.id.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _gate = gate.lock().await;

        if let Some(existing) = self.active(&room.id) {
            return Ok(existing);
        }
        if self.cooling_down(&room.id) {
            return Err(AppError::User("该房间刚刚连续失败，稍后再试".into()));
        }

        // 并发上限：4 路 1080p 已经能吃掉一块 SSD 的写入带宽，默认不再往上加。
        let limit = self.max_concurrent();
        if limit > 0 && self.active_count() >= limit {
            return Err(AppError::User(format!(
                "同时录制已达上限（{limit} 路），先停掉一路或到设置里调高上限"
            )));
        }

        // 磁盘写满会让 ffmpeg 直接退出，还会把系统拖死，所以先拦一道。
        // can_start 是开录门槛，is_low 是录制红线：两个都得过，
        // 否则「已有录制被红线停掉、监控又立刻重新开录」会变成反复起停。
        let media_root = self.media_root();
        if !disk::can_start(&media_root) || disk::is_low(&media_root) {
            let message = disk::start_blocked_message(&media_root);
            self.set_error(&room.id, message.clone());
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            return Err(AppError::User(message));
        }

        let groups = broker
            .groups(
                &crate::platform::RoomRef {
                    platform: room.platform.clone(),
                    room_key: room.room_key.clone(),
                    canonical_url: room.canonical_url.clone(),
                    input: room.canonical_url.clone(),
                },
                room.record_qn,
            )
            .await
            .map_err(AppError::from)?;

        let (candidate, group) = pick_candidate(&groups, room.record_line.as_deref())
            .ok_or_else(|| AppError::User("没有可用的线路".into()))?;

        let started_at = crate::db::now_ts();
        let path = session_path(
            &media_root,
            platform_label(&room.platform),
            &room.host_name,
            &room.title,
            started_at,
            "mp4",
        );
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| AppError::from_io(err, parent))?;
        }

        let headers: Vec<(String, String)> = candidate
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let url = candidate.url.clone().unwrap_or_default();

        let mut command = build_command(&path, &url, &headers, 0);
        let mut child = command
            .spawn()
            .map_err(|err| AppError::Ffmpeg(format!("启动 ffmpeg 失败：{err}")))?;
        drain_stderr(&mut child);

        // 弹幕落盘与视频同名，只换扩展名；后处理阶段按这个约定找旁挂文件。
        let danmaku_path = path.with_extension("jsonl");
        let danmaku = self
            .danmaku
            .as_ref()
            .filter(|_| crate::danmaku::supports(room))
            .and_then(|hub| hub.attach_record(room, &danmaku_path));
        if self.danmaku.is_some() && danmaku.is_none() && crate::danmaku::supports(room) {
            tracing::warn!(path = %danmaku_path.display(), "弹幕文件创建失败，本场只录视频");
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        self.db
            .create_session(&RecordSession {
                id: session_id.clone(),
                room_id: room.id.clone(),
                started_at,
                ended_at: None,
                status: "recording".into(),
                reason: None,
                file_path: Some(path.display().to_string()),
                file_size: 0,
                duration_ms: 0,
                quality: Some(candidate.name.clone()),
                line: Some(group.name.clone()),
                error: None,
                danmaku_path: danmaku.as_ref().map(|_| danmaku_path.display().to_string()),
                danmaku_count: 0,
            })
            .map_err(|err| AppError::Internal(err.to_string()))?;

        let recording = RecordingInfo {
            room_id: room.id.clone(),
            session_id,
            file_path: path.display().to_string(),
            started_at,
            quality: candidate.name.clone(),
            line: group.name.clone(),
        };

        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            inner.insert(
                room.id.clone(),
                Active {
                    recording: recording.clone(),
                    child,
                    stop_reason: None,
                    started: Instant::now(),
                    host_name: room.host_name.clone(),
                    danmaku,
                    danmaku_path: Some(danmaku_path.display().to_string()),
                    restarts: 0,
                },
            );
        }

        tracing::info!(
            room = %room.host_name,
            file = %path.display(),
            quality = %candidate.name,
            "开始录制"
        );
        self.clear_error(&room.id);
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        emit_notice(app, "started", &room.id, &room.host_name, None);
        self.spawn_watcher(app.clone(), room.id.clone());
        Ok(recording)
    }

    /// 停止录制。reason 会写进历史（stopped / stream_ended）。
    pub async fn stop(
        self: &Arc<Self>,
        app: &AppHandle,
        room_id: &str,
        reason: &str,
    ) -> Result<(), AppError> {
        if reason == "stopped" {
            self.suppressed
                .lock()
                .expect("停录标记被污染")
                .insert(room_id.to_string());
        }
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get_mut(room_id) else {
                return Ok(());
            };
            active.stop_reason = Some(reason.to_string());
            send_graceful(&mut active.child);
        }
        tracing::info!(room_id, reason, "请求停止录制");
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        Ok(())
    }

    /// 退出应用前调用：尽力让 ffmpeg 收尾。
    ///
    /// 只发信号不够——运行时随窗口一起销毁后 watcher 就没了，会话会永远停在
    /// recording。所以这里等进程退出后自己收尾一次，保证历史里有正确的大小。
    pub async fn shutdown_all(self: &Arc<Self>, app: &AppHandle) {
        let ids: Vec<String> = self
            .inner
            .lock()
            .expect("录制状态被污染")
            .keys()
            .cloned()
            .collect();
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            for (_, active) in inner.iter_mut() {
                active.stop_reason = Some("app_exit".into());
                send_graceful(&mut active.child);
            }
        }
        for id in ids {
            let mut status = None;
            for _ in 0..40 {
                if let Some(found) = self.poll_exit(&id) {
                    status = Some(found);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let status = match status {
                Some(status) => status,
                None => {
                    {
                        let mut inner = self.inner.lock().expect("录制状态被污染");
                        if let Some(active) = inner.get_mut(&id) {
                            let _ = active.child.start_kill();
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    self.poll_exit(&id).unwrap_or_else(|| {
                        // 实在拿不到退出码，也要把会话收掉，避免脏数据。
                        (
                            std::process::ExitStatus::default(),
                            Some("app_exit".to_string()),
                        )
                    })
                }
            };
            self.finalize(app, &id, status.0, status.1).await;
        }
    }

    fn spawn_watcher(self: &Arc<Self>, app: AppHandle, room_id: String) {
        let recorder = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                let Some(rec) = recorder.upgrade() else { return };
                if !rec.is_recording(&room_id) {
                    return;
                }
                let Some((status, stop_reason)) = rec.poll_exit(&room_id) else {
                    continue;
                };
                // 意外断流（不是用户停、也不是平台正常下播）先续录，
                // 网络抖一下不该把整场录像切成两条历史。
                // 退出码 0 但只活了不到 EARLY_EXIT 的同样算断流：播放地址过期时
                // ffmpeg 会当成正常 EOF 退出，不重试的话整场录像就此提前结束。
                let early = rec
                    .elapsed(&room_id)
                    .is_some_and(|elapsed| elapsed < EARLY_EXIT);
                if stop_reason.is_none() && (!status.success() || early) {
                    let broker = {
                        let state = app.state::<crate::state::AppState>();
                        state.broker.clone()
                    };
                    if rec.respawn(&app, &broker, &room_id).await {
                        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                        continue;
                    }
                }
                rec.finalize(&app, &room_id, status, stop_reason).await;
                return;
            }
        });
    }

    fn poll_exit(&self, room_id: &str) -> Option<(std::process::ExitStatus, Option<String>)> {
        let mut inner = self.inner.lock().expect("录制状态被污染");
        let active = inner.get_mut(room_id)?;
        match active.child.try_wait() {
            Ok(Some(status)) => Some((status, active.stop_reason.clone())),
            _ => None,
        }
    }

    /// 断流续录：同一个会话里重开 ffmpeg，分片从已有数量接着编号。
    ///
    /// 不新开会话的理由：用户看到的「这一场直播」还没结束，
    /// 媒体库里出现两条半场录像比丢几十秒更难理解。
    ///
    /// 必须重新解析线路：平台播放地址带时效签名（斗鱼 anti-code、虎牙 wsSecret），
    /// 复用旧地址时新进程往往一两秒就 EOF——那正是"续录成功但立刻结束"的成因。
    async fn respawn(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: &StreamBroker,
        room_id: &str,
    ) -> bool {
        // 取快照后立刻放锁：下面要 await 解析线路，持有 std::sync::Mutex 会卡死整个录制表。
        let (session_id, path, start_number, restarts) = {
            let inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get(room_id) else {
                return false;
            };
            if active.restarts >= MAX_RESTARTS {
                tracing::warn!(room_id, restarts = active.restarts, "续录次数用尽");
                return false;
            }
            let path = PathBuf::from(&active.recording.file_path);
            let start_number = segment::collect_parts(&path).len() as u32;
            (
                active.recording.session_id.clone(),
                path,
                start_number,
                active.restarts,
            )
        };

        let room = match self.db.get_room(room_id) {
            Ok(Some(room)) => room,
            Ok(None) => return false,
            Err(err) => {
                tracing::warn!(%err, room_id, "续录读取房间失败");
                return false;
            }
        };
        let reference = crate::monitor::room_ref(&room);
        // 断流往往就是旧地址失效：必须丢掉缓存重新解析，
        // 否则续录用回同一个过期地址，只会「续上就断」循环到次数用尽。
        broker.invalidate(&reference);
        // 给 CDN 一点时间回收上一条连接：立刻重连时常被判定为重复会话而秒断。
        tokio::time::sleep(Duration::from_secs(3)).await;
        let groups = match broker.groups(&reference, room.record_qn).await {
            Ok(groups) => groups,
            Err(err) => {
                tracing::warn!(%err, room_id, "续录解析线路失败");
                return false;
            }
        };
        let Some((candidate, _group)) = pick_candidate(&groups, room.record_line.as_deref()) else {
            tracing::warn!(room_id, "续录没有可用线路");
            return false;
        };
        let headers: Vec<(String, String)> = candidate
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let url = candidate.url.clone().unwrap_or_default();

        let mut command = build_command(&path, &url, &headers, start_number);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(%err, room_id, "续录启动失败");
                return false;
            }
        };
        drain_stderr(&mut child);

        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get_mut(room_id) else {
                let _ = child.start_kill();
                return false;
            };
            // 解析期间可能被停掉或换成新会话，别把旧进程挂到新会话上。
            if active.recording.session_id != session_id {
                let _ = child.start_kill();
                return false;
            }
            active.child = child;
            active.restarts = restarts + 1;
            active.started = Instant::now();
            tracing::info!(
                room_id,
                start_number,
                restarts = active.restarts,
                quality = %candidate.name,
                "断流后续录"
            );
        }
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        true
    }

    async fn finalize(
        self: &Arc<Self>,
        app: &AppHandle,
        room_id: &str,
        status: std::process::ExitStatus,
        stop_reason: Option<String>,
    ) {
        let active = {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            inner.remove(room_id)
        };
        let Some(active) = active else { return };

        let elapsed = active.started.elapsed();
        // 收尾产物：分片 → mp4；合成失败退化为字节拼接的 .ts；再不行只能报分片总量。
        let source_path = PathBuf::from(&active.recording.file_path);
        let auto_remux = crate::settings::load(&self.db).auto_remux;
        // 收尾里的转封装同样会写文件：登记成任务，退出时才会等它写完再走。
        let jobs = app.state::<crate::state::AppState>().jobs.clone();
        let remux_label = source_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| active.recording.session_id.clone());
        let _remux_job = auto_remux.then(|| {
            jobs.begin("remux", remux_label, Some(active.recording.quality.clone()))
        });
        let (stored_path, file_size) = finish_file(&source_path, auto_remux).await;
        drop(_remux_job);
        if stored_path != source_path {
            if let Err(err) = self.db.update_session_file(
                &active.recording.session_id,
                &stored_path.display().to_string(),
                file_size,
            ) {
                tracing::warn!(%err, "改写录像路径失败");
            }
        }

        let (final_status, reason, error) = match stop_reason.as_deref() {
            Some("stopped") => ("finished", "stopped", None),
            Some("stream_ended") => ("finished", "stream_ended", None),
            Some("app_exit") => ("aborted", "app_exit", None),
            Some("disk_full") => ("aborted", "disk_full", None),
            _ if status.success() => ("finished", "stream_ended", None),
            _ => {
                let message = format!("ffmpeg 异常退出：{status}");
                // 秒退 + 有输出文件也几乎为空 → 记为失败并进入冷却
                if elapsed < EARLY_EXIT {
                    self.record_failure(room_id);
                }
                ("failed", "error", Some(message))
            }
        };

        // 先收弹幕再写库：detach 会 flush 并返回真实条数，历史页才有准确数字。
        let (danmaku_path, danmaku_count) = match (self.danmaku.as_ref(), active.danmaku.as_ref()) {
            (Some(hub), Some(writer)) => match hub.detach_record(room_id, writer) {
                Some((path, count)) => (Some(path.display().to_string()), count as i64),
                None => (active.danmaku_path.clone(), 0),
            },
            _ => (active.danmaku_path.clone(), 0),
        };

        if let Err(err) = self.db.finish_session(
            &active.recording.session_id,
            final_status,
            reason,
            file_size,
            danmaku_path.as_deref(),
            danmaku_count,
            error.as_deref(),
        ) {
            tracing::warn!(%err, "写入录制历史失败");
        }

        // 弹幕存档（.jsonl）随录随写；收尾时顺手导出一份 .ass，媒体目录里直接可用。
        if let Some(path) = danmaku_path.as_deref().filter(|_| danmaku_count > 0) {
            match crate::postprocess::danmaku_to_ass(std::path::Path::new(path)) {
                Ok(result) => tracing::info!(ass = %result.path, "已导出弹幕字幕"),
                Err(err) => tracing::warn!(%err, "导出弹幕字幕失败"),
            }
        }

        if final_status == "finished" {
            self.clear_failure(room_id);
            self.queue_thumb(app, &active.recording.session_id);
        }

        // 每次收尾跑一次保留策略：录制是媒体库唯一的增长来源。
        crate::retention::enforce_from_settings(&self.db);

        tracing::info!(
            room_id,
            status = final_status,
            reason,
            file_size,
            "录制结束"
        );
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());

        match (final_status, reason) {
            ("finished", "stream_ended") => {
                emit_notice(app, "ended", room_id, &active.host_name, None)
            }
            ("aborted", "disk_full") => {
                emit_notice(app, "disk_full", room_id, &active.host_name, None)
            }
            ("failed", _) => emit_notice(app, "failed", room_id, &active.host_name, error.clone()),
            _ => {}
        }
    }

    fn record_failure(&self, room_id: &str) {
        let mut failures = self.failures.lock().expect("失败状态被污染");
        let entry = failures
            .entry(room_id.to_string())
            .or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
    }

    fn clear_failure(&self, room_id: &str) {
        self.failures
            .lock()
            .expect("失败状态被污染")
            .remove(room_id);
    }

    fn cooling_down(&self, room_id: &str) -> bool {
        let failures = self.failures.lock().expect("失败状态被污染");
        match failures.get(room_id) {
            Some((attempts, at)) => *attempts >= 3 && at.elapsed() < FAILURE_COOLDOWN,
            None => false,
        }
    }
}

/// 分段录制的命令行。
///
/// - segment muxer + mpegts：每 SEGMENT_SECONDS 秒落一片，进程被杀也只丢尾片
/// - reset_timestamps：每片时间戳从 0 开始，否则合并后音画会错位
/// - start_number：续录时接着编号，避免覆盖已有分片
fn build_command(
    path: &Path,
    url: &str,
    headers: &[(String, String)],
    start_number: u32,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(ffmpeg::binary());
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("warning")
        .arg("-y");
    if !headers.is_empty() {
        let mut raw = String::new();
        for (name, value) in headers {
            raw.push_str(name);
            raw.push_str(": ");
            raw.push_str(value);
            raw.push_str("\r\n");
        }
        command.arg("-headers").arg(raw);
    }
    command
        .arg("-i")
        .arg(url)
        .arg("-c")
        .arg("copy")
        .arg("-f")
        .arg("segment")
        .arg("-segment_time")
        .arg(segment::SEGMENT_SECONDS.to_string())
        .arg("-segment_format")
        .arg("mpegts")
        .arg("-reset_timestamps")
        .arg("1");
    if start_number > 0 {
        command
            .arg("-segment_start_number")
            .arg(start_number.to_string());
    }
    command
        .arg(segment::parts_pattern(path))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    command
}

/// 把 ffmpeg 的 stderr 读空。
///
/// 不读会死锁：管道缓冲区（约 64KB）写满后 ffmpeg 就卡在写日志上，
/// 表现是「录到一半没声音也没退出」。这里只做排水，内容进 debug 日志。
fn drain_stderr(child: &mut Child) {
    let Some(stderr) = child.stderr.take() else {
        return;
    };
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if !line.is_empty() {
                tracing::debug!(target: "ffmpeg", %line, "录制 ffmpeg");
            }
        }
    });
}

/// 收尾产物落定，返回（最终路径, 字节数）。
async fn finish_file(path: &Path, remux: bool) -> (PathBuf, i64) {
    if remux {
        match segment::merge(path).await {
            Ok(size) => return (path.to_path_buf(), size as i64),
            Err(err) => tracing::warn!(%err, "分片合成 mp4 失败，改用字节拼接"),
        }
    } else {
        tracing::info!("文件后处理已关闭，保留 .ts 拼接结果");
    }
    match segment::fallback_concat(path).await {
        Ok((target, size)) => (target, size as i64),
        Err(err) => {
            tracing::warn!(%err, "分片拼接失败，只能记录分片总大小");
            (path.to_path_buf(), segment::parts_total(path) as i64)
        }
    }
}

/// 启动时恢复上次崩溃留下的分片。
///
/// 必须在 close_orphan_sessions 之前调用：那一步会把 status 改成 aborted，
/// 之后就分不清「崩溃遗留」和「正常收尾的旧录像」了。
pub async fn recover_parts(db: &Db) -> (usize, u64) {
    let rows = match db.recording_sessions() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(%err, "读取待恢复会话失败");
            return (0, 0);
        }
    };
    let mut recovered = 0usize;
    let mut bytes = 0u64;
    for (id, file_path) in rows {
        let path = PathBuf::from(&file_path);
        let has_parts = !segment::collect_parts(&path).is_empty();
        if !has_parts {
            // 没有分片但成品已存在：可能上次是在合并之后、写库之前被杀。
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                if meta.len() > 0 {
                    let _ = db.update_session_file(&id, &file_path, meta.len() as i64);
                    recovered += 1;
                    bytes += meta.len();
                }
            }
            continue;
        }
        let (target, size) = finish_file(&path, true).await;
        if size <= 0 {
            continue;
        }
        let _ = db.update_session_file(&id, &target.display().to_string(), size);
        tracing::info!(id = %id, file = %target.display(), size, "恢复崩溃遗留的录像");
        recovered += 1;
        bytes += size as u64;
    }
    (recovered, bytes)
}

fn emit_notice(app: &AppHandle, kind: &str, room_id: &str, host_name: &str, detail: Option<String>) {
    let notice = AppNotice {
        kind: kind.to_string(),
        room_id: room_id.to_string(),
        host_name: host_name.to_string(),
        detail,
    };
    let _ = app.emit(EVENT_APP_NOTICE, notice);
}

/// 选线：优先 H.264（FLV 兼容最好），其次第一个有地址的候选。
fn pick_candidate<'a>(
    groups: &'a [StreamGroup],
    preferred: Option<&str>,
) -> Option<(&'a crate::platform::StreamCandidate, &'a StreamGroup)> {
    // 用户指定了线路就只在那条线上挑；线路上没有可用地址时再退回全局最优，
    // 否则「手动选了一条挂掉的线路」会让录制直接启动失败，反而更难用。
    if let Some(name) = preferred {
        if let Some(group) = groups.iter().find(|group| group.name == name) {
            if let Some(picked) = pick_in_group(group) {
                return Some(picked);
            }
            tracing::warn!(line = name, "指定线路没有可用地址，改用其它线路");
        }
    }
    let mut fallback = None;
    for group in groups {
        for candidate in &group.candidates {
            if candidate.url.is_none() {
                continue;
            }
            if candidate.codec == Some(crate::platform::Codec::H264) {
                return Some((candidate, group));
            }
            fallback.get_or_insert((candidate, group));
        }
    }
    fallback
}

fn pick_in_group(group: &StreamGroup) -> Option<(&crate::platform::StreamCandidate, &StreamGroup)> {
    let mut fallback = None;
    for candidate in &group.candidates {
        if candidate.url.is_none() {
            continue;
        }
        if candidate.codec == Some(crate::platform::Codec::H264) {
            return Some((candidate, group));
        }
        fallback.get_or_insert((candidate, group));
    }
    fallback
}

fn platform_label(platform: &str) -> &'static str {
    match platform {
        "bilibili" => "哔哩哔哩",
        "douyu" => "斗鱼",
        "huya" => "虎牙",
        "douyin" => "抖音",
        _ => "其他",
    }
}

/// 生成录制文件路径：媒体库/平台/主播/[时间][主播][标题].mp4
pub fn session_path(
    root: &Path,
    platform: &str,
    host: &str,
    title: &str,
    started_at: i64,
    ext: &str,
) -> PathBuf {
    let time = chrono::DateTime::from_timestamp(started_at, 0)
        .unwrap_or_else(chrono::Utc::now)
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H-%M-%S")
        .to_string();
    let host = sanitize(host, "未知主播", 40);
    let title = sanitize(title, "未命名直播", 60);
    root.join(platform)
        .join(&host)
        .join(format!("[{time}][{host}][{title}].{ext}"))
}

/// 跨平台文件名清洗：去掉 Windows/macOS 都不接受的字符，压缩空白，限制长度。
pub fn sanitize(input: &str, fallback: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_space = false;
    for ch in input.chars() {
        let mapped = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        };
        if mapped.is_whitespace() {
            if last_was_space {
                continue;
            }
            last_was_space = true;
            out.push(' ');
        } else {
            last_was_space = false;
            out.push(mapped);
        }
    }
    let trimmed = out.trim().trim_matches('.').trim();
    let mut result: String = trimmed.chars().take(max_chars).collect();
    if result.is_empty() {
        result = fallback.to_string();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filters_illegal_chars() {
        assert_eq!(sanitize("a/b:c*d?e\"f<g>h|i", "x", 40), "a_b_c_d_e_f_g_h_i");
        assert_eq!(sanitize("  多   空格  ", "x", 40), "多 空格");
        assert_eq!(sanitize("", "兜底", 40), "兜底");
        assert_eq!(sanitize("...", "兜底", 40), "兜底");
    }

    #[test]
    fn preferred_line_wins_and_falls_back() {
        use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};
        let candidate = |name: &str, url: Option<&str>| StreamCandidate {
            id: name.into(),
            name: name.into(),
            weight: 1.0,
            format: StreamFormat::Flv,
            codec: Some(Codec::H264),
            width: None,
            height: None,
            fps: None,
            bitrate_kbps: None,
            url: url.map(|value| value.to_string()),
            headers: Default::default(),
            attrs: Default::default(),
        };
        let groups = vec![
            StreamGroup {
                name: "线路A".into(),
                priority: 0,
                candidates: vec![candidate("a", Some("http://a"))],
            },
            StreamGroup {
                name: "线路B".into(),
                priority: 1,
                candidates: vec![candidate("b", Some("http://b"))],
            },
        ];
        let (picked, group) = pick_candidate(&groups, Some("线路B")).expect("应能选到指定线路");
        assert_eq!(group.name, "线路B");
        assert_eq!(picked.url.as_deref(), Some("http://b"));

        // 指定线路没有可用地址时退回全局最优，而不是直接失败。
        let broken = vec![
            StreamGroup {
                name: "线路A".into(),
                priority: 0,
                candidates: vec![candidate("a", Some("http://a"))],
            },
            StreamGroup {
                name: "线路B".into(),
                priority: 1,
                candidates: vec![candidate("b", None)],
            },
        ];
        let (picked, group) = pick_candidate(&broken, Some("线路B")).expect("应退回其它线路");
        assert_eq!(group.name, "线路A");
        assert_eq!(picked.url.as_deref(), Some("http://a"));
    }

    #[test]
    fn session_path_shape() {
        let path = session_path(Path::new("/tmp/lib"), "哔哩哔哩", "主播", "标题", 1_760_000_000, "flv");
        let text = path.display().to_string();
        assert!(text.starts_with("/tmp/lib/哔哩哔哩/主播/["));
        assert!(text.ends_with("][主播][标题].flv"));
    }

    #[test]
    fn manual_stop_suppresses_auto_start() {
        let db = Arc::new(Db::open(Path::new(":memory:")).expect("内存库"));
        let recorder = Recorder::new(db, PathBuf::from("/tmp"), None);
        assert!(!recorder.auto_suppressed("r1"));
        recorder
            .suppressed
            .lock()
            .expect("锁")
            .insert("r1".to_string());
        assert!(recorder.auto_suppressed("r1"));
        recorder.resume_auto("r1");
        assert!(!recorder.auto_suppressed("r1"));
    }
}
