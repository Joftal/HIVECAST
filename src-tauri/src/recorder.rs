//! 录制引擎：一个房间一个 ffmpeg 进程，分段 mpegts 直录，收尾合成源封装母带，
//! 再按设置做派生后处理（source / convert / custom）。
//!
//! 为什么分段（M7.1）：进程被强杀 / 断电时，单文件 FLV 的索引与尾部会丢，
//! 整场录像可能直接不可播；segment muxer 每 5 分钟落一个独立 ts，
//! 崩溃最多丢最后一片，下次启动还能把已有分片合成 mp4。
//!
//! 状态机：idle → recording → (stopped | stream_ended | failed)。
//! 进程退出由 1 秒轮询发现（tokio::process 的 wait 不能与 stop 并存取同一子进程）。
//! 意外断流（不是用户停、也不是平台下播）会先在同一个会话里续录最多 3 次，
//! 分片接着编号，避免一次抖动就把整场切成两条历史；彻底失败才收尾。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

use hivecast_ipc::{AppNotice, QualitySegment, RecordSession, RecordingInfo, RoomRecord};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::Child;

use crate::broker::StreamBroker;
use crate::danmaku::{DanmakuHub, RecordWriter};
use crate::db::Db;
use crate::disk;
use crate::error::AppError;
use crate::ffmpeg;
use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};
use crate::process::{send_graceful, wait_exit};
use crate::segment;

/// 进程存活不足这个时长就退出，视为「秒退」，需要换线路重试。
const EARLY_EXIT: Duration = Duration::from_secs(12);
/// 连续失败后的冷却时间，避免对着同一个坏房间疯狂重试。
const FAILURE_COOLDOWN: Duration = Duration::from_secs(60);
/// 断流自动续录的最大次数：网络抖动通常一次就够，连续 3 次说明源头真断了。
const MAX_RESTARTS: u32 = 3;
/// 一个会话里因「长时间没有新数据」重启 ffmpeg 的上限。
const MAX_STALL_RESTARTS: u32 = 2;
/// 录制文件连续这么久一个字节都没涨，判定直播源假死（S4）。
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// 下播需要连续两次成功检查都报离线（S1）。中间只要有一次在播就重新计数，
/// 不再限制两次之间的间隔：检测间隔设到 3 分钟以上时，时间窗会让计数永远
/// 攒不够，平台报了离线也停不了录（B5）。
/// 自然结束后的自动开录静默期（S2）；出现新场次会提前解除。
const ENDED_BLOCK: Duration = Duration::from_secs(600);
/// 干净 EOF 后最多复核几次「平台是否仍显示在播」（R1）。
const MAX_EOF_RETRIES: u32 = 2;
/// 连续失败多少次后进入长冷却（R6/R7）。
const FAILED_STREAK_LIMIT: u32 = 3;
/// 连续失败后的长冷却时长：源真的不可用时，别再每分钟开一条失败会话。
const FAILED_STREAK_COOLDOWN: Duration = Duration::from_secs(300);
/// 连续多少次「开录动作本身失败」后进入冷却（G5）。
const START_FAILURE_LIMIT: u32 = 3;
/// 开录连续失败后的冷却时长：鉴权过期、取流地址解析失败这类问题，
/// 每个检测周期重试只会持续打平台、刷日志，冷却后再说。
const START_FAILURE_COOLDOWN: Duration = Duration::from_secs(300);
/// 稳定录制满这么久就把续录预算清零：一场录了几个小时的直播，
/// 不该因为中途抖动过 3 次就再也续不上（A2）。
const HEALTHY_RESET: Duration = Duration::from_secs(30 * 60);
/// 平台状态查不到时间隔这么久再问一次，避免把限流当成下播（A4）。
const PLATFORM_RECHECK_DELAY: Duration = Duration::from_secs(20);
pub const EVENT_ROOMS_CHANGED: &str = "rooms://changed";
/// 启动后的崩溃恢复合并完成（I2：合并在后台跑，媒体库要重新取列表）。
pub const EVENT_MEDIA_CHANGED: &str = "media://changed";
/// 录制状态播报事件：前端收到后弹系统通知（文案走 i18n）。
pub const EVENT_APP_NOTICE: &str = "app://notice";

/// 视频分割策略：录制时只切分片，收尾时再按这些边界合成多个片段文件。
#[derive(Debug, Clone)]
struct SplitPolicy {
    on_title: bool,
    max_duration: Option<Duration>,
    max_size: Option<u64>,
    /// ffmpeg 分片长度：分割边界只能落在分片边界上。
    segment_secs: u32,
}

impl SplitPolicy {
    fn from_settings(settings: &hivecast_ipc::Settings) -> Self {
        let max_duration = (settings.split_max_duration_sec > 0)
            .then(|| Duration::from_secs(u64::from(settings.split_max_duration_sec)));
        let max_size = (settings.split_max_size_mb > 0)
            .then(|| u64::from(settings.split_max_size_mb) * 1024 * 1024);
        // 分片越短边界越准，但 60 秒以下会让分片数量暴涨；300 秒是历史默认。
        // 只按时长分割时尽量让分片长度整除目标，避免 400 秒切成 300+100 之外的
        // 600 秒超限；同时开标题/大小分割时用 15..=60 秒的细粒度，保证标题和
        // 大小边界足够贴近，且时长上限不会被 60 秒粒度顶到 120 秒才切。
        let duration_segment = max_duration.map(segment_for_duration);
        let segment_secs = if settings.split_on_title || max_size.is_some() {
            max_duration.map_or(60, |limit| segment_for_duration_between(limit, 15, 60))
        } else {
            duration_segment.unwrap_or(segment::SEGMENT_SECONDS)
        };
        Self {
            on_title: settings.split_on_title,
            max_duration,
            max_size,
            segment_secs: segment_secs.max(15),
        }
    }
}

/// 选流偏好：画质 > 编码 > 格式，都匹配不上时仍返回一个可用候选。
#[derive(Debug, Clone, Copy)]
struct StreamPrefs {
    qn: u32,
    codec: Option<Codec>,
    format: Option<StreamFormat>,
}

impl StreamPrefs {
    fn from_settings(settings: &hivecast_ipc::Settings, qn: u32) -> Self {
        let codec = match settings.record_codec.as_str() {
            "h264" => Some(Codec::H264),
            "h265" => Some(Codec::H265),
            _ => None,
        };
        let format = match settings.record_source_format.as_str() {
            "flv" => Some(StreamFormat::Flv),
            "hls" => Some(StreamFormat::Hls),
            "ts" => Some(StreamFormat::HttpTs),
            _ => None,
        };
        Self { qn, codec, format }
    }
}

/// 续录的选流偏好：钉住首段实际拿到的编码与源流格式，画质仍按用户档位。
fn respawn_prefs(
    qn: u32,
    prefs: StreamPrefs,
    codec: Option<Codec>,
    format: StreamFormat,
) -> StreamPrefs {
    StreamPrefs {
        qn,
        codec: codec.or(prefs.codec),
        format: Some(format),
    }
}

/// 只按时长分割时，选一个 60..=300 秒的分片长度，使总时长尽量贴近上限。
fn segment_for_duration(limit: Duration) -> u32 {
    let secs = limit.as_secs().max(1);
    if secs <= 300 {
        return secs.clamp(60, 300) as u32;
    }
    let mut best = (300u32, u64::MAX);
    for candidate in 60..=300u32 {
        let step = u64::from(candidate);
        let over = secs.div_ceil(step) * step - secs;
        if over < best.1 || (over == best.1 && candidate > best.0) {
            best = (candidate, over);
        }
    }
    best.0
}

/// 标题/大小分割同时开启时，分片还要尽量对齐时长上限。和
/// `segment_for_duration` 的区别是允许 15 秒的小分片：90 秒上限用 45 秒分片
/// 正好切在 90 秒，而 60 秒分片会把第一次边界推到 120 秒。
fn segment_for_duration_between(limit: Duration, min: u32, max: u32) -> u32 {
    let secs = limit.as_secs().max(1);
    let min = min.max(1);
    let max = max.max(min);
    if secs <= u64::from(min) {
        return min;
    }
    let mut best = (max, u64::MAX);
    for candidate in min..=max {
        let step = u64::from(candidate);
        let over = secs.div_ceil(step) * step - secs;
        if over < best.1 || (over == best.1 && candidate > best.0) {
            best = (candidate, over);
        }
    }
    best.0
}

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
    /// 已续录的次数，达到 MAX_RESTARTS 后不再重开
    restarts: u32,
    /// 本场直播的开播时间（平台口径）：用来区分「同一场」与「新一场」
    live_started_at: Option<i64>,
    /// 已落盘字节数与最后一次增长的时刻，用来发现「连着但不给数据」
    last_bytes: u64,
    last_growth: Instant,
    /// 假死重启次数；只要文件重新增长就清零
    stalls: u32,
    /// 「平台仍显示在播」的干净 EOF 复核次数（R1）
    eof_retries: u32,
    /// 当前实际画质档位；续录拿到不同档位时随之更新
    qn: u32,
    /// 本场目标档位：默认原画；Cookie 恢复后升级也以它为准
    target_qn: u32,
    /// 续录/升级请求锚点：始终等于 target_qn
    plan_qn: u32,
    /// 画质升级正在进行：watcher 必须跳过主动停进程的退出事件
    upgrading: bool,
    /// 本场成功升级的次数（不计入 restarts 上限）
    upgrades: u32,
    /// 本场各时段的实际画质；只在档位变化时追加
    quality_history: Vec<QualitySegment>,
    line: Option<String>,
    /// 本场开录时的编码/源流格式偏好：续录必须与首段保持一致，
    /// 否则同一场录像会混入不同编码/封装的分片。
    prefs: StreamPrefs,
    /// 首段实际拿到的编码与源流格式：续录优先选同一种，避免同一场
    /// 的分片从 H.264 切成 H.265 后 mp4 合并/播放异常。
    codec: Option<Codec>,
    format: StreamFormat,
    /// 本场开录时的分割策略与标题快照；标题变化只在收尾时切片段。
    split: SplitPolicy,
    title: String,
    title_changes: Vec<i64>,
    title_checked: Instant,
    /// ffmpeg 最近几十行 stderr：默认只进 debug，异常收尾时把尾部打进 WARN。
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// 开录时快照的后处理配置：中途改设置只影响新会话。
    post: crate::post::PostConfig,
    /// 单房间通知覆盖，开录时解析成最终布尔值。
    notify_live: bool,
    notify_error: bool,
}

/// 录制命名策略：模板 + 两个开关，设置页保存后立即对新录制生效。
#[derive(Debug, Clone)]
pub struct Naming {
    pub template: String,
    pub strip_emoji: bool,
    pub group_dirs: bool,
}

impl Default for Naming {
    fn default() -> Self {
        Self {
            template: crate::filename::DEFAULT_TEMPLATE.into(),
            strip_emoji: false,
            group_dirs: true,
        }
    }
}

/// 上一场自然结束：(结束时刻, 那场的开播时间, 是否平台确认离线)。
type EndedRecord = (Instant, Option<i64>, bool);

pub struct Recorder {
    db: Arc<Db>,
    /// 媒体库根目录：设置页可以改，开录时现取，避免改完要重启。
    media_root: std::sync::RwLock<PathBuf>,
    /// 文件名模板与清洗开关，同样开录时现取。
    naming: std::sync::RwLock<Naming>,
    inner: Mutex<HashMap<String, Active>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// 最近一次失败原因，展示在监控页行内
    errors: Mutex<HashMap<String, String>>,
    /// 最近一次「检测失败」的文案：同一句失败只告警/推事件一次，
    /// 平台一直挂着坏房间时不该每拍都刷前端（F1）。
    /// 与 errors 分开记：开录失败（例如磁盘不足）也写 errors，
    /// 成功检测不能顺手把它清掉，否则那条告警会重新变成每拍一条（C7）。
    detect_errors: Mutex<HashMap<String, String>>,
    /// 每房间一把开录锁：监控循环、添加房间、手动开录可能同时触发 start，
    /// 只有先拿到锁的那次能真正拉起 ffmpeg，其余走到里面会发现已有进程直接返回。
    start_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// 用户手动停录过的房间 → 停录时的开播时间。
    /// 同一场直播内不再自动开录，换了新场次自动放行（S3）。
    suppressed: Mutex<HashMap<String, Option<i64>>>,
    /// 平台确认离线后才停录的房间（A8）：这类收尾是「确认下播」，
    /// 不该再挡 10 分钟自动开录。
    offline_stops: Mutex<HashSet<String>>,
    /// 上一场自然结束：房间 →（结束时刻, 那场的开播时间, 是否平台确认离线）。
    /// 平台下播状态滞后时先别再开一条秒退的会话（S2）。
    /// 用单调时钟：改系统时间不该让静默窗口提前结束或永不结束（R4）。
    ended: Mutex<HashMap<String, EndedRecord>>,
    /// 连续「成功检查且离线」的次数，给下播做二次确认（S1）。
    /// 只记次数不记时间：间隔再大也该能攒够两次（B5）。
    offline: Mutex<HashMap<String, u32>>,
    /// 这一场是不是真的看到过「平台离线」。用来区分「下播后又报在播」是同一场
    /// 的延迟还是新一场（H7）：斗鱼不提供开播时间，只能靠这个信号。
    offline_seen: Mutex<HashSet<String>>,
    /// 连续「非正常收尾」的次数与最后一次时刻（R6/R7）。
    /// 源不可用时监控每轮都会重开，这里攒够 3 次就进长冷却。
    hard_failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// 连续「开录请求本身失败」的次数与最后一次时刻（G5）。
    /// 和 hard_failures 分开：那条是「录到一半断了」，这条是「根本没录起来」。
    start_failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// 每房间升级单飞锁：同一房间的凭据变更与续录可能同时触发探测。
    upgrade_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// 正在开录的房间：停录请求要能看见「ffmpeg 已经在起、但还没进 active」的这段窗口（C5）。
    starting: Mutex<HashSet<String>>,
    /// 开录在途时被取消自动录制的房间：解析线路要几秒，期间关掉自动录制也必须在拉 ffmpeg 前拦住。
    start_cancelled: Mutex<HashSet<String>>,
    /// 开录在途时收到的停录请求：房间 → 原因。等 start 把会话挂进 active 再消费。
    pending_stop: Mutex<HashMap<String, String>>,
    /// 正在收尾的房间：转封装可能持续几十秒，期间不该被监控当成「没在录」重新拉起（C4）。
    finalizing: Mutex<HashSet<String>>,
    /// 应用正在退出：此后拒绝任何新开录，避免退出窗口里再起一条会话（C1）。
    shutting_down: AtomicBool,
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
            naming: std::sync::RwLock::new(Naming::default()),
            inner: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            detect_errors: Mutex::new(HashMap::new()),
            start_locks: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashSet::new()),
            start_cancelled: Mutex::new(HashSet::new()),
            pending_stop: Mutex::new(HashMap::new()),
            finalizing: Mutex::new(HashSet::new()),
            shutting_down: AtomicBool::new(false),
            suppressed: Mutex::new(HashMap::new()),
            offline_stops: Mutex::new(HashSet::new()),
            ended: Mutex::new(HashMap::new()),
            offline: Mutex::new(HashMap::new()),
            offline_seen: Mutex::new(HashSet::new()),
            hard_failures: Mutex::new(HashMap::new()),
            start_failures: Mutex::new(HashMap::new()),
            upgrade_locks: Mutex::new(HashMap::new()),
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
        let Some(thumbs) = self.thumbs.read().expect("缩略图状态被污染").clone() else {
            return;
        };
        // 时长与最终路径都以库里的为准：收尾时可能已经从分片合成成 mp4。
        let Ok(Some(session)) = self.db.get_session(session_id) else {
            return;
        };
        let Some(path) = session.file_path.filter(|one| !one.is_empty()) else {
            return;
        };
        thumbs.ensure_tracked(app.clone(), vec![(session.id, path, session.duration_ms)]);
    }

    pub fn media_root(&self) -> PathBuf {
        self.media_root.read().expect("媒体目录被污染").clone()
    }

    /// 设置页改目录后立即对新录制生效；正在录制的进程仍写旧路径。
    pub fn set_media_root(&self, root: PathBuf) {
        *self.media_root.write().expect("媒体目录被污染") = root;
    }

    pub fn naming(&self) -> Naming {
        self.naming.read().expect("命名策略被污染").clone()
    }

    /// 设置页保存后调用；正在录制的文件不受影响。
    pub fn set_naming(&self, template: String, strip_emoji: bool, group_dirs: bool) {
        *self.naming.write().expect("命名策略被污染") = Naming {
            template,
            strip_emoji,
            group_dirs,
        };
    }

    /// 当前正在录制的路数。
    pub fn active_count(&self) -> usize {
        self.inner.lock().expect("录制状态被污染").len()
    }

    /// 正在开录、但还没挂进 active 的会话数。退出流程用它判断是否还有开录在途。
    pub(crate) fn starting_count(&self) -> usize {
        self.starting.lock().expect("开录标记被污染").len()
    }

    /// 退出流程开始：此后监控不再检测、start 一律拒绝（C1）。
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// 这一路是否正在收尾（分片合成 / 写历史还没走完，C4）。
    pub fn is_finalizing(&self, room_id: &str) -> bool {
        self.finalizing
            .lock()
            .expect("收尾标记被污染")
            .contains(room_id)
    }

    /// 自动录制在开录在途时被关闭：给这条在途请求打取消标记，拉 ffmpeg 前拦下。
    pub fn cancel_start(&self, room_id: &str) {
        let mut cancelled = self.start_cancelled.lock().expect("开录取消标记被污染");
        if self
            .starting
            .lock()
            .expect("开录标记被污染")
            .contains(room_id)
        {
            cancelled.insert(room_id.to_string());
            tracing::info!(room_id, "自动录制已关闭，取消在途开录");
        } else {
            cancelled.remove(room_id);
        }
    }

    fn is_start_cancelled(&self, room_id: &str) -> bool {
        self.start_cancelled
            .lock()
            .expect("开录取消标记被污染")
            .contains(room_id)
    }

    /// 登记「这一路正在开录」：stop 靠它判断停录请求是在 start 之前还是之中到达（C5）。
    fn begin_start<'a>(&'a self, room_id: &str) -> StartGuard<'a> {
        self.start_cancelled
            .lock()
            .expect("开录取消标记被污染")
            .remove(room_id);
        self.starting
            .lock()
            .expect("开录标记被污染")
            .insert(room_id.to_string());
        StartGuard {
            recorder: self,
            room_id: room_id.to_string(),
            inserted: false,
        }
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
        self.errors.lock().expect("错误状态被污染").remove(room_id);
    }

    /// 记下这一路最近一次检测失败的文案，返回「与上次不同」。
    ///
    /// 调用方只在返回 true 时告警并推前端事件：同一句失败连着一小时
    /// 也只产生一条日志、一次全量刷新（F1）。
    pub fn note_detect_error(&self, room_id: &str, message: &str) -> bool {
        let mut map = self.detect_errors.lock().expect("检测错误状态被污染");
        if map.get(room_id).map(String::as_str) == Some(message) {
            return false;
        }
        map.insert(room_id.to_string(), message.to_string());
        true
    }

    /// 检测恢复正常：清掉检测错误，返回是否真的清掉了（决定要不要推一次事件）。
    pub fn clear_detect_error(&self, room_id: &str) -> bool {
        self.detect_errors
            .lock()
            .expect("检测错误状态被污染")
            .remove(room_id)
            .is_some()
    }

    /// 房间是不是已经被删除（软删也算）。
    ///
    /// 录制侧要自己看一眼：删除房间时如果停录请求恰好丢了，watcher 会一直续录
    /// 一场用户已经不关心的直播，最后还给这条已删房间弹「直播已结束」（F2）。
    pub fn room_removed(&self, room_id: &str) -> bool {
        self.db.room_removed(room_id).unwrap_or(false)
    }

    /// 手动停录后是否仍处于「不要再自动开录」状态。
    ///
    /// live_started 是本轮检查到的开播时间：与停录时不同就说明已经换了一场直播，
    /// 标记自动失效（S3：下播后快速重开不会被上一场的停录卡住）。
    pub fn auto_suppressed(&self, room_id: &str, live_started: Option<i64>) -> bool {
        let mut map = self.suppressed.lock().expect("停录标记被污染");
        match map.get(room_id).copied() {
            None => false,
            Some(stored) => {
                if let (Some(stored_at), Some(now_at)) = (stored, live_started) {
                    if stored_at != now_at {
                        map.remove(room_id);
                        return false;
                    }
                }
                true
            }
        }
    }

    /// 上一场刚自然结束、平台却还显示在播：先别自动开录（S2）。
    ///
    /// 解除条件（任一）：成功看到一次离线（resume_auto）、平台给出新的开播时间、
    /// 或者过了 ENDED_BLOCK_SECS 的兜底窗口。
    pub fn auto_blocked(&self, room_id: &str, live_started: Option<i64>) -> bool {
        let mut map = self.ended.lock().expect("结束标记被污染");
        let Some((at, stored, confirmed)) = map.get(room_id).copied() else {
            return false;
        };
        if at.elapsed() > ENDED_BLOCK {
            map.remove(room_id);
            return false;
        }
        if let (Some(stored_at), Some(now_at)) = (stored, live_started) {
            if stored_at != now_at {
                map.remove(room_id);
                return false;
            }
        }
        // 上一场是「平台确认离线」才收的尾，现在平台又说在播（开播时间没变）：
        // 十有八九是那两次离线都是误报，别真的把这一场晾满 10 分钟（A8）。
        // 真下播时重开只会换来一次秒退，失败计数会把它挡住。
        if confirmed && live_started.is_some() {
            tracing::info!(room_id, "确认下播后平台又报在播，解除自动开录静默");
            map.remove(room_id);
            return false;
        }
        true
    }

    /// 平台确认离线后才停的录：标记这一场是「确认下播」而不是误停（A8）。
    pub fn note_offline_stop(&self, room_id: &str) {
        self.offline_stops
            .lock()
            .expect("离线停录标记被污染")
            .insert(room_id.to_string());
    }

    /// 记下「这一场自然结束了」，用于挡住平台状态滞后引起的二次开录。
    pub fn mark_ended(&self, room_id: &str, live_started: Option<i64>) {
        // 只有监控侧确认过离线才消费这个标记；误停/手动停不该留下「确认下播」。
        let confirmed = self
            .offline_stops
            .lock()
            .expect("离线停录标记被污染")
            .remove(room_id);
        self.ended.lock().expect("结束标记被污染").insert(
            room_id.to_string(),
            (Instant::now(), live_started, confirmed),
        );
    }

    /// 成功看到离线、或重新开录之后，清掉停录抑制与结束标记。
    pub fn resume_auto(&self, room_id: &str) {
        self.suppressed
            .lock()
            .expect("停录标记被污染")
            .remove(room_id);
        self.ended.lock().expect("结束标记被污染").remove(room_id);
    }

    /// 确认离线后留个记号：下一次再看到在播，就该按新一场处理（H7）。
    pub fn note_offline_seen(&self, room_id: &str) {
        self.offline_seen
            .lock()
            .expect("离线标记被污染")
            .insert(room_id.to_string());
    }

    /// 离线之后又报在播：清掉上一场的结束静默与手动停录抑制。
    ///
    /// 只有「先看到离线、再看到在播」才算新一场，避免平台单次误报在播
    /// 就把刚结束的 10 分钟静默解掉（那是给下播延迟留的缓冲）。
    pub fn new_session_after_offline(&self, room_id: &str) -> bool {
        let seen = self
            .offline_seen
            .lock()
            .expect("离线标记被污染")
            .remove(room_id);
        if !seen {
            return false;
        }
        let had_ended = self
            .ended
            .lock()
            .expect("结束标记被污染")
            .remove(room_id)
            .is_some();
        let had_suppressed = self
            .suppressed
            .lock()
            .expect("停录标记被污染")
            .remove(room_id)
            .is_some();
        // 只有真的解掉过什么才值得打日志：正常每场开播都会走到这里一次。
        had_ended || had_suppressed
    }

    /// 记录一次「成功检查且离线」；连续两次才认为主播真的下播（S1）。
    ///
    /// 一次接口抖动或字段缺失会把状态误报成离线，单次就停录会把一场健康录像
    /// 切成两条历史。返回 true 表示可以停录；返回后计数清零重新计。
    /// 「连续」只由调用方保证：查到在播就 clear_offline（B5）。
    pub fn confirm_offline(&self, room_id: &str) -> bool {
        let mut map = self.offline.lock().expect("离线确认被污染");
        let count = map.entry(room_id.to_string()).or_insert(0);
        *count += 1;
        if *count >= 2 {
            map.remove(room_id);
            true
        } else {
            false
        }
    }

    /// 成功看到在播、或本来就没在录：清掉离线计数。
    pub fn clear_offline(&self, room_id: &str) {
        self.offline.lock().expect("离线确认被污染").remove(room_id);
    }

    pub fn is_recording(&self, room_id: &str) -> bool {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .contains_key(room_id)
    }

    /// 画质升级正在主动停旧进程：watcher 此时不能把退出当成断流。
    pub fn is_upgrading(&self, room_id: &str) -> bool {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .is_some_and(|active| active.upgrading)
    }

    /// 这一路是不是已经在收尾了（stop_reason 已置、ffmpeg 还没退出）。
    /// 监控侧据此避免每秒重复发一遍停止请求（B6）。
    pub fn stopping(&self, room_id: &str) -> bool {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .is_some_and(|active| active.stop_reason.is_some())
    }

    /// 已经发出去的停录原因（disk_lost / disk_full / stopped / app_exit）。
    /// 假死看门狗用它判断「这一路是不是早就在收了」，别把 abort 改判成 stalled（H4）。
    pub fn stop_reason(&self, room_id: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .and_then(|active| active.stop_reason.clone())
    }

    /// 正在开录、还没挂进 active 的房间（H6）。退出流程要看到它们，
    /// 否则「开录中」点退出会被当成没有任务而直接杀掉。
    pub fn starting_rooms(&self) -> Vec<String> {
        self.starting
            .lock()
            .expect("开录标记被污染")
            .iter()
            .cloned()
            .collect()
    }

    /// 每一路正在录制的落盘目录，磁盘红线按它逐路检查（R5）。
    ///
    /// 只看「当前媒体目录」不够：用户中途换过目录后，正在录的那一路还写在旧卷上，
    /// 旧卷写满时按新目录判断会漏掉，最后只能等 ffmpeg 报错。
    pub fn active_targets(&self) -> Vec<(String, String, PathBuf)> {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .values()
            .map(|active| {
                (
                    active.recording.room_id.clone(),
                    active.host_name.clone(),
                    PathBuf::from(&active.recording.file_path),
                )
            })
            .collect()
    }

    /// 文件最近是否还在增长：停录前的交叉验证（R2）。
    ///
    /// 平台偶尔连续两次误报离线，此时分片其实还在写；单看平台状态会把一场
    /// 健康的录像切断。这里顺带刷新增长基线，避免和假死看门狗互相干扰。
    pub fn flowing(&self, room_id: &str) -> bool {
        let mut inner = self.inner.lock().expect("录制状态被污染");
        let Some(active) = inner.get_mut(room_id) else {
            return false;
        };
        let bytes = segment::parts_total(Path::new(&active.recording.file_path));
        if bytes > active.last_bytes {
            active.last_bytes = bytes;
            active.last_growth = Instant::now();
        }
        active.last_growth.elapsed() < STALL_TIMEOUT
    }

    /// 连续失败攒够次数后是否处于长冷却（R6/R7）。
    pub fn in_failure_cooldown(&self, room_id: &str) -> bool {
        let mut map = self.hard_failures.lock().expect("失败计数被污染");
        let Some((streak, at)) = map.get(room_id).copied() else {
            return false;
        };
        if at.elapsed() > FAILED_STREAK_COOLDOWN {
            map.remove(room_id);
            return false;
        }
        streak >= FAILED_STREAK_LIMIT
    }

    /// 记一次非正常收尾，返回连续次数（用于决定要不要发通知）。
    fn note_hard_failure(&self, room_id: &str) -> u32 {
        let mut map = self.hard_failures.lock().expect("失败计数被污染");
        let entry = map
            .entry(room_id.to_string())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() > FAILED_STREAK_COOLDOWN {
            entry.0 = 0;
        }
        entry.0 += 1;
        entry.1 = Instant::now();
        entry.0
    }

    fn clear_hard_failure(&self, room_id: &str) {
        self.hard_failures
            .lock()
            .expect("失败计数被污染")
            .remove(room_id);
    }

    fn eof_retries(&self, room_id: &str) -> u32 {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .map(|active| active.eof_retries)
            .unwrap_or(u32::MAX)
    }

    fn bump_eof_retry(&self, room_id: &str) {
        if let Some(active) = self.inner.lock().expect("录制状态被污染").get_mut(room_id) {
            active.eof_retries += 1;
        }
    }

    /// 进程退出后问一次平台：在播更像断流，离线才是真下播（R1/A11）。
    ///
    /// 用 fresh 取信息（跳过 15 秒缓存），否则缓存里那句「在播」正好会把
    /// 真正的下播误判成断流。查不到 / 被限流返回 Unknown，绝不能当成离线
    /// ——那正是限流时把一场健康录像「干净收尾」的原因（A4）。
    async fn platform_state(&self, broker: &StreamBroker, room_id: &str) -> PlatformState {
        let room = match self.db.get_room(room_id) {
            Ok(Some(room)) => room,
            // 房间已经被移除：没必要续录，按离线收尾。
            _ => return PlatformState::Offline,
        };
        if self.db.room_removed(room_id).unwrap_or(false) {
            return PlatformState::Offline;
        }
        match broker
            .room_info_fresh(&crate::monitor::room_ref(&room))
            .await
        {
            Ok(info) if info.is_live => PlatformState::Live,
            Ok(_) => PlatformState::Offline,
            Err(err) => {
                tracing::warn!(%err, room_id, "查询平台状态失败");
                PlatformState::Unknown
            }
        }
    }

    /// 续录失败时问一次平台：只有明确离线才算「确认下播」。
    ///
    /// 在播 / 查不到都按未确认收尾（B1）：平台明明还在播时，既不该弹
    /// 「直播已结束」，也不该写 10 分钟静默挡住下一次自动开录。
    async fn platform_confirmed_offline(&self, broker: &StreamBroker, room_id: &str) -> bool {
        matches!(
            self.platform_state(broker, room_id).await,
            PlatformState::Offline
        )
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
        self.start_inner(app, broker, room, false, true).await
    }

    /// 监控专用入口：拿锁后复查房间是否刚被移除或暂停（R3）。
    ///
    /// 监控用的是每轮开始时的房间快照；从快照到真正拉起 ffmpeg 之间，用户
    /// 可能在界面上移除房间或点「停止监控」。重新读一次库，避免给已经移除的
    /// 房间留下一个没有停止入口的「幽灵录制」。
    pub async fn start_checked(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: &StreamBroker,
        room: &RoomRecord,
    ) -> Result<RecordingInfo, AppError> {
        // 连续开录失败后先冷一会儿：鉴权过期 / 取流接口异常时每拍重试只会打平台（G5）。
        if self.in_start_cooldown(&room.id) {
            return Err(AppError::User("连续开录失败，已暂停自动重试 5 分钟".into()));
        }
        let result = self.start_inner(app, broker, room, true, true).await;
        match &result {
            Ok(_) => self.clear_start_failure(&room.id),
            Err(err) if start_failure_is_counted(err) => self.note_start_failure(&room.id),
            Err(_) => {}
        }
        result
    }

    async fn start_inner(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: &StreamBroker,
        room: &RoomRecord,
        recheck: bool,
        notify_started: bool,
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

        if recheck {
            let removed = self.db.room_removed(&room.id).unwrap_or(false);
            let fresh = self.db.get_room(&room.id).ok().flatten();
            let paused = fresh.as_ref().is_some_and(|room| room.monitor_paused);
            if !monitor_start_allowed(removed, paused) {
                tracing::info!(room = %room.host_name, removed, paused, "监控开录复查未通过");
                return Err(AppError::User("房间已移除或监控已暂停".into()));
            }
            if let Some(fresh) = fresh.as_ref() {
                if !fresh.auto_record {
                    tracing::info!(room = %room.host_name, "监控开录复查未通过：自动录制已关闭");
                    return Err(AppError::User("自动录制已关闭".into()));
                }
            } else {
                return Err(AppError::User("房间不存在".into()));
            }
        }

        if let Some(existing) = self.active(&room.id) {
            tracing::debug!(room_id = %room.id, session_id = %existing.session_id, "房间已在录制，复用现有会话");
            return Ok(existing);
        }
        // 应用已经在退出：退出流程会收掉所有会话，此刻再起一条只会立刻被
        // 第二次收尾杀成 0 字节记录（C1）。
        if self.is_shutting_down() {
            tracing::info!(room_id = %room.id, "拒绝开录：应用正在退出");
            return Err(AppError::User("应用正在退出".into()));
        }
        // 上一场正在合成 / 写库：它其实还在「这一场」的收尾里，不能重开（C4）。
        if self.is_finalizing(&room.id) {
            tracing::debug!(room_id = %room.id, "拒绝开录：上一场仍在收尾");
            return Err(AppError::User("上一场正在收尾".into()));
        }
        if self.db.room_has_unclosed_recording(&room.id)? {
            return Err(AppError::User(
                "上一场录制尚未确认退出，请恢复录像目录后重新启动".into(),
            ));
        }
        if self.cooling_down(&room.id) {
            tracing::warn!(room_id = %room.id, "拒绝开录：该房间连续录制失败，仍在冷却");
            return Err(AppError::User("该房间连续录制失败，已暂停自动重试".into()));
        }
        // 连续失败冷却（R6/R7）只拦自动开录：用户手动点「录制」说明他知道源可能有问题，
        // 硬拦 5 分钟反而像功能坏了；这里只记一条日志，让 ffmpeg 去试。
        if self.in_failure_cooldown(&room.id) {
            if recheck {
                return Err(AppError::User("该房间连续录制失败，已暂停自动重试".into()));
            }
            tracing::info!(room = %room.host_name, "手动开录，忽略连续失败冷却");
        }

        // 从这里开始算「开录在途」：停录请求要能穿过解析线路、拉起 ffmpeg 这段窗口（C5）。
        let slot = self.begin_start(&room.id);

        // begin_start 之前的一瞬用户可能刚好关掉自动录制：cancel_start 看到
        // 还没进入 starting，不会留下标记。进入在途后立刻再读一次库，
        // 把这个窗口也关上；此后的关闭会由 is_start_cancelled 拦住。
        if recheck
            && !self
                .db
                .get_room(&room.id)
                .ok()
                .flatten()
                .is_some_and(|fresh| fresh.auto_record)
        {
            tracing::info!(room = %room.host_name, "开录在途时自动录制已关闭，取消本次开录");
            return Err(AppError::User("自动录制已关闭".into()));
        }

        // 磁盘写满会让 ffmpeg 直接退出，还会把系统拖死，所以先拦一道。
        // can_start 是开录门槛，is_low 是录制红线：两个都得过，
        // 否则「已有录制被红线停掉、监控又立刻重新开录」会变成反复起停。
        let media_root = self.media_root();
        if !disk::can_start(&media_root) || disk::is_low(&media_root) {
            let message = disk::start_blocked_message(&media_root);
            tracing::warn!(
                room_id = %room.id,
                media_root = %media_root.display(),
                can_start = disk::can_start(&media_root),
                low = disk::is_low(&media_root),
                "拒绝开录：磁盘空间不足"
            );
            self.set_error(&room.id, message.clone());
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            return Err(AppError::User(message));
        }

        // 开录时现取设置：画质/格式/编码偏好与分割策略只影响新会话，
        // 正在录的这一场不会因为用户中途改设置而换档（A3）。
        let mut settings = crate::settings::load(&self.db);
        // 单房间分片覆盖：Some 时覆盖全局；0 表示关闭该项。
        if let Some(value) = room.split_max_duration_sec {
            settings.split_max_duration_sec = value.clamp(0, u32::MAX as i64) as u32;
        }
        if let Some(value) = room.split_max_size_mb {
            settings.split_max_size_mb = value.clamp(0, u32::MAX as i64) as u32;
        }
        let record_qn =
            crate::settings::effective_record_qn(settings.default_record_qn, room.record_qn);
        let prefs = StreamPrefs::from_settings(&settings, record_qn);
        let split = SplitPolicy::from_settings(&settings);
        let post = crate::post::PostConfig::from_settings(&settings);

        let plan = broker
            .plan(
                &crate::platform::RoomRef {
                    platform: room.platform.clone(),
                    room_key: room.room_key.clone(),
                    canonical_url: room.canonical_url.clone(),
                    input: room.canonical_url.clone(),
                },
                record_qn,
            )
            .await
            .map_err(|err| {
                tracing::warn!(room_id = %room.id, room = %room.host_name, error = %err, "开录解析线路失败");
                AppError::from(err)
            })?;

        if self.is_start_cancelled(&room.id) {
            tracing::info!(room = %room.host_name, "开录在途时自动录制被关闭，取消本次开录");
            return Err(AppError::User("自动录制已关闭".into()));
        }

        let (candidate, group) = pick_candidate(&plan.groups, room.record_line.as_deref(), prefs)
            .ok_or_else(|| {
            tracing::warn!(
                room_id = %room.id,
                room = %room.host_name,
                groups = plan.groups.len(),
                line = ?room.record_line,
                "开录没有可用线路"
            );
            if only_av1_available(&plan.groups) {
                AppError::User("当前直播只提供 AV1 编码，录制链路暂不支持 AV1".into())
            } else {
                AppError::User("没有可用的线路".into())
            }
        })?;

        let actual_qn = candidate_qn(candidate);
        let started_at = crate::db::now_ts();
        let path = session_path(
            &media_root,
            &crate::filename::NameParts {
                platform_id: &room.platform,
                platform: platform_label(&room.platform),
                host: &room.host_name,
                title: &room.title,
                area: room.area_name.as_deref().unwrap_or(""),
                room_key: &room.room_key,
                // 文件名里写实际拿到的档位：Cookie 失效时平台会降档，
                // 写请求值会让用户以为录到的是 4K。
                qn: if actual_qn > 0 { actual_qn } else { record_qn },
            },
            started_at,
            // 母带容器按实际拿到的源流格式与编码决定，后处理只产出派生文件。
            crate::post::source_extension(candidate.format, candidate.codec),
            &self.naming(),
        );
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                tracing::warn!(room_id = %room.id, dir = %parent.display(), error = %err, "创建录制目录失败");
                AppError::from_io(err, parent)
            })?;
        }
        // 同名会同时覆盖视频和旁挂弹幕，落盘前先让开；这里是原子占位，
        // 两路同一秒开录也不会拿到同一个路径（D1）。目录必须先建好。
        let path = crate::filename::reserve_unique_path(&path).map_err(|err| {
            tracing::warn!(room_id = %room.id, file = %path.display(), error = %err, "预占录制文件失败");
            AppError::Internal(format!("无法创建录制文件：{err}"))
        })?;

        let headers: Vec<(String, String)> = candidate
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let url = candidate.url.clone().unwrap_or_default();

        if self.is_start_cancelled(&room.id) {
            let _ = tokio::fs::remove_file(&path).await;
            tracing::info!(room = %room.host_name, "开录在途时自动录制被关闭，取消本次开录");
            return Err(AppError::User("自动录制已关闭".into()));
        }

        let mut command = build_command(&path, &url, &headers, 0, split.segment_secs);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                // reserve_unique_path 已经建了一个空占位文件；启动失败不删，
                // 媒体库里就会留一条 0 字节录像（H3）。
                tracing::warn!(
                    room_id = %room.id,
                    room = %room.host_name,
                    file = %path.display(),
                    quality = %candidate.name,
                    line = %group.name,
                    error = %err,
                    "启动录制 ffmpeg 失败"
                );
                let _ = tokio::fs::remove_file(&path).await;
                return Err(AppError::Ffmpeg(format!("启动 ffmpeg 失败：{err}")));
            }
        };
        let stderr_tail = drain_stderr(&mut child);
        if let Err(err) = write_pid_file(&path, child.id()) {
            if child.kill().await.is_ok() && path.metadata().is_ok_and(|m| m.len() == 0) {
                let _ = tokio::fs::remove_file(&path).await;
            }
            return Err(AppError::from_io(err, pid_file(&path)));
        }

        // 弹幕落盘与视频同名，只换扩展名；后处理阶段按这个约定找旁挂文件。
        // 总开关关闭时整场不挂 writer：既不建 JSONL，也不会在收尾时导出 ASS。
        let danmaku_path = path.with_extension("jsonl");
        let danmaku = self
            .danmaku
            .as_ref()
            .filter(|_| settings.record_danmaku && crate::danmaku::supports(room))
            .and_then(|hub| {
                hub.attach_record(
                    room,
                    &danmaku_path,
                    crate::danmaku::RecordRules::new(
                        settings.record_danmaku_gift,
                        settings.record_danmaku_super_chat,
                        settings.record_danmaku_guard,
                    ),
                )
            });
        if settings.record_danmaku
            && self.danmaku.is_some()
            && danmaku.is_none()
            && crate::danmaku::supports(room)
        {
            tracing::warn!(path = %danmaku_path.display(), "弹幕文件创建失败，本场只录视频");
        }
        let danmaku_path_text = danmaku.as_ref().map(|_| danmaku_path.display().to_string());

        let session_id = uuid::Uuid::new_v4().to_string();
        let quality_history = vec![QualitySegment {
            at: started_at,
            qn: actual_qn,
            quality: candidate.name.clone(),
            line: group.name.clone(),
        }];
        let created = self.db.create_session(&RecordSession {
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
            quality_history: quality_history.clone(),
            line: Some(group.name.clone()),
            error: None,
            danmaku_path: danmaku_path_text.clone(),
            danmaku_count: 0,
            source_path: Some(path.display().to_string()),
            post_mode: post.mode.clone(),
            post_state: crate::post::STATE_RECORDING.to_string(),
            source_deleted: false,
            post_error: None,
            group_id: None,
            clip_index: 0,
        });
        if let Err(err) = created {
            tracing::error!(
                room_id = %room.id,
                room = %room.host_name,
                session_id = %session_id,
                file = %path.display(),
                error = %err,
                "创建录制会话失败，回收 ffmpeg 与占位文件"
            );
            // 建会话失败时 ffmpeg 已经起来了：不收掉会变成一个没人管的进程（C5）。
            // 必须等它真的退出再删文件：Windows 上 ffmpeg 还持有分片句柄时
            // remove_file 会失败，留下 0 字节录像和孤儿分片。
            let _ = child.start_kill();
            let _ = wait_exit(&mut child, Duration::from_secs(2)).await;
            remove_pid_file(&path);
            if let (Some(hub), Some(writer)) = (self.danmaku.as_ref(), danmaku.as_ref()) {
                hub.detach_record(&room.id, writer);
            }
            // 这次开录没成，留下的空文件/空弹幕/已生成分片也算残留，一并清掉。
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&danmaku_path);
            for part in segment::collect_parts(&path) {
                let _ = std::fs::remove_file(part);
            }
            return Err(AppError::Internal(err.to_string()));
        }

        let target_quality = if record_qn >= crate::settings::RECORD_QN_HIGHEST {
            "最高可用".to_string()
        } else {
            plan.qualities
                .iter()
                .find(|quality| quality.qn == record_qn)
                .map(|quality| quality.label.clone())
                .unwrap_or_else(|| candidate.name.clone())
        };
        let recording = RecordingInfo {
            room_id: room.id.clone(),
            session_id,
            file_path: path.display().to_string(),
            started_at,
            quality: candidate.name.clone(),
            line: group.name.clone(),
            target_qn: record_qn,
            target_quality,
        };

        let entry = Active {
            recording: recording.clone(),
            child,
            stop_reason: None,
            started: Instant::now(),
            host_name: room.host_name.clone(),
            danmaku,
            restarts: 0,
            live_started_at: room.started_at,
            last_bytes: 0,
            last_growth: Instant::now(),
            stalls: 0,
            eof_retries: 0,
            qn: actual_qn,
            target_qn: record_qn,
            plan_qn: record_qn,
            upgrading: false,
            upgrades: 0,
            quality_history,
            line: room.record_line.clone(),
            prefs,
            codec: candidate.codec,
            format: candidate.format,
            split,
            title: room.title.clone(),
            title_changes: Vec::new(),
            title_checked: Instant::now(),
            stderr_tail,
            post,
            notify_live: room.notify_live.unwrap_or(true),
            notify_error: room.notify_error.unwrap_or(true),
        };
        // 录制数量由用户控制：这里只把会话挂进 active，不再做全局并发上限检查。
        self.inner
            .lock()
            .expect("录制状态被污染")
            .insert(room.id.clone(), entry);
        let (cancelled, requested_stop) = slot.finish();
        app.state::<crate::state::AppState>().jobs.emit_session(
            &recording.session_id,
            "record",
            "recording",
            "running",
            0,
            Some(&recording.quality),
        );

        // 挂入 active 后已经取走本次的取消与停录标记，再复查最新的自动录制开关。
        let auto_off = recheck
            && !self
                .db
                .get_room(&room.id)
                .ok()
                .flatten()
                .is_some_and(|fresh| fresh.auto_record);
        // 开录在途时用户点了停录：会话既然已经挂上，就立刻按这个原因收尾（C5）。
        let pending = if cancelled || auto_off {
            Some("stopped".to_string())
        } else {
            requested_stop
        };
        if let Some(reason) = pending {
            tracing::info!(
                room = %room.host_name,
                reason = %reason,
                auto_off,
                cancelled,
                "开录在途时收到停止请求，立即收尾"
            );
            {
                let mut inner = self.inner.lock().expect("录制状态被污染");
                if let Some(active) = inner.get_mut(&room.id) {
                    active.stop_reason = Some(reason);
                    send_graceful(&mut active.child);
                }
            }
            let _ = app.emit(EVENT_ROOMS_CHANGED, ());
            self.spawn_watcher(app.clone(), room.id.clone());
            return Ok(recording);
        }

        // 这一场已经开始录了：清掉上一场的停录抑制 / 结束静默。
        self.resume_auto(&room.id);

        tracing::info!(
            room_id = %room.id,
            session_id = %recording.session_id,
            room = %room.host_name,
            file = %path.display(),
            quality = %candidate.name,
            requested_qn = record_qn,
            actual_qn,
            line = %group.name,
            split_on_title = settings.split_on_title,
            split_max_duration_sec = settings.split_max_duration_sec,
            split_max_size_mb = settings.split_max_size_mb,
            "开始录制"
        );
        self.clear_error(&room.id);
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        if notify_started && room.notify_live.unwrap_or(true) {
            emit_notice(app, "started", &room.id, &room.host_name, None);
        }
        if quality_fallback(record_qn, actual_qn) {
            // 显式选了档位却只拿到更低档：通常是 Cookie 失效、账号权限不够或平台限流。
            // 录制照常开始，不能因为画质不达标就放弃；但必须让用户知道实际档位。
            tracing::warn!(
                room_id = %room.id,
                room = %room.host_name,
                requested_qn = record_qn,
                actual_qn,
                quality = %candidate.name,
                "实际画质低于所选档位，已按可用画质开录"
            );
            if room.notify_live.unwrap_or(true) {
                emit_notice(
                    app,
                    "quality_fallback",
                    &room.id,
                    &room.host_name,
                    Some(format!("{record_qn} → {actual_qn}")),
                );
            }
        }
        if codec_fallback(prefs.codec, candidate.codec) {
            tracing::warn!(
                room_id = %room.id,
                room = %room.host_name,
                requested_codec = prefs.codec.map(Codec::as_str).unwrap_or("auto"),
                actual_codec = candidate.codec.map(Codec::as_str).unwrap_or("unknown"),
                "平台没有所选编码，已按可用编码开录"
            );
        }
        if format_fallback(prefs.format, candidate.format) {
            tracing::warn!(
                room_id = %room.id,
                room = %room.host_name,
                requested_format = prefs.format.map(StreamFormat::as_str).unwrap_or("auto"),
                actual_format = candidate.format.as_str(),
                "平台没有所选源流格式，已按可用格式开录"
            );
        }
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
        // 开录还在路上（解析线路 / 起 ffmpeg）时先记下这条请求：start 把会话
        // 挂进 active 后会立刻消费它，否则会被结尾的 resume_auto 抹掉（C5）。
        {
            let starting = self.starting.lock().expect("开录标记被污染");
            if starting.contains(room_id) {
                self.pending_stop
                    .lock()
                    .expect("停录标记被污染")
                    .insert(room_id.to_string(), reason.to_string());
            }
        }
        if reason == "stopped" {
            // 记住这场的开播时间：同一场不再自动拉起，换新场次自动放行（S3）。
            let live = self
                .db
                .get_room(room_id)
                .ok()
                .flatten()
                .and_then(|room| room.started_at);
            self.suppressed
                .lock()
                .expect("停录标记被污染")
                .insert(room_id.to_string(), live);
            // 用户亲自停的：之前若刚被标记成「确认下播」，以用户操作为准。
            self.offline_stops
                .lock()
                .expect("离线停录标记被污染")
                .remove(room_id);
        }
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get_mut(room_id) else {
                return Ok(());
            };
            // 幂等：监控每秒一拍都会走到这里，ffmpeg 退出前不该反复发信号。
            // 已有 reason 时只允许「用户停录」覆盖「磁盘红线」——用户的意思更优先。
            if !should_restop(active.stop_reason.as_deref(), reason) {
                return Ok(());
            }
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
        // 先立旗再收：监控每秒一拍，晚一步就会在收尾窗口里再拉起一路（C1）。
        self.begin_shutdown();
        let ids: Vec<String> = self
            .inner
            .lock()
            .expect("录制状态被污染")
            .keys()
            .cloned()
            .collect();
        if ids.is_empty() {
            tracing::debug!("退出收尾：没有进行中的录制会话");
            return;
        }
        tracing::info!(count = ids.len(), "开始收尾全部录制会话");
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            for active in inner.values_mut() {
                active.stop_reason = Some("app_exit".into());
                send_graceful(&mut active.child);
            }
        }
        let total = ids.len();
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
                    tracing::warn!(room_id = %id, "录制 ffmpeg 未在 2 秒内退出，强制结束");
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
            self.finalize(app, &id, status.0, status.1, true).await;
        }
        tracing::info!(count = total, "录制会话收尾完成");
    }

    /// 轮询标题，记录变化时刻（用于收尾时分割）。每 5 秒最多查一次库。
    fn track_title(&self, room_id: &str) {
        let due = {
            let inner = self.inner.lock().expect("录制状态被污染");
            match inner.get(room_id) {
                Some(active) if active.split.on_title => {
                    active.title_checked.elapsed() >= Duration::from_secs(5)
                }
                _ => false,
            }
        };
        if !due {
            return;
        }
        let title = match self.db.get_room(room_id) {
            Ok(Some(room)) => room.title,
            _ => return,
        };
        let mut inner = self.inner.lock().expect("录制状态被污染");
        let Some(active) = inner.get_mut(room_id) else {
            return;
        };
        active.title_checked = Instant::now();
        if active.title != title {
            tracing::info!(
                room_id,
                old = %active.title,
                new = %title,
                "直播标题变化，收尾时按此分割"
            );
            active.title = title;
            active.title_changes.push(crate::db::now_ts());
        }
    }

    fn spawn_watcher(self: &Arc<Self>, app: AppHandle, room_id: String) {
        let recorder = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                let Some(rec) = recorder.upgrade() else {
                    return;
                };
                if !rec.is_recording(&room_id) {
                    return;
                }
                // 升级会主动停掉旧 ffmpeg；这段窗口的退出是预期行为，
                // 交给升级任务自己续上，watcher 既不能收尾也不能续录。
                if rec.is_upgrading(&room_id) {
                    continue;
                }
                // 标题变化要切片段：只在收尾时按时间点找分片边界，录制过程不打断。
                rec.track_title(&room_id);
                // 房间在录制过程中被删掉：删除流程自己会先停录，但那一步可能
                // 因为异常丢掉。这里兜底按「用户停录」收尾——不续录、
                // 不弹结束通知，也不给已删房间写静默窗口（F2）。
                if rec.room_removed(&room_id) {
                    tracing::info!(room_id, "房间已删除，结束本次录制");
                    let status = rec.kill_and_wait(&room_id).await;
                    rec.finalize(
                        &app,
                        &room_id,
                        status.unwrap_or_default(),
                        Some("stopped".into()),
                        false,
                    )
                    .await;
                    return;
                }
                let Some((status, stop_reason)) = rec.poll_exit(&room_id) else {
                    // 进程还活着，但可能卡在「连着却不给数据」：ffmpeg 不退出、
                    // 文件也不增长，会话会永远停在 recording（S4）。
                    if !rec.stalled(&room_id) {
                        continue;
                    }
                    // 掉盘、磁盘红线、用户停录都只置了 stop_reason，ffmpeg 却可能
                    // 卡在 IO 上不退出；此时再续录就是拿 stalled 覆盖真正的原因（H4）。
                    if let Some(reason) = rec.stop_reason(&room_id) {
                        tracing::info!(
                            room_id,
                            reason = %reason,
                            "已有停录请求，不再重启 ffmpeg，直接收尾"
                        );
                        let killed = rec.kill_and_wait(&room_id).await;
                        rec.finalize(
                            &app,
                            &room_id,
                            killed.unwrap_or_default(),
                            Some(reason),
                            false,
                        )
                        .await;
                        return;
                    }
                    tracing::warn!(
                        room_id,
                        tail = %rec.stderr_tail(&room_id),
                        "录制长时间没有新数据，重启 ffmpeg"
                    );
                    let killed = rec.kill_and_wait(&room_id).await;
                    let stalls = rec.bump_stall(&room_id);
                    let broker = {
                        let state = app.state::<crate::state::AppState>();
                        state.broker.clone()
                    };
                    if stalls <= MAX_STALL_RESTARTS && rec.respawn(&app, &broker, &room_id).await {
                        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                        continue;
                    }
                    rec.finalize(
                        &app,
                        &room_id,
                        killed.unwrap_or_default(),
                        Some("stalled".into()),
                        true,
                    )
                    .await;
                    return;
                };
                // 意外断流（不是用户停、也不是平台正常下播）先续录，
                // 网络抖一下不该把整场录像切成两条历史。
                // 退出码 0 但只活了不到 EARLY_EXIT 的同样算断流：播放地址过期时
                // ffmpeg 会当成正常 EOF 退出，不重试的话整场录像就此提前结束。
                let early = rec
                    .elapsed(&room_id)
                    .is_some_and(|elapsed| elapsed < EARLY_EXIT);
                // 干净退出也可能是 CDN 主动关连接：ffmpeg 读到 EOF 会当成正常结束。
                // 收尾前问一次平台，仍显示在播就按断流续录（R1）。真正的下播
                // 这里只会多一次查询，不会重连。
                let retries = rec.eof_retries(&room_id);
                // 复核预算已经用完还拿不到确认：按「未确认结束」收尾。
                // 不能当成确认下播——那会给一场可能还在播的录像弹结束通知，
                // 并用 10 分钟静默挡住重开（A4）。
                if stop_reason.is_none() && !early && status.success() && retries >= MAX_EOF_RETRIES
                {
                    tracing::warn!(room_id, "多次复核仍未确认下播，按未确认结束收尾");
                    rec.finalize(&app, &room_id, status, stop_reason, false)
                        .await;
                    return;
                }
                if should_probe_platform(stop_reason.as_deref(), status.success(), early, retries) {
                    let broker = {
                        let state = app.state::<crate::state::AppState>();
                        state.broker.clone()
                    };
                    let mut state = rec.platform_state(&broker, &room_id).await;
                    if state == PlatformState::Unknown {
                        // 一次查询失败可能只是限流 / 网络抖动：隔一会儿再问一次。
                        tokio::time::sleep(PLATFORM_RECHECK_DELAY).await;
                        // 等待期间用户可能已经点了停录，按用户的意思收尾。
                        let latest = rec.poll_exit(&room_id).map(|(_, reason)| reason);
                        if let Some(Some(reason)) = latest {
                            rec.finalize(&app, &room_id, status, Some(reason), true)
                                .await;
                            return;
                        }
                        state = rec.platform_state(&broker, &room_id).await;
                    }
                    match state {
                        PlatformState::Live => {
                            tracing::info!(room_id, "平台仍显示在播，按断流续录");
                            if rec.respawn(&app, &broker, &room_id).await {
                                // 只有干净 EOF 才消耗复核预算：崩溃续录不该吃掉它。
                                if status.success() {
                                    rec.bump_eof_retry(&room_id);
                                }
                                let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                                continue;
                            }
                            // 续不上了，但平台说还在播：绝不能落到下面那条「确认下播」
                            // 的收尾，否则会弹结束通知并静默 10 分钟（B1/B4）。
                            tracing::warn!(room_id, "平台仍显示在播，但续录失败，按未确认结束收尾");
                            rec.finalize(&app, &room_id, status, stop_reason, false)
                                .await;
                            return;
                        }
                        PlatformState::Offline => {
                            // 半关连接（写一半对端消失）常让 ffmpeg 非 0 退出，
                            // 但平台已经确认离线，就该按正常下播收尾（A11）。
                            rec.finalize(&app, &room_id, status, stop_reason, true)
                                .await;
                            return;
                        }
                        PlatformState::Unknown => {
                            tracing::warn!(room_id, "平台状态未知，按未确认结束收尾");
                            rec.finalize(&app, &room_id, status, stop_reason, false)
                                .await;
                            return;
                        }
                    }
                }
                if stop_reason.is_none() && (!status.success() || early) {
                    let broker = {
                        let state = app.state::<crate::state::AppState>();
                        state.broker.clone()
                    };
                    if rec.respawn(&app, &broker, &room_id).await {
                        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
                        continue;
                    }
                    // 续录没成功：先问平台，只有明确离线才算确认下播（B1）。
                    // 平台还在播 / 查不到时按未确认收尾，留给监控重新判断。
                    let confirmed = rec.platform_confirmed_offline(&broker, &room_id).await;
                    tracing::warn!(room_id, confirmed, "续录失败，按平台状态收尾");
                    rec.finalize(&app, &room_id, status, stop_reason, confirmed)
                        .await;
                    return;
                }
                // 能走到这里的只剩「用户停录 / 监控确认下播」：stop_reason 已置，
                // 这本身就是确认凭据；其余分支在各自的位置 return 或 continue 了（B1）。
                let confirmed = stop_reason.is_some();
                rec.finalize(&app, &room_id, status, stop_reason, confirmed)
                    .await;
                return;
            }
        });
    }

    fn stderr_tail(&self, room_id: &str) -> String {
        self.inner
            .lock()
            .expect("录制状态被污染")
            .get(room_id)
            .map(|active| tail_text(&active.stderr_tail))
            .unwrap_or_default()
    }

    /// 文件是否已经 STALL_TIMEOUT 没有增长；活着且正常写入时顺便刷新基线。
    fn stalled(&self, room_id: &str) -> bool {
        let mut inner = self.inner.lock().expect("录制状态被污染");
        let Some(active) = inner.get_mut(room_id) else {
            return false;
        };
        let bytes = segment::parts_total(Path::new(&active.recording.file_path));
        if bytes > active.last_bytes {
            active.last_bytes = bytes;
            active.last_growth = Instant::now();
            active.stalls = 0;
            // 稳稳录了半小时以上：把「断流续录」「EOF 复核」的预算补满，
            // 否则预算是一次性的，长直播抖动几次之后就再也续不上（A2）。
            if should_reset_budget(
                active.started.elapsed(),
                active.restarts,
                active.eof_retries,
            ) {
                tracing::info!(room_id, "长时间稳定录制，重置续录预算");
                active.restarts = 0;
                active.eof_retries = 0;
            }
            return false;
        }
        active.last_growth.elapsed() >= STALL_TIMEOUT
    }

    fn bump_stall(&self, room_id: &str) -> u32 {
        let mut inner = self.inner.lock().expect("录制状态被污染");
        let Some(active) = inner.get_mut(room_id) else {
            return u32::MAX;
        };
        active.stalls += 1;
        active.last_growth = Instant::now();
        active.last_bytes = segment::parts_total(Path::new(&active.recording.file_path));
        active.stalls
    }

    /// 杀掉卡住的 ffmpeg 并等它真的退出：否则续录会和旧进程抢同一批分片。
    async fn kill_and_wait(&self, room_id: &str) -> Option<std::process::ExitStatus> {
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            if let Some(active) = inner.get_mut(room_id) {
                let _ = active.child.start_kill();
            }
        }
        for _ in 0..60 {
            if let Some((status, _)) = self.poll_exit(room_id) {
                return Some(status);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
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
        self.respawn_inner(
            app,
            broker,
            room_id,
            false,
            true,
            "quality_fallback",
            None,
            None,
        )
        .await
    }

    /// 续录/升级共用的重启逻辑。
    /// - skip_delay：升级时旧进程刚停，不必再等 3 秒
    /// - count_restart：升级不计入 restarts 上限，单独记 upgrades
    /// - qn：本次请求档位；None 用本场目标
    /// - require_better_than：候选必须严格高于该档，否则不动进程
    #[allow(clippy::too_many_arguments)]
    async fn respawn_inner(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: &StreamBroker,
        room_id: &str,
        skip_delay: bool,
        count_restart: bool,
        notice_kind: &'static str,
        qn: Option<u32>,
        require_better_than: Option<u32>,
    ) -> bool {
        // 房间已经被删除就别再续录：用户已经不关心这场直播了（F2）。
        if self.room_removed(room_id) {
            tracing::info!(room_id, "房间已删除，不再续录");
            return false;
        }
        // 取快照后立刻放锁：下面要 await 解析线路，持有 std::sync::Mutex 会卡死整个录制表。
        // 画质、编码、格式与线路同样取快照：这中间用户可能在设置页改过，
        // 正在录的这一场不该跟着换档（A3）。
        let (
            session_id,
            path,
            start_number,
            restarts,
            plan_qn,
            line,
            prefs,
            codec,
            format,
            segment_secs,
        ) = {
            let inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get(room_id) else {
                return false;
            };
            if count_restart && active.restarts >= MAX_RESTARTS {
                tracing::warn!(room_id, restarts = active.restarts, "续录次数用尽");
                return false;
            }
            let path = PathBuf::from(&active.recording.file_path);
            // 不能按「当前剩余分片数」编号：收尾分割会删掉前段，
            // 编号回退会覆盖后面已经录好的分片。
            let start_number = segment::next_part_index(&path);
            (
                active.recording.session_id.clone(),
                path,
                start_number,
                active.restarts,
                active.plan_qn,
                active.line.clone(),
                active.prefs,
                active.codec,
                active.format,
                active.split.segment_secs,
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
        // 但只丢地址、不丢共享失败冷却：录制重启不该把监控侧的限流退避清零（B3）。
        // 本次解析允许穿过冷却（续录是用户已经在等的动作），失败了也照常累计退避。
        // 给 CDN 一点时间回收上一条连接的动作放在 broker 调用前，顺序与旧版一致。
        if !skip_delay {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        let requested_qn = qn.unwrap_or(plan_qn);
        let plan = match broker.plan_for_respawn(&reference, requested_qn).await {
            Ok(plan) => plan,
            Err(err) => {
                tracing::warn!(%err, room_id, "续录解析线路失败");
                return false;
            }
        };
        // 续录钉住首段的编码与源流格式：平台中途换编码时，宁可牺牲一点码率，
        // 也不要让同一场的分片在 H.264/H.265 之间跳变。
        let prefs = respawn_prefs(requested_qn, prefs, codec, format);
        let Some((candidate, group)) = pick_candidate(&plan.groups, line.as_deref(), prefs) else {
            tracing::warn!(room_id, "续录没有可用线路");
            return false;
        };
        let actual_qn = candidate_qn(candidate);
        if let Some(previous) = require_better_than {
            if !quality_is_better(&plan, actual_qn, previous) {
                tracing::info!(
                    room_id,
                    previous_qn = previous,
                    candidate_qn = actual_qn,
                    "升级候选没有更高画质，保持当前档位"
                );
                return false;
            }
        }
        if let (Some(first), Some(next)) = (codec, candidate.codec) {
            if first != next {
                tracing::warn!(
                    room_id,
                    first = ?first,
                    next = ?next,
                    "续录找不到同编码线路，本场可能混入不同编码的分片"
                );
            }
        }
        if format_fallback(Some(format), candidate.format) {
            tracing::warn!(
                room_id,
                requested_format = format.as_str(),
                actual_format = candidate.format.as_str(),
                "续录找不到同源流格式线路，已按可用格式续录"
            );
        }
        let headers: Vec<(String, String)> = candidate
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let url = candidate.url.clone().unwrap_or_default();

        let mut command = build_command(&path, &url, &headers, start_number, segment_secs);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(%err, room_id, "续录启动失败");
                return false;
            }
        };
        let stderr_tail = drain_stderr(&mut child);
        if let Err(err) = write_pid_file(&path, child.id()) {
            tracing::warn!(%err, room_id, "续录无法登记 PID，停止本次子进程");
            let _ = child.kill().await;
            return false;
        }

        let mut change = None;
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
            // 解析的 3 秒里用户点了停录：这时候再续上就是「停了又自己开」（A12）。
            if active.stop_reason.is_some() {
                tracing::info!(room_id, "续录等待期间已被停录，放弃续录");
                let _ = child.start_kill();
                return false;
            }
            active.child = child;
            active.stderr_tail = stderr_tail;
            if count_restart {
                active.restarts = restarts + 1;
            } else {
                active.upgrades = active.upgrades.saturating_add(1);
            }
            active.upgrading = false;
            active.started = Instant::now();
            let previous_qn = active.qn;
            let previous_quality = active.recording.quality.clone();
            let quality_changed =
                actual_quality_changed(previous_qn, &previous_quality, actual_qn, &candidate.name);
            let line_changed = active.recording.line != group.name;
            if quality_changed || line_changed {
                if quality_changed {
                    active.qn = actual_qn;
                    active.recording.quality = candidate.name.clone();
                    active.quality_history.push(QualitySegment {
                        at: crate::db::now_ts(),
                        qn: actual_qn,
                        quality: candidate.name.clone(),
                        line: group.name.clone(),
                    });
                }
                active.recording.line = group.name.clone();
                change = Some((
                    quality_changed,
                    previous_qn,
                    previous_quality,
                    active.quality_history.clone(),
                    active.host_name.clone(),
                    active.notify_live,
                ));
            }
            tracing::info!(
                room_id,
                start_number,
                restarts = active.restarts,
                upgrades = active.upgrades,
                quality = %candidate.name,
                requested_qn,
                actual_qn,
                "续录/升级后重新开始写流"
            );
        }
        if let Some((
            quality_changed,
            previous_qn,
            previous_quality,
            history,
            host_name,
            notify_live,
        )) = change
        {
            if quality_changed {
                tracing::warn!(
                    room_id,
                    previous_qn,
                    actual_qn,
                    previous_quality = %previous_quality,
                    quality = %candidate.name,
                    "续录实际画质发生变化"
                );
            }
            if let Err(err) =
                self.db
                    .update_session_quality(&session_id, &candidate.name, &group.name, &history)
            {
                tracing::warn!(%err, room_id, "续录画质/线路写回数据库失败");
            }
            if quality_changed && notify_live {
                emit_notice(
                    app,
                    notice_kind,
                    room_id,
                    &host_name,
                    Some(format!("{previous_quality} → {}", candidate.name)),
                );
            }
        }
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        true
    }

    /// 凭据恢复后的画质重检：探测目标档是否已可用，必要时升级。
    ///
    /// 只在保存 Cookie / 扫码确认这类登录态变化后触发，不做周期探测。
    /// v1（同编码同源流格式）：在同一个会话里换 ffmpeg，分片继续编号；
    /// v2（编码/格式发生变化）：结束旧会话并开新会话，两条记录用 group_id 关联。
    /// 升级失败会退回当前实际档位续录，绝不让已开始的录制停在那里。
    pub async fn recheck_quality(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: &StreamBroker,
        room_id: &str,
    ) -> bool {
        let gate = self
            .upgrade_locks
            .lock()
            .expect("升级锁被污染")
            .entry(room_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _gate = gate.lock().await;

        let snapshot = {
            let inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get(room_id) else {
                return false;
            };
            if active.upgrading || active.stop_reason.is_some() {
                return false;
            }
            // 已经达到目标（或目标就是最高可用）时不用探测。
            if active.target_qn < crate::settings::RECORD_QN_HIGHEST
                && active.qn >= active.target_qn
                && active.qn > 0
            {
                return false;
            }
            Some((
                active.recording.session_id.clone(),
                active.qn,
                active.target_qn,
                active.line.clone(),
                active.prefs,
                active.codec,
                active.format,
                active.host_name.clone(),
                active.notify_live,
            ))
        };
        let Some((
            session_id,
            current_qn,
            target_qn,
            line,
            prefs,
            codec,
            format,
            host_name,
            notify_live,
        )) = snapshot
        else {
            return false;
        };

        let room = match self.db.get_room(room_id) {
            Ok(Some(room)) => room,
            Ok(None) => return false,
            Err(err) => {
                tracing::warn!(%err, room_id, "画质重检读取房间失败");
                return false;
            }
        };
        let reference = crate::monitor::room_ref(&room);
        // 探测失败不写监控退避：这次升级只是锦上添花，不能影响下播判定与重试节奏。
        let plan = match broker.probe(&reference, target_qn).await {
            Ok(plan) => plan,
            Err(err) => {
                tracing::debug!(%err, room_id, "画质升级探测失败，保持当前档位");
                return false;
            }
        };
        // 升级探测按用户偏好选流，不能钉住当前编码/格式：
        // 匿名时可能回落到 H.264，补 Cookie 后同一档位已有 HEVC/新格式，
        // 这正是 v2 需要识别并另开会话的场景。
        let probe_prefs = StreamPrefs {
            qn: target_qn,
            codec: prefs.codec,
            format: prefs.format,
        };
        let Some((candidate, _group)) = pick_candidate(&plan.groups, line.as_deref(), probe_prefs)
        else {
            return false;
        };
        let candidate_qn = candidate_qn(candidate);
        if !quality_is_better(&plan, candidate_qn, current_qn) {
            tracing::debug!(
                room_id,
                current_qn,
                candidate_qn,
                target_qn,
                "平台当前没有更高画质，保持当前档位"
            );
            return false;
        }

        // 标记「升级中」：watcher 看到 ffmpeg 退出时既不能收尾也不能续录。
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            let Some(active) = inner.get_mut(room_id) else {
                return false;
            };
            if active.recording.session_id != session_id {
                return false;
            }
            active.upgrading = true;
        }
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            if let Some(active) = inner.get_mut(room_id) {
                send_graceful(&mut active.child);
            }
        }
        let mut status = None;
        for _ in 0..50 {
            if let Some((exit, _)) = self.poll_exit(room_id) {
                status = Some(exit);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if status.is_none() {
            status = self.kill_and_wait(room_id).await;
        }
        // 等待期间用户点了停录 / 磁盘红线：直接放弃升级，让 watcher 正常收尾。
        if self.stop_reason(room_id).is_some() {
            self.set_upgrading(room_id, false);
            return false;
        }
        let status = match status {
            Some(status) => status,
            None => {
                self.set_upgrading(room_id, false);
                tracing::warn!(room_id, "画质升级时旧 ffmpeg 未能退出，放弃升级");
                return false;
            }
        };

        let same_container = !container_changed(codec, candidate.codec, format, candidate.format);
        if same_container {
            let upgraded = self
                .respawn_inner(
                    app,
                    broker,
                    room_id,
                    true,
                    false,
                    "quality_upgraded",
                    Some(target_qn),
                    Some(current_qn),
                )
                .await;
            if !upgraded {
                // 升级没成，退回旧档位继续录：哪怕这一秒只拿到旧画质，也不能停录。
                // 这里必须保持 upgrading，否则 watcher 会在回退进程启动前把会话收尾。
                let _ = self
                    .respawn_inner(
                        app,
                        broker,
                        room_id,
                        true,
                        false,
                        "quality_fallback",
                        Some(current_qn),
                        None,
                    )
                    .await;
            }
            self.set_upgrading(room_id, false);
            if upgraded {
                tracing::info!(
                    room_id,
                    previous_qn = current_qn,
                    target_qn,
                    candidate_qn,
                    "画质升级完成（同一会话续写）"
                );
            }
            return upgraded;
        }

        // v2：编码/源流格式变了，绝不把两种编码混进同一个文件。
        let old = {
            let mut inner = self.inner.lock().expect("录制状态被污染");

            inner.remove(room_id)
        };
        let Some(old) = old else {
            return false;
        };
        match self.start_inner(app, broker, &room, false, false).await {
            Ok(new) => {
                let _ = self.db.set_session_group(
                    &old.recording.session_id,
                    &old.recording.session_id,
                    0,
                    None,
                );
                let _ = self.db.set_session_group(
                    &new.session_id,
                    &old.recording.session_id,
                    1,
                    Some("quality_upgrade"),
                );
                tracing::info!(
                    room_id,
                    old_session = %old.recording.session_id,
                    new_session = %new.session_id,
                    previous_quality = %old.recording.quality,
                    quality = %new.quality,
                    "画质升级完成（新会话，编码/格式已变化）"
                );
                if notify_live {
                    emit_notice(
                        app,
                        "quality_upgraded",
                        room_id,
                        &host_name,
                        Some(format!("{} → {}", old.recording.quality, new.quality)),
                    );
                }
                let rec = Arc::clone(self);
                let app = app.clone();
                let room_id = room_id.to_string();
                tauri::async_runtime::spawn(async move {
                    rec.finalize_active(
                        &app,
                        &room_id,
                        old,
                        status,
                        Some("quality_upgrade".into()),
                        true,
                    )
                    .await;
                });
                true
            }
            Err(err) => {
                tracing::warn!(%err, room_id, "画质升级新会话启动失败，回退旧会话续录");
                {
                    let mut inner = self.inner.lock().expect("录制状态被污染");
                    inner.insert(room_id.to_string(), old);
                }
                let recovered = self
                    .respawn_inner(
                        app,
                        broker,
                        room_id,
                        true,
                        false,
                        "quality_fallback",
                        Some(current_qn),
                        None,
                    )
                    .await;
                if !recovered {
                    self.set_upgrading(room_id, false);
                }
                false
            }
        }
    }

    fn set_upgrading(&self, room_id: &str, value: bool) {
        if let Some(active) = self.inner.lock().expect("录制状态被污染").get_mut(room_id) {
            active.upgrading = value;
        }
    }

    /// 凭据保存 / 扫码确认后调用：只对同一平台正在录制的房间做一次重检。
    ///
    /// 保存 A 平台 Cookie 不能顺带探测 B 平台的活动录制：那会让一条无关的
    /// 录像因为另一个平台的登录态变化而短暂换 ffmpeg。平台从房间行读取，
    /// 因为 RecordingInfo 是前端快照，不携带平台字段。
    pub fn recheck_all_quality(
        self: &Arc<Self>,
        app: &AppHandle,
        broker: Arc<StreamBroker>,
        platform: &str,
        reason: &'static str,
    ) {
        for recording in self.active_all() {
            let room_platform = self
                .db
                .get_room(&recording.room_id)
                .ok()
                .flatten()
                .map(|room| room.platform);
            if room_platform.as_deref() != Some(platform) {
                continue;
            }
            let platform = platform.to_string();
            let recorder = Arc::clone(self);
            let app = app.clone();
            let broker = Arc::clone(&broker);
            tauri::async_runtime::spawn(async move {
                tracing::info!(
                    room_id = %recording.room_id,
                    platform,
                    reason,
                    "凭据恢复，检查是否可升级画质"
                );
                recorder
                    .recheck_quality(&app, &broker, &recording.room_id)
                    .await;
            });
        }
    }

    async fn finalize(
        self: &Arc<Self>,
        app: &AppHandle,
        room_id: &str,
        status: std::process::ExitStatus,
        stop_reason: Option<String>,
        // 这次收尾是否已经有「平台确认离线」的凭据（A4/A11）：
        // 没有凭据时不弹结束通知、也不写 10 分钟静默，留给监控重新判断。
        confirmed_end: bool,
    ) {
        let active = {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            let removed = inner.remove(room_id);
            if removed.is_some() {
                // 分片合成可能跑几十秒：这段时间里监控仍会看到「平台在播、没在录」，
                // 不挡住就会在旧会话收尾时并行开一条新会话（C4）。
                self.finalizing
                    .lock()
                    .expect("收尾标记被污染")
                    .insert(room_id.to_string());
            }
            removed
        };
        let Some(active) = active else { return };
        self.finalize_active(app, room_id, active, status, stop_reason, confirmed_end)
            .await;
    }

    /// 收尾一个已经摘下来的 Active：断流收尾与画质升级分会话共用。
    async fn finalize_active(
        self: &Arc<Self>,
        app: &AppHandle,
        room_id: &str,
        active: Active,
        status: std::process::ExitStatus,
        stop_reason: Option<String>,
        confirmed_end: bool,
    ) {
        let finalizing_guard = FinalizeGuard {
            recorder: self,
            room_id: room_id.to_string(),
        };

        let elapsed = active.started.elapsed();
        // 收尾产物：分片 → 源封装母带 → 后处理派生文件；合并失败退化为 .ts。
        let source_path = PathBuf::from(&active.recording.file_path);
        // 进程已经不在了，pid 文件没有保留价值；漏删会让下次启动误杀无关进程。
        remove_pid_file(&source_path);
        let post_cfg = active.post.clone();
        // 多片段收尾会连续跑多个后处理任务；取消要覆盖整场，先登记会话级作用域。
        let _post_scope = crate::post::begin_session(&active.recording.session_id);
        // 收尾里的合并与后处理同样会写文件：登记成任务，退出时才会等它写完再走。
        let jobs = app.state::<crate::state::AppState>().jobs.clone();
        let post_label = source_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| active.recording.session_id.clone());
        // 收尾任务必须挂到 session 上：任务中心按 `session:<id>` 聚合 Run，
        // 否则 finalizing/merging/splitting/danmaku/commit 这些事件会落到
        // `task:job:N` 这一条孤立的 run 上，详情时间线看不到。
        let post_job = jobs.begin_for(
            "finalize",
            post_label,
            Some(active.recording.quality.clone()),
            Some(active.recording.session_id.clone()),
            Some(room_id.to_string()),
        );
        // 收尾不是后处理槽位里的任务：它负责合并分片、导出弹幕、写分片清单。
        // 单独一个阶段，任务中心才能把「录制结束但文件还没落定」画出来。
        jobs.emit_session(
            &active.recording.session_id,
            "record",
            "recording",
            "done",
            100,
            None,
        );
        post_job.progress(0, "finalizing", Some("合并分片并生成文件"));
        // 状态先算出来：分割清单要把最终状态写进去，崩溃恢复才能还原。
        let (mut final_status, mut reason, mut error) = match stop_reason.as_deref() {
            Some("stopped") => ("finished", "stopped", None),
            Some("stream_ended") => ("finished", "stream_ended", None),
            Some("app_exit") => ("aborted", "app_exit", None),
            Some("disk_full") => ("aborted", "disk_full", None),
            Some("disk_lost") => ("aborted", "disk_lost", None),
            Some("stalled") => ("failed", "stalled", Some("直播流长时间没有数据".into())),
            Some("quality_upgrade") => ("finished", "quality_upgrade", None),
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

        // 弹幕 writer 必须在 ffmpeg 退出后立刻摘掉，不能等分片合并跑完。
        // 合并 2GB 录像可能持续十几秒，这段时间弹幕连接还活着，事件已经
        // 不属于这段视频：留在 JSONL 里会让条数/ASS 越界；合并多段时还会
        // 按上一段时长算偏移，落进下一段时间轴。
        let writer_started_ms = active
            .danmaku
            .as_ref()
            .map(|writer| writer.started_epoch_ms());
        let (danmaku_path, danmaku_count) = match (self.danmaku.as_ref(), active.danmaku.as_ref()) {
            (Some(hub), Some(writer)) => hub
                .detach_record(room_id, writer)
                .map_or((None, 0), |(path, count)| {
                    (Some(path.display().to_string()), count as i64)
                }),
            _ => (None, 0),
        };

        // 分割只在收尾做：录制期间始终是一个连续 ffmpeg，不存在停旧起新的空窗。
        let plans = clip_plans(&active);
        // 即使只有一段也写清单：收尾失败（磁盘满、合并/拼接失败）时
        // 它能保留「分片还在、成品未落定」的现场，启动后按清单重试。
        // 成功后清单会被删除，正常录制不会留下额外文件。
        let fail_before_merge = |message: String| {
            tracing::warn!(session = %active.recording.session_id, %message, "保留原始分片等待重试");
            if let Err(err) = self.db.finish_session_with_post(
                &active.recording.session_id,
                "failed",
                "finalize_failed",
                crate::db::now_ts(),
                &source_path.display().to_string(),
                0,
                Some(&source_path.display().to_string()),
                &post_cfg.mode,
                crate::post::STATE_FAILED,
                false,
                Some(&message),
                danmaku_path.as_deref(),
                danmaku_count,
                Some(&message),
            ) {
                tracing::warn!(%err, "登记收尾失败状态失败");
            }
            message
        };
        let _finalize_lease = match reserve_finalize(&manifest_path(&source_path).to_string_lossy())
        {
            Ok(lease) => lease,
            Err(err) => {
                post_job.finish("failed", Some(&fail_before_merge(err.to_string())));
                return;
            }
        };
        let mut manifest = if !plans.is_empty() {
            if plans.len() > 1 {
                tracing::info!(
                    session_id = %active.recording.session_id,
                    clips = plans.len(),
                    "开始按分片边界生成分割片段"
                );
            }
            let mut clips = Vec::new();
            for (index, plan) in plans.iter().enumerate() {
                let output = if index == 0 {
                    source_path.clone()
                } else {
                    match clip_output_path(&source_path, index) {
                        Ok(path) => path,
                        Err(err) => {
                            post_job.finish("failed", Some(&fail_before_merge(err.to_string())));
                            return;
                        }
                    }
                };
                let id = if index == 0 {
                    active.recording.session_id.clone()
                } else {
                    uuid::Uuid::new_v4().to_string()
                };
                clips.push(ManifestClip {
                    id,
                    output: output.display().to_string(),
                    path: output.display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: plan.start,
                    end: plan.end,
                    reason: plan.reason.clone(),
                    started_at: plan.started_at,
                    ended_at: plan.ended_at,
                });
            }
            let manifest = SplitManifest {
                version: 2,
                session_id: active.recording.session_id.clone(),
                room_id: active.recording.room_id.clone(),
                source: source_path.display().to_string(),
                status: final_status.to_string(),
                reason: reason.to_string(),
                error: error.clone(),
                quality: Some(active.recording.quality.clone()),
                quality_history: active.quality_history.clone(),
                line: Some(active.recording.line.clone()),
                post: post_cfg.clone(),
                clips,
                rendered: 0,
                completed: false,
            };
            let file = manifest_path(&source_path);
            if let Err(err) = write_manifest(&file, &manifest) {
                post_job.finish("failed", Some(&fail_before_merge(err)));
                return;
            } else if let Err(err) = self.db.set_split_manifest(
                &active.recording.session_id,
                Some(&file.display().to_string()),
            ) {
                post_job.finish("failed", Some(&fail_before_merge(err.to_string())));
                return;
            }
            Some(manifest)
        } else {
            None
        };
        // 阶段一（封口）：只做分片合并/按边界切片段，不跑后处理。
        // 后处理可能转码几分钟，不能把同房间的新一场挡在 finalizing 外面。
        let mut prepared: Vec<PreparedClip> = Vec::new();
        post_job.progress(
            5,
            if plans.len() > 1 {
                "splitting"
            } else {
                "merging"
            },
            Some(if plans.len() > 1 {
                "合并分片并按边界切片段"
            } else {
                "合并分片"
            }),
        );
        for (index, plan) in plans.iter().enumerate() {
            let output = PathBuf::from(&manifest.as_ref().expect("有清单").clips[index].output);
            let (source_clip, source_size) =
                finish_range(&source_path, &output, plan.start, plan.end).await;
            if source_size <= 0 {
                tracing::warn!(
                    room_id,
                    clip = index,
                    file = %source_clip.display(),
                    "源封装母带缺失，跳过该片段的后处理"
                );
                prepared.push(PreparedClip {
                    source_path: source_clip.clone(),
                    source_size: 0,
                    reason: plan.reason.clone(),
                    started_at: plan.started_at,
                    ended_at: plan.ended_at,
                });
                if let Some(manifest) = manifest.as_mut() {
                    manifest.clips[index].path = source_clip.display().to_string();
                    manifest.rendered = index + 1;
                    let file = manifest_path(&source_path);
                    if let Err(err) = write_manifest(&file, manifest) {
                        tracing::warn!(%err, file = %file.display(), "更新分割清单失败");
                    }
                }
                continue;
            }
            prepared.push(PreparedClip {
                source_path: source_clip.clone(),
                source_size,
                reason: plan.reason.clone(),
                started_at: plan.started_at,
                ended_at: plan.ended_at,
            });
            if let Some(manifest) = manifest.as_mut() {
                manifest.clips[index].path = source_clip.display().to_string();
                manifest.rendered = index + 1;
                let file = manifest_path(&source_path);
                if let Err(err) = write_manifest(&file, manifest) {
                    tracing::warn!(%err, file = %file.display(), "更新分割清单失败");
                }
            }
            post_job.progress(
                5 + ((index + 1) as i64 * 25 / plans.len().max(1) as i64),
                if plans.len() > 1 {
                    "splitting"
                } else {
                    "merging"
                },
                Some(&format!("已完成 {}/{} 段", index + 1, plans.len())),
            );
        }

        // 0 字节不能算「录制完成」：在释放 finalizing 之前就定案，
        // 否则成功计数会先清掉失败冷却，库里却是一条空录像。
        let sealed_size = prepared.first().map_or(0, |clip| clip.source_size);
        if final_status == "finished" {
            if let Some((empty_reason, empty_error)) = empty_recording_failure(sealed_size) {
                tracing::warn!(room_id, "录制没有产生任何数据，标记为失败");
                final_status = "failed";
                reason = empty_reason;
                error = empty_error;
            }
        }

        // 房间级副作用必须在释放 finalizing 之前结算：新一场可能几秒后就开始，
        // 旧场的成功/失败/静默不能落到新场的计数上。
        if final_status == "finished" && reason == "stream_ended" && confirmed_end {
            self.mark_ended(room_id, active.live_started_at);
        }
        let unconfirmed = unconfirmed_end(final_status, reason, confirmed_end);
        let failed_streak = if final_status == "failed" {
            let streak = self.note_hard_failure(room_id);
            if streak == FAILED_STREAK_LIMIT {
                tracing::warn!(room_id, "连续录制失败，暂停自动重试 5 分钟");
            }
            streak
        } else if unconfirmed {
            let streak = self.note_hard_failure(room_id);
            if streak == FAILED_STREAK_LIMIT {
                tracing::warn!(room_id, "连续未确认收尾，暂停自动重试 5 分钟");
            }
            streak
        } else {
            0
        };

        if final_status == "finished" {
            // 未确认收尾刚刚记了一次失败，别在这里又清掉，否则永远攒不到阈值。
            if failed_streak == 0 {
                self.clear_failure(room_id);
                self.clear_hard_failure(room_id);
            }
        }

        drop(finalizing_guard);
        tracing::info!(
            room_id,
            session_id = %active.recording.session_id,
            clips = prepared.len(),
            "封口完成，释放收尾标记并转入后台后处理"
        );

        // 阶段二（后处理）：源封装母带已经落定，转封装/自定义命令慢慢跑。
        let mut rendered: Vec<RenderedClip> = Vec::new();
        for (index, clip) in prepared.iter().enumerate() {
            if clip.source_size <= 0 {
                rendered.push(RenderedClip {
                    source_path: clip.source_path.clone(),
                    path: clip.source_path.clone(),
                    size: 0,
                    post_state: crate::post::STATE_FAILED.to_string(),
                    post_error: Some("分片合并失败，保留现场等待恢复".into()),
                    delete_source: None,
                    reason: clip.reason.clone(),
                    started_at: clip.started_at,
                    ended_at: clip.ended_at,
                });
                continue;
            }
            let task_id = format!("auto:{}:{index}", active.recording.session_id);
            let outcome = crate::post::run(
                &task_id,
                &active.recording.session_id,
                &clip.source_path,
                &post_cfg,
                Some(app),
            )
            .await;
            let clip_state = outcome.state.clone();
            let clip_error = outcome.error.clone();
            if let Some(err) = outcome.error.as_deref() {
                tracing::warn!(
                    room_id,
                    clip = index,
                    source = %clip.source_path.display(),
                    state = %outcome.state,
                    error = %err,
                    "片段后处理失败，保留源封装母带"
                );
            }
            rendered.push(RenderedClip {
                source_path: outcome.source_path,
                path: outcome.path.clone(),
                size: outcome.size,
                post_state: outcome.state,
                post_error: outcome.error,
                delete_source: outcome.delete_source,
                reason: clip.reason.clone(),
                started_at: clip.started_at,
                ended_at: clip.ended_at,
            });
            if let Some(manifest) = manifest.as_mut() {
                manifest.clips[index].final_path = Some(outcome.path.display().to_string());
                manifest.clips[index].post_state = Some(clip_state);
                manifest.clips[index].post_error = clip_error;
                let file = manifest_path(&source_path);
                if let Err(err) = write_manifest(&file, manifest) {
                    tracing::warn!(%err, file = %file.display(), "更新分割清单失败");
                }
            }
        }
        // 后处理结束后还有切弹幕、写库、封面三件收尾事：finalize 任务保持到
        // 它们全部完成，详情时间线才不会在「后处理 100%」后突然断掉。
        post_job.progress(75, "danmaku", Some("切分并导出弹幕"));
        // 分片容器时长通常比标称 60 秒多出 0.2–0.5 秒，六段累计就能差出几秒。
        // 切弹幕必须按实际容器时长推进，否则片段越靠后，弹幕相对画面越早。
        let mut actual_durations: Vec<Option<i64>> = Vec::with_capacity(rendered.len());
        if rendered.len() > 1 {
            for clip in &rendered {
                let probe = if clip.source_path.is_file() {
                    &clip.source_path
                } else {
                    &clip.path
                };
                actual_durations.push(
                    crate::post::media_duration_ms(probe)
                        .await
                        .map(|ms| ms as i64)
                        .filter(|ms| *ms > 0),
                );
            }
        }
        let clip_windows = clip_windows(&actual_durations);
        let first = rendered.first().cloned().unwrap_or(RenderedClip {
            source_path: source_path.clone(),
            path: source_path.clone(),
            size: 0,
            post_state: crate::post::STATE_FAILED.to_string(),
            post_error: Some("没有生成任何片段".into()),
            delete_source: None,
            reason: None,
            started_at: active.recording.started_at,
            ended_at: crate::db::now_ts(),
        });
        let (stored_path, file_size) = (first.path, first.size);
        // 0 字节不能算「录制完成」：否则任务中心显示成功、媒体库留一个空文件，
        // 监控也会把这次当成正常结束。保留清单让启动恢复还能重试。
        if final_status == "finished" {
            if let Some((empty_reason, empty_error)) = empty_recording_failure(file_size) {
                tracing::warn!(room_id, "录制没有产生任何数据，标记为失败");
                final_status = "failed";
                reason = empty_reason;
                error = empty_error;
            }
        }
        // 片段历史是否全部落库；失败时不能删清单，否则恢复逻辑再没有重试依据。
        let mut history_ok = stored_path.is_file() && file_size > 0;
        if !history_ok {
            tracing::warn!(
                room_id,
                file = %stored_path.display(),
                size = file_size,
                "首片段成品缺失，保留分割清单以便恢复"
            );
        }
        if final_status == "failed" {
            let tail = tail_text(&active.stderr_tail);
            if !tail.is_empty() {
                tracing::warn!(room_id, tail = %tail, "ffmpeg 错误输出");
            }
        }

        // 分割录制只有一条整场 JSONL。收尾时按片段区间切片并重设 t=0，
        // 否则第二段起 danmaku_path 为空、ASS 也只覆盖第一段。
        let mut clip_danmaku: Vec<Option<(String, i64)>> = vec![None; rendered.len()];
        if rendered.len() > 1 {
            if let (Some(master), Some(writer_started_ms)) =
                (danmaku_path.as_deref(), writer_started_ms)
            {
                let recording_started_ms = active.recording.started_at * 1000;
                let writer_shift = writer_started_ms - recording_started_ms;
                // 原地切第一段会在下一段读取前截断母文件，先留一份只读副本。
                let first_jsonl = rendered[0].path.with_extension("jsonl");
                let source = Path::new(master).with_extension("split-src");
                if let Err(err) = std::fs::copy(master, &source) {
                    tracing::warn!(%err, file = %master, "复制整场弹幕失败，分割片段将没有弹幕");
                } else {
                    let mut first_sliced = false;
                    for (index, clip) in rendered.iter().enumerate() {
                        let (start, end) = match clip_windows.as_ref().and_then(|w| w.get(index)) {
                            Some((actual_start, actual_end)) => (
                                (*actual_start - writer_shift).max(0),
                                Some((*actual_end - writer_shift).max(0)),
                            ),
                            None => {
                                let start =
                                    (clip.started_at * 1000 - recording_started_ms - writer_shift)
                                        .max(0);
                                // 最后一段也要用结束时刻封口：停止录制后合并/收尾期间
                                // 弹幕连接还活着，继续到达的事件不属于这段视频。
                                let end = Some(
                                    (clip.ended_at * 1000 - recording_started_ms - writer_shift)
                                        .max(start),
                                );
                                (start, end)
                            }
                        };
                        let out = if index == 0 {
                            first_jsonl.clone()
                        } else {
                            clip.path.with_extension("jsonl")
                        };
                        match crate::postprocess::slice_danmaku(&source, &out, start, end) {
                            Ok(0) => {
                                if index == 0 {
                                    first_sliced = true;
                                }
                            }
                            Ok(count) => {
                                if index == 0 {
                                    first_sliced = true;
                                }
                                clip_danmaku[index] =
                                    Some((out.display().to_string(), count as i64));
                            }
                            Err(err) => tracing::warn!(
                                %err,
                                clip = index,
                                file = %out.display(),
                                "切分片段弹幕失败"
                            ),
                        }
                    }
                    let _ = std::fs::remove_file(&source);
                    if first_sliced && Path::new(master) != first_jsonl {
                        let _ = std::fs::remove_file(master);
                    }
                }
            }
        }
        let (db_danmaku_path, db_danmaku_count) = if rendered.len() > 1 {
            clip_danmaku
                .first()
                .and_then(|entry| entry.as_ref())
                .map_or((None, 0), |(path, count)| (Some(path.clone()), *count))
        } else {
            (danmaku_path.clone(), danmaku_count)
        };

        post_job.progress(90, "commit", Some("写入录像库"));
        let first_ended_at = clip_windows
            .as_ref()
            .map(|windows| first.started_at + (windows[0].1 + 999) / 1000)
            .unwrap_or(first.ended_at);
        match self.db.finish_session_with_post(
            &active.recording.session_id,
            final_status,
            reason,
            first_ended_at,
            &stored_path.display().to_string(),
            file_size,
            Some(&first.source_path.display().to_string()),
            &post_cfg.mode,
            &first.post_state,
            false,
            first.post_error.as_deref(),
            db_danmaku_path.as_deref(),
            db_danmaku_count,
            error.as_deref(),
        ) {
            Ok(()) => {
                delete_source_after_commit(
                    &self.db,
                    &active.recording.session_id,
                    first.delete_source.as_deref(),
                )
                .await;
            }
            Err(err) => {
                history_ok = false;
                tracing::warn!(%err, "写入录制历史失败");
            }
        }

        // 同一场直播的后续片段：共享首片段 id 作为 group_id，媒体库按 group 聚合。
        if rendered.len() > 1 {
            let group_id = active.recording.session_id.clone();
            if let Err(err) = self.db.set_session_group(&group_id, &group_id, 0, None) {
                history_ok = false;
                tracing::warn!(%err, "写入首片段分组失败");
            }
            for (index, clip) in rendered.iter().enumerate().skip(1) {
                if !clip.path.is_file() || clip.size <= 0 {
                    history_ok = false;
                    tracing::warn!(
                        room_id,
                        clip = index,
                        file = %clip.path.display(),
                        "片段成品缺失，跳过写库并保留清单"
                    );
                    continue;
                }
                // 用清单里的 id，崩溃后恢复不会写出两行同一片段。
                let clip_id = manifest
                    .as_ref()
                    .map(|manifest| manifest.clips[index].id.clone())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let (clip_danmaku_path, clip_danmaku_count) = clip_danmaku
                    .get(index)
                    .and_then(|entry| entry.as_ref())
                    .map_or((None, 0), |(path, count)| (Some(path.clone()), *count));
                let session = RecordSession {
                    id: clip_id.clone(),
                    room_id: active.recording.room_id.clone(),
                    started_at: clip.started_at,
                    ended_at: Some(clip.ended_at),
                    status: final_status.to_string(),
                    reason: Some(reason.to_string()),
                    file_path: Some(clip.path.display().to_string()),
                    file_size: clip.size,
                    duration_ms: actual_durations
                        .get(index)
                        .and_then(|value| *value)
                        .unwrap_or_else(|| (clip.ended_at - clip.started_at).max(0) * 1000),
                    quality: Some(active.recording.quality.clone()),
                    quality_history: active.quality_history.clone(),
                    line: Some(active.recording.line.clone()),
                    error: error.clone(),
                    danmaku_path: clip_danmaku_path,
                    danmaku_count: clip_danmaku_count,
                    source_path: Some(clip.source_path.display().to_string()),
                    post_mode: post_cfg.mode.clone(),
                    post_state: clip.post_state.clone(),
                    source_deleted: false,
                    post_error: clip.post_error.clone(),
                    group_id: Some(group_id.clone()),
                    clip_index: index as i64,
                };
                match self
                    .db
                    .create_clip(&session, &group_id, index as i64, clip.reason.as_deref())
                {
                    Ok(()) => {
                        delete_source_after_commit(
                            &self.db,
                            &clip_id,
                            clip.delete_source.as_deref(),
                        )
                        .await;
                        if final_status == "finished" {
                            self.queue_thumb(app, &clip_id);
                        }
                    }
                    Err(err) => {
                        history_ok = false;
                        tracing::warn!(
                            %err,
                            clip = index,
                            file = %clip.path.display(),
                            "写入片段历史失败"
                        );
                    }
                }
            }
        }

        // 片段行都写完了才把清单标成完成：中途崩溃时恢复逻辑会按清单补齐。
        // 只要有一段落库失败/成品缺失就保留清单，下次启动还能重试，不能
        // 把「已经收尾」当成「历史已经写全」。
        if let Some(mut manifest) = manifest.take() {
            let file = manifest_path(&source_path);
            if history_ok {
                for clip in &manifest.clips {
                    remove_range_parts(&source_path, clip.start, clip.end).await;
                }
                manifest.completed = true;
                if let Err(err) = write_manifest(&file, &manifest) {
                    tracing::warn!(%err, file = %file.display(), "完成分割清单失败");
                }
                match self
                    .db
                    .set_split_manifest(&active.recording.session_id, None)
                {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&file);
                    }
                    Err(err) => tracing::warn!(%err, "清理分割清单登记失败"),
                }
            } else {
                // 清单里保留已成功的 path/rendered，恢复时只补失败的片段和 DB 行。
                manifest.completed = false;
                if let Err(err) = write_manifest(&file, &manifest) {
                    tracing::warn!(%err, file = %file.display(), "保留分割清单失败");
                }
                tracing::warn!(
                    room_id,
                    session = %active.recording.session_id,
                    clips = manifest.clips.len(),
                    "片段历史未全部写入，保留分割清单等待恢复"
                );
            }
        }

        // 弹幕存档（.jsonl）随录随写；收尾时顺手导出一份 .ass，媒体目录里直接可用。
        // 分割录制要为每个片段各导一份，时间轴已随片段 JSONL 归零。
        let ass_targets: Vec<(String, i64)> = if rendered.len() > 1 {
            clip_danmaku
                .iter()
                .filter_map(|entry| entry.clone())
                .collect()
        } else {
            db_danmaku_path
                .clone()
                .filter(|_| db_danmaku_count > 0)
                .map(|path| (path, db_danmaku_count))
                .into_iter()
                .collect()
        };
        for (path, _) in ass_targets {
            match crate::postprocess::danmaku_to_ass(std::path::Path::new(&path)) {
                Ok(result) => tracing::info!(ass = %result.path, "已导出弹幕字幕"),
                Err(err) => tracing::warn!(%err, file = %path, "导出弹幕字幕失败"),
            }
        }

        if final_status == "finished" && history_ok {
            post_job.progress(98, "thumbnail", Some("生成封面缩略图"));
            self.queue_thumb(app, &active.recording.session_id);
        }
        if !history_ok {
            post_job.finish("failed", Some("录像尚未完整登记，已保留收尾清单供重试"));
        } else if final_status == "failed" {
            post_job.finish("failed", error.as_deref());
        } else {
            post_job.finish("done", error.as_deref().or(Some("录制收尾完成")));
        }

        // 每次收尾跑一次保留策略：录制是媒体库唯一的增长来源。
        crate::retention::enforce_from_settings(&self.db);
        crate::retention::enforce_tasks(&self.db);

        tracing::info!(
            room_id,
            session_id = %active.recording.session_id,
            status = final_status,
            reason,
            file_size,
            restarts = active.restarts,
            elapsed_ms = active.started.elapsed().as_millis() as u64,
            "录制结束"
        );
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());

        match (final_status, reason) {
            ("finished", "stream_ended") if confirmed_end => {
                if active.notify_live {
                    emit_notice(app, "ended", room_id, &active.host_name, None);
                }
            }
            ("finished", "stream_ended") => tracing::warn!(
                room_id,
                failed_streak,
                confirmed_end,
                "未能确认平台已下播，本次收尾不弹结束通知"
            ),
            ("aborted", "disk_full") => {
                if active.notify_error {
                    emit_notice(app, "disk_full", room_id, &active.host_name, None);
                }
            }
            ("aborted", "disk_lost") => {
                if active.notify_error {
                    emit_notice(app, "disk_lost", room_id, &active.host_name, None);
                }
            }
            ("failed", _) => {
                // 只提醒第 1 次与进入长冷却那一次；中间的重试不打扰用户。
                if active.notify_error
                    && (failed_streak == 1 || failed_streak == FAILED_STREAK_LIMIT)
                {
                    emit_notice(app, "failed", room_id, &active.host_name, error.clone());
                }
            }
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

    /// 记一次开录失败；攒够阈值后由 `in_start_cooldown` 拦住后续自动重试（G5）。
    fn note_start_failure(&self, room_id: &str) {
        let mut map = self.start_failures.lock().expect("开录失败状态被污染");
        let entry = map
            .entry(room_id.to_string())
            .or_insert((0, Instant::now()));
        // 冷却期早过了就重新计数：上次那批失败已经是旧账。
        if entry.1.elapsed() >= START_FAILURE_COOLDOWN {
            entry.0 = 0;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = Instant::now();
        if entry.0 == START_FAILURE_LIMIT {
            tracing::warn!(room_id, "连续开录失败，暂停自动重试 5 分钟");
        }
    }

    fn clear_start_failure(&self, room_id: &str) {
        self.start_failures
            .lock()
            .expect("开录失败状态被污染")
            .remove(room_id);
    }

    /// 这个房间是否处于「连续开录失败」的冷却里。
    pub fn in_start_cooldown(&self, room_id: &str) -> bool {
        let map = self.start_failures.lock().expect("开录失败状态被污染");
        match map.get(room_id) {
            Some((attempts, at)) => {
                *attempts >= START_FAILURE_LIMIT && at.elapsed() < START_FAILURE_COOLDOWN
            }
            None => false,
        }
    }
}

/// 这次开录失败该不该计入连续失败（G5）。
///
/// 用户意图 / 房间已移除这类失败，重试也不会变好，每拍重试只是刷日志；
/// 鉴权、取流地址、文件写入这些才值得冷却之后再试。
fn start_failure_is_counted(err: &AppError) -> bool {
    match err {
        AppError::Db(_) | AppError::Internal(_) => false,
        AppError::User(message) => ![
            "房间已移除",
            "房间不存在",
            "应用正在退出",
            "上一场正在收尾",
            "该房间连续录制失败",
            "连续开录失败",
        ]
        .iter()
        .any(|prefix| message.starts_with(prefix)),
        _ => true,
    }
}

/// 一次开录的「在途」标记：无论成功、失败还是提前 return 都要摘掉（C5）。
struct StartGuard<'a> {
    recorder: &'a Recorder,
    room_id: String,
    inserted: bool,
}

impl StartGuard<'_> {
    fn finish(mut self) -> (bool, Option<String>) {
        let mut cancelled = self.recorder.start_cancelled.lock().expect("取消标记");
        let mut starting = self.recorder.starting.lock().expect("开录标记");
        starting.remove(&self.room_id);
        let pending = self
            .recorder
            .pending_stop
            .lock()
            .expect("停录标记")
            .remove(&self.room_id);
        self.inserted = true;
        (cancelled.remove(&self.room_id), pending)
    }
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        if self.inserted {
            return;
        }
        // 与 begin_start / cancel_start 保持同一把锁顺序：先 cancel 标记，再 starting。
        self.recorder
            .start_cancelled
            .lock()
            .expect("开录取消标记被污染")
            .remove(&self.room_id);
        self.recorder
            .starting
            .lock()
            .expect("开录标记被污染")
            .remove(&self.room_id);
        // 开录失败：这条停录请求没有对应的会话可停，别留给下一次开录。
        self.recorder
            .pending_stop
            .lock()
            .expect("停录标记被污染")
            .remove(&self.room_id);
    }
}

/// 收尾期间挂在 finalizing 上：避免转封装窗口里监控又开一条会话（C4）。
struct FinalizeGuard<'a> {
    recorder: &'a Recorder,
    room_id: String,
}

impl Drop for FinalizeGuard<'_> {
    fn drop(&mut self) {
        self.recorder
            .finalizing
            .lock()
            .expect("收尾标记被污染")
            .remove(&self.room_id);
    }
}

/// 进程退出时平台的真实状态（A4/A11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformState {
    /// 平台显示在播：更像断流，值得续录。
    Live,
    /// 平台确认离线：正常下播。
    Offline,
    /// 查询失败 / 被限流：什么都不能断言，更不能当离线。
    Unknown,
}

/// 退出后要不要先问平台（R1/A4/A11）。
///
/// 条件：没人叫停、活够了（秒退走另一条续录分支）。另外两种情况都值得问：
/// - 干净 EOF，且复核预算没用完（R1）；
/// - 非 0 退出，但进程活过了秒退门槛（A11：半关连接会让 ffmpeg 报错）。
fn should_probe_platform(
    stop_reason: Option<&str>,
    status_success: bool,
    early: bool,
    retries: u32,
) -> bool {
    stop_reason.is_none() && !early && (!status_success || retries < MAX_EOF_RETRIES)
}

/// 已经有一个停录理由时，后来的请求还要不要真的生效（B6）。
///
/// 监控每拍都会对磁盘红线发一次 stop，ffmpeg 退出前不该每次都重发信号。
/// 唯一例外是用户手动停录：它可以把「磁盘红线」改成「用户停」，反之不行。
fn should_restop(existing: Option<&str>, reason: &str) -> bool {
    match existing {
        None => true,
        Some(existing) => existing != reason && reason == "stopped",
    }
}

/// 这算不算「平台在播、却只能以自然结束收尾」（B1）。
///
/// 只有拿到「平台确认离线」凭据的 stream_ended 才是真下播；没有凭据的
/// 干净收尾既不能发结束通知，也不能写静默，还要计入连续失败。
fn unconfirmed_end(final_status: &str, reason: &str, confirmed_end: bool) -> bool {
    final_status == "finished" && reason == "stream_ended" && !confirmed_end
}

/// 稳定录制够久就该把续录预算补满（A2）。
///
/// 预算是「这一场直播允许断几次」，原本终身制：一场 6 小时直播抖三次之后
/// 就再也续不上，后面的正常断流全变成收尾。
fn should_reset_budget(elapsed: Duration, restarts: u32, eof_retries: u32) -> bool {
    elapsed >= HEALTHY_RESET && (restarts > 0 || eof_retries > 0)
}

/// 崩溃遗留 ffmpeg 的 pid 文件：与视频同名，扩展名换成 .ffmpeg.pid。
fn pid_file(path: &Path) -> PathBuf {
    path.with_extension("ffmpeg.pid")
}

fn write_pid_file(path: &Path, pid: Option<u32>) -> std::io::Result<()> {
    use std::io::Write;
    let pid = pid.ok_or_else(|| std::io::Error::other("录制进程没有 PID"))?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    write!(file, "{pid}")?;
    file.as_file().sync_all()?;
    file.persist(pid_file(path)).map_err(|e| e.error)?;
    Ok(())
}

fn remove_pid_file(path: &Path) {
    let _ = std::fs::remove_file(pid_file(path));
}

/// 启动时清理上次崩溃留下的 ffmpeg（A6）。
///
/// 崩溃重启后旧进程还活着：它继续往 .part 分片里写，而启动恢复流程正在
/// 把这些分片合成 mp4，结果是「合成到一半又被追加」，产物直接坏掉。
/// 必须先杀。pid 可能被系统复用，所以还要核对命令行里确实有这条录像的
/// 文件名，避免误杀用户自己的 ffmpeg。
pub fn reap_orphans(db: &Db) -> Result<HashSet<String>, AppError> {
    let mut deferred = HashSet::new();
    for (id, file) in db.recording_sessions()? {
        let path = Path::new(&file);
        if !path.is_absolute() {
            deferred.extend(db.session_group_ids(&id)?);
            continue;
        }
        let marker = pid_file(path);
        let safe = match std::fs::read_to_string(&marker) {
            Ok(raw) => raw
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid > 0)
                .is_some_and(|pid| crate::orphan::stop_if_owned(pid, path)),
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound
                    && path.parent().is_some_and(Path::is_dir)
                {
                    crate::orphan::stop_unmarked(path)
                } else {
                    false
                }
            }
        };
        if safe {
            let _ = std::fs::remove_file(marker);
        } else {
            tracing::warn!(session_id = %id, "录制进程尚未确认退出，延后整场恢复");
            deferred.extend(db.session_group_ids(&id)?);
        }
    }
    Ok(deferred)
}

/// 监控开录前的复查条件（R3）：房间还在、且没被暂停监控。
fn monitor_start_allowed(removed: bool, paused: bool) -> bool {
    !removed && !paused
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
    segment_secs: u32,
) -> tokio::process::Command {
    let mut command = crate::process::background_async(ffmpeg::binary());
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("warning")
        .arg("-y");
    // 30 秒收不到任何数据就退出，交给续录逻辑重连。
    // 没有它时源「连着但不给数据」会把 ffmpeg 永久挂住（S4）。
    command.arg("-rw_timeout").arg("30000000");
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
        .arg(segment_secs.to_string())
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

/// 每路 ffmpeg 保留最近这么多行 stderr，异常收尾时一次性打进 WARN。
const STDERR_TAIL_LINES: usize = 40;

/// 把 ffmpeg 的 stderr 读空，同时留一圈尾部。
///
/// 不读会死锁：管道缓冲区（约 64KB）写满后 ffmpeg 就卡在写日志上，
/// 表现是「录到一半没声音也没退出」。逐行进 debug（默认过滤掉），
/// 但非 0 退出 / 假死时把尾部提成 WARN，否则用户只能看到一句「异常退出」。
fn drain_stderr(child: &mut Child) -> Arc<Mutex<VecDeque<String>>> {
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let Some(stderr) = child.stderr.take() else {
        return tail;
    };
    let sink = tail.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            tracing::debug!(target: "ffmpeg", %line, "录制 ffmpeg");
            if let Ok(mut tail) = sink.lock() {
                let mut line: String = line.chars().take(400).collect();
                if line.len() < line.chars().count() {
                    line.push('…');
                }
                tail.push_back(line);
                while tail.len() > STDERR_TAIL_LINES {
                    tail.pop_front();
                }
            }
        }
    });
    tail
}

fn tail_text(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    let Ok(tail) = tail.lock() else {
        return String::new();
    };
    let joined = tail.iter().cloned().collect::<Vec<_>>().join("\n");
    let chars: Vec<char> = joined.chars().collect();
    if chars.len() <= 8000 {
        joined
    } else {
        chars[chars.len() - 8000..].iter().collect()
    }
}

/// 分割收尾清单：写完一段就落一次盘，崩溃后据此续做，不会把首片段覆盖掉。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SplitManifest {
    version: u8,
    session_id: String,
    room_id: String,
    source: String,
    status: String,
    reason: String,
    error: Option<String>,
    quality: Option<String>,
    /// 本场各时段的实际画质；旧清单没有该字段时为空
    #[serde(default)]
    quality_history: Vec<QualitySegment>,
    line: Option<String>,
    /// 开录时快照的后处理配置；v1 清单没有这个字段，反序列化时用 source 模式兜底。
    #[serde(default)]
    post: crate::post::PostConfig,
    clips: Vec<ManifestClip>,
    /// 已完成渲染的片段数（前 rendered 个的 path 已可播放）。
    rendered: usize,
    completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestClip {
    id: String,
    /// 计划输出路径；恢复时按它重跑合并。
    output: String,
    /// 源封装母带的实际路径（合并失败会降级成 .ts，与 output 不同）。
    path: String,
    /// 后处理产物的最终路径；后处理失败时与 path 相同。
    #[serde(default)]
    final_path: Option<String>,
    /// 后处理结果状态；旧清单没有该字段时按 done 处理。
    #[serde(default)]
    post_state: Option<String>,
    /// 后处理失败原因，恢复后写入数据库。
    #[serde(default)]
    post_error: Option<String>,
    start: u32,
    end: Option<u32>,
    reason: Option<String>,
    started_at: i64,
    ended_at: i64,
}

/// 一个待生成的片段：分片编号范围 + 切分原因 + 估算时间。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClipPlan {
    start: u32,
    end: Option<u32>,
    reason: Option<String>,
    started_at: i64,
    ended_at: i64,
}

/// 封口阶段已经合并落盘的片段：源封装母带 + 元数据。
///
/// 后处理在 finalizing 释放之后才跑：合并/切片段已经完成，源文件不会再变，
/// 但转封装/自定义命令可能持续几分钟，不该继续挡住同房间的新一场录制。
#[derive(Debug, Clone)]
struct PreparedClip {
    source_path: PathBuf,
    source_size: i64,
    reason: Option<String>,
    started_at: i64,
    ended_at: i64,
}

/// 收尾后真正落盘的片段。
#[derive(Debug, Clone)]
struct RenderedClip {
    /// 源封装母带（后处理删除源之前必须保留）。
    source_path: PathBuf,
    /// 媒体库实际展示/播放的最终文件。
    path: PathBuf,
    size: i64,
    post_state: String,
    post_error: Option<String>,
    /// 数据库提交成功后才执行的源文件回收。
    delete_source: Option<PathBuf>,
    reason: Option<String>,
    started_at: i64,
    ended_at: i64,
}

fn part_epoch(part: &Path) -> i64 {
    std::fs::metadata(part)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_else(crate::db::now_ts)
}

/// 按已落分片规划片段。标题变化映射到分片结束时刻，时长/大小按分片累计；
/// 边界只落在分片之间，所以不会打断正在写入的 ffmpeg。
fn clip_plans(active: &Active) -> Vec<ClipPlan> {
    let source = PathBuf::from(&active.recording.file_path);
    let parts = segment::collect_parts(&source);
    let metas: Vec<(u32, u64, i64)> = parts
        .iter()
        .filter_map(|part| {
            let index = segment::part_index(part)?;
            let bytes = std::fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
            Some((index, bytes, part_epoch(part)))
        })
        .collect();
    if metas.is_empty() {
        return vec![ClipPlan {
            start: 0,
            end: None,
            reason: None,
            started_at: active.recording.started_at,
            ended_at: crate::db::now_ts(),
        }];
    }
    let plans = plan_clips(
        &metas,
        active.recording.started_at,
        &active.split,
        &active.title_changes,
        crate::db::now_ts(),
    );
    for plan in &plans {
        if let Some(reason) = plan.reason.as_deref() {
            tracing::info!(
                source = %source.display(),
                start = plan.start,
                end = plan.end.unwrap_or(0),
                reason,
                "收尾分割：在分片边界切出片段"
            );
        }
    }
    plans
}

/// 纯函数版本，方便覆盖标题 / 时长 / 体积三种边界。
fn plan_clips(
    metas: &[(u32, u64, i64)],
    started_at: i64,
    split: &SplitPolicy,
    title_changes: &[i64],
    now: i64,
) -> Vec<ClipPlan> {
    if metas.is_empty() {
        return vec![ClipPlan {
            start: 0,
            end: None,
            reason: None,
            started_at,
            ended_at: now,
        }];
    }
    let split_enabled = split.on_title || split.max_duration.is_some() || split.max_size.is_some();
    if !split_enabled {
        return vec![ClipPlan {
            start: metas[0].0,
            end: None,
            reason: None,
            started_at,
            ended_at: now,
        }];
    }

    let mut plans = Vec::new();
    let mut start_pos = 0usize;
    let mut clip_started = started_at;
    let mut bytes = 0u64;
    let mut pending_reason: Option<String> = None;
    // metas 的第三个字段是该分片最后一次写入的时间；它比实际分片结束可能滞后
    // 数秒（ffmpeg 关闭文件时才刷新），所以时长边界改用标称分片长度推进：
    // `-segment_time N` 的每一片就是 N 秒，不受 mtime 抖动影响。边界只能落在
    // 两片之间，标题变化仍按分片实际时间匹配。
    for (pos, (_index, size, ended)) in metas.iter().enumerate() {
        bytes = bytes.saturating_add(*size);
        if pos + 1 == metas.len() {
            break;
        }
        let span = (pos - start_pos + 1) as u64;
        let nominal_end = clip_started.saturating_add((span * split.segment_secs as u64) as i64);
        // 分片 mtime 是 ffmpeg 最后一次写入，常常比这一片真正结束早几秒；
        // 标题恰好在这几秒内变化时，只看 mtime 会把边界拖到下一片。
        // 取标称结束与 mtime 的较大值，既覆盖 mtime 偏早，也不丢延迟关闭的分片。
        let title_end = (*ended).max(nominal_end);
        let title_hit = split.on_title
            && title_changes
                .iter()
                .any(|change| *change > clip_started && *change <= title_end);
        let duration_hit = split.max_duration.is_some_and(|limit| {
            nominal_end.saturating_sub(clip_started) as u64 >= limit.as_secs()
        });
        let size_hit = split.max_size.is_some_and(|limit| bytes >= limit);
        let reason = if title_hit {
            Some("title")
        } else if duration_hit {
            Some("duration")
        } else if size_hit {
            Some("size")
        } else {
            None
        };
        if let Some(reason) = reason {
            // 边界是第 pos+1 片的起点，标称时间比 mtime 更接近真实边界：
            // mtime 是 ffmpeg 最后一次写入，通常比这一片真正结束早几秒。
            let boundary_at = nominal_end;
            plans.push(ClipPlan {
                start: metas[start_pos].0,
                end: Some(metas[pos + 1].0),
                reason: pending_reason.take(),
                started_at: clip_started,
                ended_at: boundary_at,
            });
            start_pos = pos + 1;
            clip_started = boundary_at;
            bytes = 0;
            pending_reason = Some(reason.to_string());
        }
    }
    plans.push(ClipPlan {
        start: metas[start_pos].0,
        end: None,
        reason: pending_reason,
        started_at: clip_started,
        ended_at: now,
    });
    plans
}

/// 后续片段沿用原文件名加 .clipNN 后缀，仍然走原子占位避免撞名。
fn clip_output_path(source: &Path, index: usize) -> std::io::Result<PathBuf> {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "clip".into());
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    // 后续片段与母带同容器：源是 FLV 就切出 FLV，是 TS 就切出 TS。
    let ext = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("ts");
    let candidate = parent.join(format!("{stem}.clip{index:02}.{ext}"));
    crate::filename::reserve_unique_path(&candidate)
}

/// 把每个片段容器的实际时长累加成视频时间轴区间；任一段探测失败就整体退回标称时间。
fn clip_windows(durations: &[Option<i64>]) -> Option<Vec<(i64, i64)>> {
    if durations.is_empty() || durations.iter().any(Option::is_none) {
        return None;
    }
    let mut cursor = 0i64;
    let mut windows = Vec::with_capacity(durations.len());
    for duration in durations {
        let duration = duration.unwrap_or(0).max(0);
        windows.push((cursor, cursor + duration));
        cursor += duration;
    }
    Some(windows)
}

fn manifest_path(source: &Path) -> PathBuf {
    source.with_extension("split.json")
}

fn write_manifest(path: &Path, manifest: &SplitManifest) -> Result<(), String> {
    let body = serde_json::to_string_pretty(manifest).map_err(|err| err.to_string())?;
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| e.to_string())?;
    tmp.write_all(body.as_bytes())
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    tmp.persist(path)
        .map_err(|e| format!("提交 {}: {}", path.display(), e.error))?;
    Ok(())
}

#[cfg(any(windows, test))]
pub(crate) fn relocate_manifest(file: &Path, previous: &Path, root: &Path) -> Result<(), AppError> {
    let Ok(mut manifest) = read_manifest(file) else {
        return Ok(());
    };
    let mut changed = false;
    let mut relocate = |value: &mut String| {
        if let Some(path) = crate::paths::relocated(Path::new(value), previous, root) {
            *value = path.to_string_lossy().into_owned();
            changed = true;
        }
    };
    relocate(&mut manifest.source);
    for clip in &mut manifest.clips {
        relocate(&mut clip.output);
        relocate(&mut clip.path);
        if let Some(path) = &mut clip.final_path {
            relocate(path);
        }
    }
    if changed {
        write_manifest(file, &manifest).map_err(AppError::Internal)?;
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<SplitManifest, String> {
    let body =
        std::fs::read_to_string(path).map_err(|err| format!("读 {}: {err}", path.display()))?;
    serde_json::from_str(&body).map_err(|err| format!("解析 {}: {err}", path.display()))
}

/// 只有数据库提交成功后才允许删除源封装母带；删除失败保留源，不能丢录像。
async fn delete_source_after_commit(db: &Db, id: &str, source: Option<&Path>) {
    let Some(source) = source else { return };
    if !source.is_file() {
        // 用户/清理工具已经拿走了源：DB 标记为已删即可，别再报错。
        let _ = db.set_source_deleted(id, true);
        return;
    }
    match crate::post::trash(source) {
        Ok(()) => {
            if let Err(err) = db.set_source_deleted(id, true) {
                tracing::warn!(%err, id, file = %source.display(), "源已移入回收站但更新数据库失败");
            }
        }
        Err(err) => tracing::warn!(
            id,
            file = %source.display(),
            %err,
            "源封装母带移入回收站失败，保留文件"
        ),
    }
}

/// 按范围收尾：先 concat 合成源封装母带，失败才退化为字节拼接的 .ts。
async fn finish_range(base: &Path, output: &Path, start: u32, end: Option<u32>) -> (PathBuf, i64) {
    // 成品已完整时直接复用。恢复路径通常已经提前判断，这里再兜一层，
    // 避免 merge 因「目标已存在」失败后又走兜底把完整 mp4 删掉换成 .ts。
    if let Ok(meta) = tokio::fs::metadata(output).await {
        if meta.is_file() && meta.len() > 0 {
            return (output.to_path_buf(), meta.len() as i64);
        }
    }
    match segment::merge_range(base, output, start, end).await {
        Ok(size) => return (output.to_path_buf(), size as i64),
        Err(err) => {
            tracing::warn!(%err, path = %output.display(), "片段合成源封装失败，改用字节拼接")
        }
    }
    match segment::fallback_concat_range(base, output, start, end).await {
        Ok((target, size)) => {
            // 占位文件是给 mp4 准备的；兜底成 .ts 后别留 0 字节空壳。
            if target != output && output.metadata().is_ok_and(|m| m.is_file() && m.len() == 0) {
                let _ = std::fs::remove_file(output);
            }
            (target, size as i64)
        }
        Err(err) => {
            // 合并和字节拼接都失败：分片还在，绝不能再把 0 字节的 mp4 占位
            // 当成成品写进历史。返回 0 让调用方保留清单/分片，下次启动重试。
            let total: u64 = segment::collect_parts_range(base, start, end)
                .iter()
                .filter_map(|part| std::fs::metadata(part).ok())
                .map(|meta| meta.len())
                .sum();
            tracing::warn!(
                %err,
                path = %output.display(),
                parts_bytes = total,
                "片段拼接失败，保留分片等待恢复"
            );
            (output.to_path_buf(), 0)
        }
    }
}

async fn remove_range_parts(source: &Path, start: u32, end: Option<u32>) {
    for part in segment::collect_parts_range(source, start, end) {
        let _ = tokio::fs::remove_file(part).await;
    }
}

/// 同一份收尾清单只允许一个手动重试，避免两个任务同时写清单/片段。
/// 分组录制的多个片段共用一份清单，锁必须按清单而不是 session id。
static FINALIZE_RETRYING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub(crate) struct FinalizeRetryLease {
    manifest: String,
    /// 跨进程文件锁：进程退出时由内核释放，锁文件本身可以留着。
    _lock: Option<std::fs::File>,
}

impl Drop for FinalizeRetryLease {
    fn drop(&mut self) {
        if let Some(set) = FINALIZE_RETRYING.get() {
            if let Ok(mut guard) = set.lock() {
                guard.remove(&self.manifest);
            }
        }
    }
}

/// 尝试占用一份收尾清单；成功时返回的 lease 在离开作用域后自动释放。
///
/// 公开给命令层，让「重试收尾」在创建任务之前就拿到锁：拿不到锁时直接
/// 返回错误，不会留下一条假的失败任务。
pub(crate) fn acquire_finalize_retry(manifest: &str) -> Result<FinalizeRetryLease, AppError> {
    let mut lease = reserve_finalize(manifest)?;
    lease._lock = Some(lock_manifest_file(manifest)?);
    Ok(lease)
}

fn reserve_finalize(manifest: &str) -> Result<FinalizeRetryLease, AppError> {
    let set = FINALIZE_RETRYING.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = set
        .lock()
        .map_err(|_| AppError::Internal("收尾重试锁被污染".into()))?;
    if !guard.insert(manifest.to_string()) {
        return Err(AppError::User("这一场正在重试收尾，请稍候".into()));
    }
    Ok(FinalizeRetryLease {
        manifest: manifest.to_string(),
        _lock: None,
    })
}

fn lock_manifest_file(manifest: &str) -> Result<std::fs::File, AppError> {
    let path = format!("{manifest}.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| AppError::from_io(err, Path::new(&path)))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(AppError::User("这一场正在别的窗口重试收尾，请稍候".into()))
        }
        Err(err) => Err(AppError::from_io(err.into(), Path::new(&path))),
    }
}

/// 手动重试一次失败的收尾：只处理该场次留下的分割清单，不碰其它录像。
///
/// 收尾失败时清单会保留，里面记着每个片段的分片范围、已生成的源封装和
/// 后处理状态；这里复用启动恢复的逻辑，已完成的片段直接沿用，缺失的补齐。
///
/// 调用方必须先通过 acquire_finalize_retry 拿到清单锁，并在创建任务之前
/// 完成这一步；这样锁冲突会直接返回错误，不会留下假的失败任务。
pub(crate) async fn retry_finalize_with_lease(
    db: &Db,
    session_id: &str,
    lease: FinalizeRetryLease,
) -> Result<(), AppError> {
    let manifest_path = PathBuf::from(&lease.manifest);
    tracing::info!(
        session = %session_id,
        manifest = %manifest_path.display(),
        "手动重试录制收尾"
    );
    match recover_split_manifest(db, &manifest_path, session_id).await {
        Some((recovered, bytes)) if !manifest_path.exists() => {
            tracing::info!(
                session = %session_id,
                clips = recovered,
                bytes,
                "手动重试收尾完成"
            );
            Ok(())
        }
        Some((recovered, bytes)) => {
            tracing::warn!(
                session = %session_id,
                clips = recovered,
                bytes,
                "手动重试收尾仍未完成，清单已保留"
            );
            Err(AppError::User(
                "收尾仍未完成，缺失的分片或源文件可能已被清理".into(),
            ))
        }
        None => {
            tracing::warn!(session = %session_id, "手动重试收尾失败：清单不可读");
            Err(AppError::User("收尾清单损坏，无法继续重试".into()))
        }
    }
}

/// 按清单恢复一次分割收尾：已渲染的片段直接沿用，缺失的按分片范围重跑。
///
/// 返回 None 表示清单不可读，应保留整组和登记等待重试。
async fn recover_split_manifest(db: &Db, file: &Path, session_id: &str) -> Option<(usize, u64)> {
    let mut manifest = match read_manifest(file) {
        Ok(manifest) => manifest,
        Err(err) => {
            tracing::warn!(%err, file = %file.display(), "读取分割清单失败");
            return None;
        }
    };
    if manifest.session_id != session_id || manifest.clips.is_empty() {
        return None;
    }
    if manifest.completed {
        if db.set_split_manifest(&manifest.session_id, None).is_ok() {
            let _ = std::fs::remove_file(file);
        }
        return Some((0, 0));
    }

    // 清单里可能有多个片段要补后处理：作用域覆盖整轮，取消一次就停掉后面的片段。
    let _post_scope = crate::post::begin_session(&manifest.session_id);
    let source = PathBuf::from(&manifest.source);
    let mut recovered = 0usize;
    let mut bytes = 0u64;
    let mut db_ok = true;
    for index in 0..manifest.clips.len() {
        let clip = manifest.clips[index].clone();
        let planned = PathBuf::from(&clip.output);
        // 先找源封装母带。清单里 path 记录的就是源封装（v1 清单则是旧的 mp4 成品）。
        let mut source_path: Option<PathBuf> = None;
        let mut source_size = 0i64;
        for candidate in [PathBuf::from(&clip.path), planned.clone()] {
            if clip.final_path.as_deref() == candidate.to_str()
                && candidate != Path::new(&clip.path)
            {
                continue;
            }
            if let Ok(meta) = tokio::fs::metadata(&candidate).await {
                if meta.is_file() && meta.len() > 0 {
                    tracing::warn!(
                        session = %manifest.session_id,
                        clip = index,
                        file = %candidate.display(),
                        "发现已生成的片段源封装，直接沿用"
                    );
                    source_path = Some(candidate);
                    source_size = meta.len() as i64;
                    break;
                }
            }
        }

        let mut final_path = clip
            .final_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.metadata().is_ok_and(|m| m.is_file() && m.len() > 0));
        // 清单里已有 final_path 时，保留上次后处理结果；失败也要保留 failed，
        // 不能在恢复时被误标成 done。
        let mut post_state = clip
            .post_state
            .clone()
            .unwrap_or_else(|| crate::post::STATE_DONE.to_string());
        let mut post_error = clip.post_error.clone();
        let mut delete_source = source_path.clone().filter(|src| {
            manifest.post.delete_source
                && manifest.post.mode == crate::settings::POST_MODE_CONVERT
                && post_state == crate::post::STATE_DONE
                && final_path.as_ref().is_some_and(|target| target != src)
        });

        if source_size <= 0 && final_path.is_none() {
            let (target, got) = finish_range(&source, &planned, clip.start, clip.end).await;
            if got > 0 {
                source_path = Some(target);
            }
        }
        if final_path.is_none() {
            let Some(src) = source_path.as_deref() else {
                tracing::warn!(
                    session = %manifest.session_id,
                    clip = index,
                    file = %planned.display(),
                    "分割恢复失败：没有可用分片，继续尝试后续片段"
                );
                db_ok = false;
                let _ = write_manifest(file, &manifest);
                continue;
            };
            // 崩溃可能发生在「母带已合并、后处理还没跑完」之间：从源重新执行。
            let task_id = format!("auto:{}:manifest:{index}", manifest.session_id);
            let outcome =
                crate::post::run(&task_id, &manifest.session_id, src, &manifest.post, None).await;
            post_state = outcome.state;
            post_error = outcome.error;
            delete_source = outcome.delete_source;
            final_path = Some(outcome.path);
        }
        let Some(final_path) = final_path else {
            db_ok = false;
            continue;
        };
        let size = tokio::fs::metadata(&final_path)
            .await
            .ok()
            .filter(|m| m.is_file())
            .map(|meta| meta.len() as i64)
            .unwrap_or(0);
        if size <= 0 {
            tracing::warn!(
                session = %manifest.session_id,
                clip = index,
                file = %final_path.display(),
                "分割恢复失败：最终成品为空，继续尝试后续片段"
            );
            db_ok = false;
            let _ = write_manifest(file, &manifest);
            continue;
        }

        manifest.clips[index].path = source_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| clip.path.clone());
        manifest.clips[index].final_path = Some(final_path.display().to_string());
        manifest.clips[index].post_state = Some(post_state.clone());
        manifest.clips[index].post_error = post_error.clone();
        manifest.rendered = index + 1;
        if let Err(err) = write_manifest(file, &manifest) {
            tracing::warn!(%err, file = %file.display(), "更新分割恢复清单失败");
            db_ok = false;
            continue;
        }

        let existing = match db.get_session(&clip.id) {
            Ok(existing) => existing,
            Err(err) => {
                tracing::warn!(%err, id = %clip.id, "恢复片段时读取数据库失败");
                db_ok = false;
                continue;
            }
        };
        let source_text = source_path.as_ref().map(|path| path.display().to_string());
        let db_result = if let Some(existing) = existing.as_ref() {
            // 每个片段都要补分组：旧库/早期版本可能只有首片段写了 group_id，
            // 后续片段缺 group_id 时媒体库会把它们拆成单独一场。
            if manifest.clips.len() > 1 {
                if let Err(err) = db.set_session_group(
                    &clip.id,
                    &manifest.session_id,
                    index as i64,
                    clip.reason.as_deref(),
                ) {
                    tracing::warn!(%err, id = %clip.id, clip = index, "恢复片段分组失败");
                    db_ok = false;
                    continue;
                }
            }
            let mut count = existing.danmaku_count;
            let mut danmaku_path = existing.danmaku_path.clone();
            // In a split recording the original track can still span all clips.
            // Only a single-clip receipt unambiguously owns the complete writer track.
            if manifest.clips.len() == 1 {
                if let Some(track) = existing.danmaku_path.as_deref().map(Path::new) {
                    match crate::postprocess::recovered_chat_count(track) {
                        Ok(n) => {
                            count = n;
                            if n > 0 {
                                if let Err(err) = crate::postprocess::recover_danmaku_ass(track, n)
                                {
                                    tracing::warn!(%err, "恢复弹幕字幕失败，保留清单重试");
                                    post_error = Some(format!(
                                        "{}恢复弹幕字幕失败：{err}",
                                        post_error
                                            .as_ref()
                                            .map(|s| format!("{s}；"))
                                            .unwrap_or_default()
                                    ));
                                    db_ok = false;
                                }
                            }
                        }
                        Err(err)
                            if err.kind() == std::io::ErrorKind::NotFound
                                && track.parent().is_some_and(Path::is_dir) =>
                        {
                            // 父目录在线而 .jsonl 确实已缺失（如用户手动删除或退出时清理了 0 字节空弹幕）：
                            // 清空失效路径并按 0 条放行，避免收尾清单永不清零并反复触发恢复。
                            tracing::warn!(
                                path = %track.display(),
                                "弹幕文件已缺失，跳过弹幕恢复并继续完成收尾"
                            );
                            count = 0;
                            danmaku_path = None;
                        }
                        Err(err) => {
                            tracing::warn!(%err, "读取弹幕文件失败，保留清单重试");
                            post_error = Some(format!(
                                "{}恢复弹幕字幕失败：{err}",
                                post_error
                                    .as_ref()
                                    .map(|s| format!("{s}；"))
                                    .unwrap_or_default()
                            ));
                            db_ok = false;
                        }
                    }
                }
            }
            db.finish_session_with_post(
                &clip.id,
                &manifest.status,
                &manifest.reason,
                clip.ended_at,
                &final_path.display().to_string(),
                size,
                source_text.as_deref().or(existing.source_path.as_deref()),
                &manifest.post.mode,
                &post_state,
                existing.source_deleted,
                post_error.as_deref(),
                danmaku_path.as_deref(),
                count,
                manifest.error.as_deref(),
            )
        } else {
            let session = RecordSession {
                id: clip.id.clone(),
                room_id: manifest.room_id.clone(),
                started_at: clip.started_at,
                ended_at: Some(clip.ended_at),
                status: manifest.status.clone(),
                reason: Some(manifest.reason.clone()),
                file_path: Some(final_path.display().to_string()),
                file_size: size,
                duration_ms: (clip.ended_at - clip.started_at).max(0) * 1000,
                quality: manifest.quality.clone(),
                quality_history: manifest.quality_history.clone(),
                line: manifest.line.clone(),
                error: manifest.error.clone(),
                danmaku_path: None,
                danmaku_count: 0,
                source_path: source_text.clone(),
                post_mode: manifest.post.mode.clone(),
                post_state: post_state.clone(),
                source_deleted: false,
                post_error: post_error.clone(),
                group_id: None,
                clip_index: 0,
            };
            // 首片段行不存在通常意味着数据库被重建过；create_clip 会补上
            // group_id/clip_index，finish_session_with_post 对不存在的行是静默 0 行更新。
            db.create_clip(
                &session,
                &manifest.session_id,
                index as i64,
                clip.reason.as_deref(),
            )
        };
        match db_result {
            Ok(()) => {
                if source_text.is_none() {
                    let _ = db.set_source_deleted(&clip.id, true);
                }
                delete_source_after_commit(db, &clip.id, delete_source.as_deref()).await;
            }
            Err(err) => {
                db_ok = false;
                tracing::warn!(%err, clip = index, "恢复片段历史失败");
                continue;
            }
        }
        remove_range_parts(&source, clip.start, clip.end).await;
        recovered += 1;
        bytes += size as u64;
    }

    if recovered == manifest.clips.len() && db_ok {
        manifest.completed = true;
        if db.set_split_manifest(&manifest.session_id, None).is_ok() {
            let _ = std::fs::remove_file(file);
        } else {
            let _ = write_manifest(file, &manifest);
        }
    }
    tracing::info!(
        session = %manifest.session_id,
        clips = recovered,
        total = manifest.clips.len(),
        bytes,
        db_ok,
        "按分割清单恢复崩溃收尾"
    );
    Some((recovered, bytes))
}

/// 启动时恢复上次崩溃留下的分片。
///
/// 在 close_orphan_sessions 之后调用（I2）：崩溃会话已被标成 aborted，
/// 先处理有清单的场次，再逐页检查其余失败或中止记录。
/// 由 state.rs 放在后台任务里跑，不挡窗口出现。
pub async fn recover_parts(db: &Db, deferred: &HashSet<String>) -> (usize, u64) {
    let mut recovered = 0usize;
    let mut bytes = 0u64;
    let mut handled = deferred.clone();
    let post = crate::post::PostConfig::from_settings(&crate::settings::load(db));
    let rows = match db.manifest_sessions() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(%err, "读取分割清单失败");
            return (0, 0);
        }
    };
    for (id, file) in rows {
        match db.session_group_ids(&id) {
            Ok(group) => handled.extend(group),
            Err(err) => {
                tracing::warn!(%err, "查询待恢复分组失败");
                return (recovered, bytes);
            }
        }
        if deferred.contains(&id) || !Path::new(&file).is_absolute() {
            continue;
        }
        let Ok(_lease) = acquire_finalize_retry(&file) else {
            continue;
        };
        if let Some((count, size)) = recover_split_manifest(db, Path::new(&file), &id).await {
            recovered += count;
            bytes += size;
        } else if let Err(err) = db.mark_manifest_unreadable(&id, &file) {
            tracing::warn!(%err, %id, "无法记录清单读取错误");
        }
    }

    // Only terminal sessions are eligible. The monitor can already be starting new recordings.
    let mut cursor = None;
    loop {
        let rows = match db.unfinished_sessions(cursor.as_deref()) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(%err, "读取待恢复会话失败");
                break;
            }
        };
        let Some((last, _)) = rows.last() else {
            break;
        };
        cursor = Some(last.clone());
        for (id, file) in rows {
            if handled.contains(&id) || !Path::new(&file).is_absolute() {
                continue;
            }
            match recover_terminal_session(db, &id, Path::new(&file), &post).await {
                Ok(Some((count, size))) => {
                    recovered += count;
                    bytes += size;
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, %id, "恢复录像失败，保留记录重试"),
            }
        }
    }
    (recovered, bytes)
}

/// Use the same durable output receipt as split finalization, including after a database error.
async fn recover_terminal_session(
    db: &Db,
    id: &str,
    path: &Path,
    post: &crate::post::PostConfig,
) -> Result<Option<(usize, u64)>, AppError> {
    let Some(session) = db.get_session(id)? else {
        return Ok(None);
    };
    let parts = segment::collect_parts(path);
    let complete = path.metadata().is_ok_and(|m| m.is_file() && m.len() > 0);
    let registered = session.file_size > 0
        && matches!(
            session.post_state.as_str(),
            crate::post::STATE_DONE | crate::post::STATE_FAILED | crate::post::STATE_CANCELLED
        );
    let old_chat = match session.danmaku_path.as_deref().map(Path::new) {
        Some(track)
            if parts.is_empty()
                && complete
                && registered
                && session.reason.as_deref() == Some("app_exit")
                && track.is_file()
                && (session.danmaku_count == 0
                    || crate::postprocess::ass_needs_recovery(track, session.danmaku_count)?) =>
        {
            db.session_group_ids(id)?.len() == 1
                && crate::postprocess::recovered_chat_count(track)? > 0
        }
        _ => false,
    };
    if parts.is_empty() && (!complete || (registered && !old_chat)) {
        return Ok(None);
    }
    let file = manifest_path(path);
    let file_name = file.to_string_lossy();
    let _lease = acquire_finalize_retry(&file_name)?;
    if file.try_exists()? {
        let existing = read_manifest(&file).map_err(AppError::Internal)?;
        if existing.session_id != id {
            return Err(AppError::User("同名清单属于其他场次".into()));
        }
    } else {
        let already_done = complete && registered;
        let original = path.display().to_string();
        let clip = ManifestClip {
            id: id.into(),
            output: path.display().to_string(),
            path: if already_done {
                session.source_path.clone().unwrap_or(original)
            } else {
                original
            },
            final_path: already_done.then(|| path.display().to_string()),
            post_state: already_done.then(|| session.post_state.clone()),
            post_error: session.post_error.clone(),
            start: 0,
            end: None,
            reason: None,
            started_at: session.started_at,
            ended_at: session.ended_at.unwrap_or(session.started_at),
        };
        let manifest = SplitManifest {
            version: 2,
            session_id: id.into(),
            room_id: session.room_id,
            source: path.display().to_string(),
            status: session.status,
            reason: session.reason.unwrap_or_default(),
            error: session.error,
            quality: session.quality,
            quality_history: session.quality_history,
            line: session.line,
            post: if already_done {
                crate::post::PostConfig {
                    mode: session.post_mode,
                    delete_source: false,
                    ..post.clone()
                }
            } else {
                post.clone()
            },
            clips: vec![clip],
            rendered: 0,
            completed: false,
        };
        write_manifest(&file, &manifest).map_err(AppError::Internal)?;
    }
    db.set_split_manifest(id, Some(&file_name))?;
    Ok(recover_split_manifest(db, &file, id).await)
}

fn emit_notice(
    app: &AppHandle,
    kind: &str,
    room_id: &str,
    host_name: &str,
    detail: Option<String>,
) {
    let notice = AppNotice {
        kind: kind.to_string(),
        room_id: room_id.to_string(),
        host_name: host_name.to_string(),
        detail,
        suppress: false,
    };
    let _ = app.emit(EVENT_APP_NOTICE, notice);
}

/// 只有分辨率、没有平台 qn 时，把高度折算到统一档位。
///
/// 不能直接用 `height * 10`：1080p 会变成 10800，比「原画 10000」还高，
/// 用户显式选 250/400 时会被误判成超档。这里只做保守映射，宁可低估。
fn qn_from_height(height: u32) -> u32 {
    match height {
        h if h >= 2160 => crate::settings::RECORD_QN_HIGHEST,
        h if h >= 1440 => 10_000,
        h if h >= 1080 => 250,
        h if h >= 720 => 150,
        _ => 80,
    }
}

/// 候选画质：平台 qn 优先；没有 qn 的平台（或字段缺失）退回分辨率。
fn candidate_qn(candidate: &StreamCandidate) -> u32 {
    candidate
        .attrs
        .get("qn")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .or_else(|| candidate.height.map(qn_from_height))
        .unwrap_or(0)
}

/// 0 字节的「正常结束」不能算成功：媒体库里留空文件、任务中心显示完成都会误导。
fn empty_recording_failure(file_size: i64) -> Option<(&'static str, Option<String>)> {
    (file_size <= 0).then(|| ("empty", Some("录制没有产生任何数据".to_string())))
}

/// 编码/源流格式是否真的变了。平台没给 codec 时按「已变化」处理：
/// 未知 → HEVC/AV1 若继续写同一容器，收尾合并可能得到非法文件；宁可多分一段。
fn container_changed(
    current_codec: Option<Codec>,
    next_codec: Option<Codec>,
    current_format: StreamFormat,
    next_format: StreamFormat,
) -> bool {
    current_codec != next_codec || current_format != next_format
}

/// 续录的实际档位是否变了：qn 可用时比 qn，平台没给 qn 时退回画质名。
fn actual_quality_changed(previous_qn: u32, previous: &str, next_qn: u32, next: &str) -> bool {
    if previous_qn > 0 && next_qn > 0 {
        previous_qn != next_qn
    } else {
        previous != next
    }
}

/// 用户明确选了档位，实际却拿到更低档：需要提示，但绝不能因此拒绝录制。
///
/// `requested == 0`（跟随平台）和 `requested == 40000`（最高可用）本来就会
/// 落在平台能给的最好档位，不算降级；`actual == 0` 是平台没给 qn，无法判断。
/// 用平台返回的画质清单顺序比较高低：斗鱼 401/400、虎牙 15000/20000/30000
/// 的裸 qn 并不单调，只有 qualities 的降序位置可靠；缺档时退回数值比较。
fn quality_is_better(plan: &crate::platform::StreamPlan, candidate: u32, current: u32) -> bool {
    let rank = |qn: u32| plan.qualities.iter().position(|quality| quality.qn == qn);
    match (rank(candidate), rank(current)) {
        (Some(next), Some(prev)) => next < prev,
        _ => candidate > current,
    }
}

fn quality_fallback(requested: u32, actual: u32) -> bool {
    requested > 0
        && requested < crate::settings::RECORD_QN_HIGHEST
        && actual > 0
        && actual < requested
}

/// 画质打分：显式档位优先选不高于目标的最高档；目标高于平台上限时直接取最高。
/// 返回 (是否在目标以内, 画质值)，用元组比较即可。
fn quality_rank(qn: u32, requested: u32) -> (u8, u32) {
    if requested == 0 || requested >= crate::settings::RECORD_QN_HIGHEST {
        return (1, qn);
    }
    if qn <= requested {
        (1, qn)
    } else {
        // 没有不高过目标的档位时，宁可超一点也不要落到最低档。
        (0, u32::MAX.saturating_sub(qn))
    }
}

/// 用户明确选了编码，平台却只给得出别的编码。没有编码信息（None）时无法判断，
/// 不告警；录制照常继续，绝不因为编码不符而拒绝开录。
fn codec_fallback(requested: Option<Codec>, actual: Option<Codec>) -> bool {
    requested.is_some() && actual.is_some() && requested != actual
}

/// 用户明确选了源流格式，实际拿到的格式不同。选 HLS 时 TS 视为同一类；
/// 其余不匹配都提示，但同样只降级、不拒绝录制。
fn format_fallback(requested: Option<StreamFormat>, actual: StreamFormat) -> bool {
    match requested {
        None => false,
        Some(want) if want == actual => false,
        Some(StreamFormat::Hls) if actual == StreamFormat::HttpTs => false,
        Some(_) => true,
    }
}

fn codec_rank(codec: Option<Codec>, preferred: Option<Codec>) -> u8 {
    let codec = codec.unwrap_or(Codec::Unknown);
    match preferred {
        Some(want) if codec == want => 3,
        Some(Codec::H264) if codec == Codec::H265 => 1,
        Some(Codec::H265) if codec == Codec::H264 => 1,
        Some(_) => 0,
        // 自动：H.264 兼容性最好，H.265 次之。
        None if codec == Codec::H264 => 2,
        None if codec == Codec::H265 => 1,
        None => 0,
    }
}

fn format_rank(format: StreamFormat, preferred: Option<StreamFormat>) -> u8 {
    if let Some(want) = preferred {
        return match (want, format) {
            // HLS/TS 同时属于 HLS 和 TS，选 HLS 时也应接受它。
            (StreamFormat::Hls, StreamFormat::HttpTs) => 2,
            _ if format == want => 3,
            _ => 0,
        };
    }
    match format {
        StreamFormat::Flv => 4,
        StreamFormat::HttpTs => 3,
        StreamFormat::Hls => 2,
        StreamFormat::Dash => 1,
        StreamFormat::AudioOnly => 0,
    }
}

fn candidate_score(candidate: &StreamCandidate, prefs: StreamPrefs) -> ((u8, u32), u8, u8, u64) {
    let qn = candidate_qn(candidate);
    (
        quality_rank(qn, prefs.qn),
        codec_rank(candidate.codec, prefs.codec),
        format_rank(candidate.format, prefs.format),
        // 同档位时码率高的通常更清晰；最后再按平台权重。
        u64::from(candidate.bitrate_kbps.unwrap_or(0)) * 1000 + candidate.weight.max(0.0) as u64,
    )
}

fn best_in_group(group: &StreamGroup, prefs: StreamPrefs) -> Option<&StreamCandidate> {
    group
        .candidates
        .iter()
        .filter(|candidate| candidate.url.is_some())
        // AV1 不能直接 copy 进 MPEG-TS/Matroska 分片：ffmpeg 会报
        // "Output file does not contain any stream"，产物为 0 字节。
        // 观看仍可走 AV1（MSE 有能力门禁），录制链路暂不支持。
        .filter(|candidate| candidate.codec != Some(Codec::Av1))
        .max_by_key(|candidate| candidate_score(candidate, prefs))
}

/// 直播只给 AV1 时，错误要说明原因，不能笼统报「没有可用的线路」。
fn only_av1_available(groups: &[StreamGroup]) -> bool {
    let mut saw_av1 = false;
    for candidate in groups.iter().flat_map(|group| group.candidates.iter()) {
        if candidate.url.is_none() {
            continue;
        }
        if candidate.codec == Some(Codec::Av1) {
            saw_av1 = true;
        } else {
            return false;
        }
    }
    saw_av1
}

/// 选流：画质 > 编码 > 格式。
///
/// 用户指定了线路就只在那条线上挑；线路上没有可用地址时再退回全局最优，
/// 否则「手动选了一条挂掉的线路」会让录制直接启动失败，反而更难用。
fn pick_candidate<'a>(
    groups: &'a [StreamGroup],
    preferred: Option<&str>,
    prefs: StreamPrefs,
) -> Option<(&'a StreamCandidate, &'a StreamGroup)> {
    if let Some(name) = preferred {
        if let Some(group) = groups.iter().find(|group| group.name == name) {
            if let Some(candidate) = best_in_group(group, prefs) {
                return Some((candidate, group));
            }
            tracing::warn!(line = name, "指定线路没有可用地址，改用其它线路");
        }
    }
    groups
        .iter()
        .filter_map(|group| best_in_group(group, prefs).map(|candidate| (candidate, group)))
        .max_by_key(|(candidate, _)| candidate_score(candidate, prefs))
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

/// 生成录制文件路径：媒体库/[平台/主播]/模板渲染出的文件名.ext
pub fn session_path(
    root: &Path,
    parts: &crate::filename::NameParts<'_>,
    started_at: i64,
    ext: &str,
    naming: &Naming,
) -> PathBuf {
    let mut dir = root.to_path_buf();
    if naming.group_dirs {
        dir.push(parts.platform);
        dir.push(crate::filename::sanitize(
            parts.host,
            "未知主播",
            40,
            naming.strip_emoji,
        ));
    }
    let budget = crate::filename::stem_budget(&dir, ext);
    let stem = crate::filename::render(
        &naming.template,
        parts,
        started_at,
        naming.strip_emoji,
        budget,
    )
    .unwrap_or_else(|_| {
        // 保存设置时已校验；这里只兜底手改库或并发写入留下的坏模板。
        crate::filename::render(
            crate::filename::DEFAULT_TEMPLATE,
            parts,
            started_at,
            naming.strip_emoji,
            budget,
        )
        .unwrap_or_else(|_| "未命名直播".to_string())
    });
    dir.join(format!("{stem}.{ext}"))
}

#[cfg(test)]
#[path = "recorder/recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filters_illegal_chars() {
        let s = |input: &str, fallback: &str| crate::filename::sanitize(input, fallback, 40, false);
        assert_eq!(s("a/b:c*d?e\"f<g>h|i", "x"), "a_b_c_d_e_f_g_h_i");
        assert_eq!(s("  多   空格  ", "x"), "多 空格");
        assert_eq!(s("", "兜底"), "兜底");
        assert_eq!(s("...", "兜底"), "兜底");
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
        let prefs = StreamPrefs {
            qn: 0,
            codec: None,
            format: None,
        };
        let (picked, group) =
            pick_candidate(&groups, Some("线路B"), prefs).expect("应能选到指定线路");
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
        let (picked, group) =
            pick_candidate(&broken, Some("线路B"), prefs).expect("应退回其它线路");
        assert_eq!(group.name, "线路A");
        assert_eq!(picked.url.as_deref(), Some("http://a"));
    }

    #[test]
    fn segment_length_tracks_the_duration_limit() {
        let seconds = |value: u64| segment_for_duration(Duration::from_secs(value));
        assert_eq!(seconds(60), 60);
        assert_eq!(seconds(90), 90);
        assert_eq!(seconds(300), 300);
        // 能整除时正好卡在上限，不会 400 秒切出 600 秒的片段。
        assert_eq!(seconds(400), 200);
        assert_eq!(seconds(600), 300);
        // 除不尽时选超出最少的分片长度。
        assert_eq!(seconds(997), 111);
    }

    #[test]
    fn combined_split_rules_align_the_duration_cap_with_fine_segments() {
        let bounded = |value: u64| segment_for_duration_between(Duration::from_secs(value), 15, 60);
        assert_eq!(bounded(60), 60);
        // 90 秒上限：45 秒分片正好在 90 秒切，不会拖到 120 秒。
        assert_eq!(bounded(90), 45);
        assert_eq!(bounded(100), 50);
        // 除不尽时选超限最少的分片长度。
        assert_eq!(bounded(61), 31);
        assert_eq!(bounded(61) * 2, 62);
    }

    #[test]
    fn stream_choice_prefers_quality_then_codec_then_format() {
        use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};
        let candidate = |name: &str, qn: u32, codec: Codec, format: StreamFormat| StreamCandidate {
            id: name.into(),
            name: name.into(),
            weight: 1.0,
            format,
            codec: Some(codec),
            width: None,
            height: None,
            fps: None,
            bitrate_kbps: None,
            url: Some(format!("http://{name}")),
            headers: Default::default(),
            attrs: [("qn".to_string(), qn.to_string())].into_iter().collect(),
        };
        let groups = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![
                candidate("h265-4k", 40_000, Codec::H265, StreamFormat::Hls),
                candidate("h264-origin", 10_000, Codec::H264, StreamFormat::Flv),
                candidate("h264-250", 250, Codec::H264, StreamFormat::Flv),
                candidate("h264-80", 80, Codec::H264, StreamFormat::Flv),
            ],
        }];
        let prefs = |qn, codec, format| StreamPrefs { qn, codec, format };
        // 画质优先：最高可用即使编码不如 H.264、格式不如 FLV，也先按画质选。
        let (picked, _) = pick_candidate(&groups, None, prefs(0, None, None)).expect("有候选");
        assert_eq!(picked.id, "h265-4k");
        // 显式 250：选不高于目标的最高档。
        let (picked, _) = pick_candidate(&groups, None, prefs(250, None, None)).expect("有候选");
        assert_eq!(picked.id, "h264-250");
        // 同画质下编码偏好优先于格式偏好。
        let same_qn = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![
                candidate("h265", 250, Codec::H265, StreamFormat::Flv),
                candidate("h264", 250, Codec::H264, StreamFormat::Hls),
            ],
        }];
        let (picked, _) =
            pick_candidate(&same_qn, None, prefs(250, Some(Codec::H265), None)).expect("有候选");
        assert_eq!(picked.id, "h265");
        // 编码相同再看格式。
        let same_codec = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![
                candidate("flv", 250, Codec::H264, StreamFormat::Flv),
                candidate("hls", 250, Codec::H264, StreamFormat::Hls),
            ],
        }];
        let (picked, _) =
            pick_candidate(&same_codec, None, prefs(250, None, Some(StreamFormat::Hls)))
                .expect("有候选");
        assert_eq!(picked.id, "hls");
    }

    #[test]
    fn recording_skips_av1_and_reports_av1_only_plans() {
        use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};
        let candidate = |name: &str, codec: Codec| StreamCandidate {
            id: name.into(),
            name: name.into(),
            weight: 1.0,
            format: StreamFormat::Flv,
            codec: Some(codec),
            width: None,
            height: None,
            fps: None,
            bitrate_kbps: None,
            url: Some(format!("http://{name}")),
            headers: Default::default(),
            attrs: Default::default(),
        };
        let prefs = StreamPrefs {
            qn: 0,
            codec: None,
            format: None,
        };
        let mixed = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![candidate("av1", Codec::Av1), candidate("h264", Codec::H264)],
        }];
        let (picked, _) = pick_candidate(&mixed, None, prefs).expect("应跳过 AV1");
        assert_eq!(picked.id, "h264");
        assert!(!only_av1_available(&mixed));

        let av1_only = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![candidate("av1", Codec::Av1)],
        }];
        assert!(pick_candidate(&av1_only, None, prefs).is_none());
        assert!(only_av1_available(&av1_only));
    }

    #[test]
    fn format_preference_distinguishes_ts_from_flv_and_hls() {
        use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};
        let candidate = |id: &str, format: StreamFormat| StreamCandidate {
            id: id.into(),
            name: id.into(),
            weight: 1.0,
            format,
            codec: Some(Codec::H264),
            width: None,
            height: None,
            fps: None,
            bitrate_kbps: None,
            url: Some(format!("http://{id}")),
            headers: Default::default(),
            attrs: [("qn".to_string(), "250".to_string())]
                .into_iter()
                .collect(),
        };
        let groups = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![
                candidate("flv", StreamFormat::Flv),
                candidate("hls-fmp4", StreamFormat::Hls),
                candidate("hls-ts", StreamFormat::HttpTs),
            ],
        }];
        let prefs = |format| StreamPrefs {
            qn: 250,
            codec: None,
            format,
        };
        assert_eq!(
            pick_candidate(&groups, None, prefs(Some(StreamFormat::HttpTs)))
                .expect("有候选")
                .0
                .id,
            "hls-ts"
        );
        assert_eq!(
            pick_candidate(&groups, None, prefs(Some(StreamFormat::Hls)))
                .expect("有候选")
                .0
                .id,
            "hls-fmp4"
        );
        assert_eq!(
            pick_candidate(&groups, None, prefs(None))
                .expect("有候选")
                .0
                .id,
            "flv"
        );
    }

    #[test]
    fn respawn_keeps_the_first_segments_codec_and_format() {
        use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};
        let candidate = |id: &str, codec: Codec, format: StreamFormat| StreamCandidate {
            id: id.into(),
            name: id.into(),
            weight: 1.0,
            format,
            codec: Some(codec),
            width: None,
            height: None,
            fps: None,
            bitrate_kbps: None,
            url: Some(format!("http://{id}")),
            headers: Default::default(),
            attrs: [("qn".to_string(), "250".to_string())]
                .into_iter()
                .collect(),
        };
        let groups = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![
                candidate("h265-flv", Codec::H265, StreamFormat::Flv),
                candidate("h264-hls", Codec::H264, StreamFormat::Hls),
            ],
        }];
        let user = StreamPrefs {
            qn: 250,
            codec: None,
            format: None,
        };
        // 首段是 H.264/HLS：续录即使 auto 也优先同编码同格式，不跟着默认偏好走。
        let pinned = respawn_prefs(250, user, Some(Codec::H264), StreamFormat::Hls);
        let (picked, _) = pick_candidate(&groups, None, pinned).expect("有候选");
        assert_eq!(picked.id, "h264-hls");
        // 平台只剩 H.265/FLV 时仍要续录，不能因为钉不住就放弃。
        let only_h265 = vec![StreamGroup {
            name: "线路".into(),
            priority: 0,
            candidates: vec![candidate("h265-flv", Codec::H265, StreamFormat::Flv)],
        }];
        let (picked, _) = pick_candidate(&only_h265, None, pinned).expect("应降级续录");
        assert_eq!(picked.id, "h265-flv");
    }

    #[test]
    fn quality_fallback_only_when_an_explicit_target_is_missed() {
        assert!(quality_fallback(400, 250));
        assert!(quality_fallback(10_000, 250));
        assert!(!quality_fallback(400, 400));
        assert!(!quality_fallback(250, 400));
        assert!(!quality_fallback(0, 250));
        assert!(!quality_fallback(crate::settings::RECORD_QN_HIGHEST, 250));
        assert!(!quality_fallback(400, 0));
    }

    #[test]
    fn quality_upgrade_uses_platform_order() {
        use crate::platform::StreamPlan;
        use hivecast_ipc::QualityInfo;
        let quality = |qn: u32| QualityInfo {
            qn,
            label: qn.to_string(),
            hint: None,
            available: true,
            codec: None,
        };
        let plan = StreamPlan {
            groups: Vec::new(),
            qualities: vec![quality(40_000), quality(10_000), quality(250)],
        };
        assert!(quality_is_better(&plan, 10_000, 250));
        assert!(!quality_is_better(&plan, 250, 10_000));
        assert!(!quality_is_better(&plan, 250, 250));
    }

    #[test]
    fn zero_byte_finished_recording_is_downgraded() {
        let (reason, error) = empty_recording_failure(0).expect("0 字节应判失败");
        assert_eq!(reason, "empty");
        assert!(error.unwrap().contains("没有产生任何数据"));
        assert!(empty_recording_failure(1).is_none());
    }

    #[test]
    fn container_change_only_when_codec_or_format_really_differs() {
        use crate::platform::{Codec, StreamFormat};
        assert!(container_changed(
            Some(Codec::H264),
            Some(Codec::H265),
            StreamFormat::Flv,
            StreamFormat::Flv
        ));
        assert!(container_changed(
            Some(Codec::H264),
            Some(Codec::H264),
            StreamFormat::Flv,
            StreamFormat::Hls
        ));
        assert!(!container_changed(
            Some(Codec::H264),
            Some(Codec::H264),
            StreamFormat::Flv,
            StreamFormat::Flv
        ));
        assert!(
            container_changed(
                None,
                Some(Codec::H265),
                StreamFormat::Flv,
                StreamFormat::Flv
            ),
            "编码未知 → HEVC 必须分段，不能继续写同一容器"
        );
        assert!(!container_changed(
            None,
            None,
            StreamFormat::Flv,
            StreamFormat::Flv
        ));
    }

    #[test]
    fn respawn_quality_change_compares_actual_not_requested() {
        assert!(actual_quality_changed(10_000, "原画", 250, "超清"));
        assert!(actual_quality_changed(250, "超清", 10_000, "原画"));
        assert!(!actual_quality_changed(250, "超清", 250, "超清"));
        // 平台没给 qn 时退回画质名，避免 0 == 0 掩盖换档。
        assert!(actual_quality_changed(0, "高清", 0, "标清"));
        assert!(!actual_quality_changed(0, "高清", 0, "高清"));
    }

    #[test]
    fn explicit_codec_and_format_fallbacks_are_detected() {
        assert!(codec_fallback(Some(Codec::H265), Some(Codec::H264)));
        assert!(codec_fallback(Some(Codec::H264), Some(Codec::H265)));
        assert!(!codec_fallback(Some(Codec::H264), Some(Codec::H264)));
        assert!(!codec_fallback(None, Some(Codec::H265)), "auto 不算降级");
        assert!(
            !codec_fallback(Some(Codec::H265), None),
            "平台不给编码信息时无法判断"
        );

        assert!(format_fallback(Some(StreamFormat::Flv), StreamFormat::Hls));
        assert!(format_fallback(
            Some(StreamFormat::HttpTs),
            StreamFormat::Hls
        ));
        assert!(!format_fallback(
            Some(StreamFormat::Hls),
            StreamFormat::HttpTs
        ));
        assert!(!format_fallback(Some(StreamFormat::Flv), StreamFormat::Flv));
        assert!(!format_fallback(None, StreamFormat::Hls), "auto 不算降级");
    }

    #[test]
    fn height_fallback_uses_quality_tiers_not_raw_height_times_ten() {
        use crate::platform::{StreamCandidate, StreamFormat};
        let candidate = |height| StreamCandidate {
            id: format!("h{height}"),
            name: format!("{height}p"),
            weight: 1.0,
            format: StreamFormat::Flv,
            codec: None,
            width: None,
            height: Some(height),
            fps: None,
            bitrate_kbps: None,
            url: Some(format!("http://h{height}")),
            headers: Default::default(),
            attrs: Default::default(),
        };
        assert_eq!(candidate_qn(&candidate(2160)), 40_000);
        assert_eq!(candidate_qn(&candidate(1440)), 10_000);
        // 旧实现会得到 10800，比原画 10000 还高，显式选 250 时会被判成超档。
        assert_eq!(candidate_qn(&candidate(1080)), 250);
        assert_eq!(candidate_qn(&candidate(720)), 150);
        assert_eq!(candidate_qn(&candidate(480)), 80);
    }

    #[test]
    fn clip_windows_follow_actual_container_durations() {
        let windows = clip_windows(&[Some(60_384), Some(60_500), Some(19_150)]).unwrap();
        assert_eq!(
            windows,
            vec![(0, 60_384), (60_384, 120_884), (120_884, 140_034)]
        );
        assert!(clip_windows(&[Some(60_384), None]).is_none());
        assert!(clip_windows(&[]).is_none());
    }

    #[test]
    fn split_plan_honors_title_duration_and_size_boundaries() {
        let split = |on_title, duration: Option<u64>, size: Option<u64>| SplitPolicy {
            on_title,
            max_duration: duration.map(Duration::from_secs),
            max_size: size,
            segment_secs: 60,
        };
        // 四片，每片 60 秒 / 100 字节；编号故意不连续，边界仍按实际编号。
        let metas = [(0, 100, 100), (2, 100, 160), (5, 100, 220), (9, 100, 280)];

        let none = plan_clips(&metas, 100, &split(false, None, None), &[], 300);
        assert_eq!(none.len(), 1);
        assert_eq!((none[0].start, none[0].end), (0, None));

        let duration = plan_clips(&metas, 100, &split(false, Some(120), None), &[], 300);
        assert_eq!(duration.len(), 2);
        assert_eq!((duration[0].start, duration[0].end), (0, Some(5)));
        assert_eq!(duration[0].reason, None);
        assert_eq!(duration[1].reason.as_deref(), Some("duration"));

        // 标题在第 1 片内（mtime 100..160）变化：边界应落在第 1 片结束，
        // 不能因为 mtime 偏早就拖到第 2 片。
        let title = plan_clips(&metas, 100, &split(true, None, None), &[150], 300);
        assert_eq!(title.len(), 2);
        assert_eq!((title[0].start, title[0].end), (0, Some(2)));
        assert_eq!(title[1].reason.as_deref(), Some("title"));

        // mtime 比标称结束早 6 秒，标题落在最后 6 秒内也要在第 1 片切。
        let late = plan_clips(&metas, 100, &split(true, None, None), &[158], 300);
        assert_eq!(late.len(), 2);
        assert_eq!((late[0].start, late[0].end), (0, Some(2)));

        let size = plan_clips(&metas, 100, &split(false, None, Some(250)), &[], 300);
        assert_eq!(size.len(), 2);
        assert_eq!((size[0].start, size[0].end), (0, Some(9)));
        assert_eq!(size[1].reason.as_deref(), Some("size"));

        // 最后一片不能再切：边界永远落在两片之间。
        let tiny = plan_clips(&metas[..1], 100, &split(false, Some(60), None), &[], 200);
        assert_eq!(tiny.len(), 1);
    }

    #[test]
    fn split_plan_uses_nominal_segment_length_for_duration() {
        let split = |duration| SplitPolicy {
            on_title: false,
            max_duration: Some(Duration::from_secs(duration)),
            max_size: None,
            segment_secs: 60,
        };
        // mtime 比实际分片结束早 1 秒也不能把 60 秒目标拖成 120 秒一段。
        let jitter = [(0, 100, 59), (1, 100, 119), (2, 100, 179), (3, 100, 239)];
        let plans = plan_clips(&jitter, 0, &split(60), &[], 260);
        assert_eq!(plans.len(), 4);
        assert_eq!((plans[0].start, plans[0].end), (0, Some(1)));
        assert_eq!((plans[1].start, plans[1].end), (1, Some(2)));
        assert_eq!((plans[2].start, plans[2].end), (2, Some(3)));
        assert_eq!(plans[3].reason.as_deref(), Some("duration"));

        // 600 秒目标、300 秒分片，两片标称 600 秒就应切。
        let long = [(0, 100, 299), (1, 100, 598), (2, 100, 897)];
        let long_split = SplitPolicy {
            segment_secs: 300,
            ..split(600)
        };
        let plans = plan_clips(&long, 0, &long_split, &[], 920);
        assert_eq!(plans.len(), 2);
        assert_eq!((plans[0].start, plans[0].end), (0, Some(2)));
    }

    #[test]
    fn session_path_shape() {
        let parts = crate::filename::NameParts {
            platform_id: "bilibili",
            platform: "哔哩哔哩",
            host: "主播",
            title: "标题",
            area: "",
            room_key: "6",
            qn: 10000,
        };
        // 用临时目录而不是 /tmp：Windows 上 /tmp 会被解析成当前盘根目录。
        let root = std::env::temp_dir().join("hivecast-path");
        let naming = Naming::default();
        let path = session_path(&root, &parts, 1_760_000_000, "flv", &naming);
        let rel = path.strip_prefix(&root).expect("必须落在媒体根下");
        let segments: Vec<String> = rel
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_string())
            .collect();
        assert_eq!(segments[0], "哔哩哔哩");
        assert_eq!(segments[1], "主播");
        assert!(segments[2].starts_with("哔哩哔哩-主播-标题-"));
        assert!(segments[2].ends_with(".flv"));
        // 关掉分目录后直接落在根目录。
        let flat = Naming {
            group_dirs: false,
            ..naming.clone()
        };
        let path = session_path(&root, &parts, 1_760_000_000, "flv", &flat);
        assert_eq!(path.parent(), Some(root.as_path()));
    }

    fn test_recorder(tag: &str) -> Recorder {
        let db = Arc::new(Db::open(Path::new(":memory:")).expect("内存库"));
        let _ = tag;
        Recorder::new(db, PathBuf::from("/tmp"), None)
    }

    #[test]
    fn new_session_requires_a_confirmed_offline_first() {
        // 平台单次误报在播不能解除上一场的静默；只有「先离线、后在播」才是新一场（H7）。
        let recorder = test_recorder("h7");
        recorder.mark_ended("r1", None);
        recorder
            .suppressed
            .lock()
            .expect("锁")
            .insert("r1".to_string(), None);
        assert!(!recorder.new_session_after_offline("r1"));
        assert!(recorder.auto_blocked("r1", None));
        assert!(recorder.auto_suppressed("r1", None));

        recorder.note_offline_seen("r1");
        assert!(recorder.new_session_after_offline("r1"));
        assert!(!recorder.auto_blocked("r1", None));
        assert!(!recorder.auto_suppressed("r1", None));
        // 记号只消费一次，不会把之后的静默也一路放行。
        assert!(!recorder.new_session_after_offline("r1"));
        // 没有静默可解时返回 false，不刷「按新一场处理」的日志。
        recorder.note_offline_seen("r2");
        assert!(!recorder.new_session_after_offline("r2"));
    }

    #[test]
    fn starting_rooms_lists_inflight_starts() {
        // 退出流程要看得到「开录中但还没挂进 active」的房间（H6）。
        let recorder = test_recorder("h6");
        assert!(recorder.starting_rooms().is_empty());
        let guard = recorder.begin_start("r1");
        assert_eq!(recorder.starting_rooms(), vec!["r1".to_string()]);
        drop(guard);
        assert!(recorder.starting_rooms().is_empty());
    }

    #[test]
    fn manual_stop_suppresses_auto_start() {
        let recorder = test_recorder("suppress");
        assert!(!recorder.auto_suppressed("r1", Some(100)));
        recorder
            .suppressed
            .lock()
            .expect("锁")
            .insert("r1".to_string(), Some(100));
        assert!(recorder.auto_suppressed("r1", Some(100)));
        // 平台没给开播时间时保守拦截。
        assert!(recorder.auto_suppressed("r1", None));
        recorder.resume_auto("r1");
        assert!(!recorder.auto_suppressed("r1", Some(100)));
    }

    #[test]
    fn manual_stop_only_blocks_the_same_live_session() {
        let recorder = test_recorder("suppress-next");
        recorder
            .suppressed
            .lock()
            .expect("锁")
            .insert("r1".to_string(), Some(100));
        // 换成新一场（开播时间变了）：上一场的停录不该继续挡着。
        assert!(!recorder.auto_suppressed("r1", Some(200)));
        assert!(!recorder.auto_suppressed("r1", Some(100)));
    }

    #[test]
    fn natural_end_blocks_auto_start_until_offline_or_new_live() {
        let recorder = test_recorder("ended");
        recorder.mark_ended("r1", Some(100));
        assert!(recorder.auto_blocked("r1", Some(100)));
        // 成功看到离线后解除。
        recorder.resume_auto("r1");
        assert!(!recorder.auto_blocked("r1", Some(100)));
        // 平台给出新的开播时间也解除。
        recorder.mark_ended("r1", Some(100));
        assert!(!recorder.auto_blocked("r1", Some(200)));
        assert!(!recorder.auto_blocked("r1", Some(100)));
    }

    #[test]
    fn platform_probe_covers_clean_eof_and_half_closed_stream() {
        // 干净 EOF：复核预算没用完就问平台（R1）。
        assert!(should_probe_platform(None, true, false, 0));
        assert!(should_probe_platform(
            None,
            true,
            false,
            MAX_EOF_RETRIES - 1
        ));
        // 复核次数用尽后不再问平台，避免「在播但没流」时无限重连（R1）。
        assert!(!should_probe_platform(None, true, false, MAX_EOF_RETRIES));
        // 非 0 退出但活过了秒退门槛：半关连接也会让 ffmpeg 报错，同样要问（A11）。
        assert!(should_probe_platform(None, false, false, 0));
        // 用户停录、秒退走各自的分支。
        assert!(!should_probe_platform(Some("stopped"), true, false, 0));
        assert!(!should_probe_platform(
            Some("stream_ended"),
            false,
            false,
            0
        ));
        assert!(!should_probe_platform(None, true, true, 0));
    }

    #[test]
    fn healthy_recording_refills_restart_budget() {
        // 刚开录就抖动：预算照常消耗，不重置（否则会被反复抖动刷满）。
        assert!(!should_reset_budget(Duration::from_secs(60), 1, 0));
        // 稳稳录了半小时以上：补满（A2）。
        assert!(should_reset_budget(HEALTHY_RESET, 1, 0));
        assert!(should_reset_budget(HEALTHY_RESET, 0, 1));
        // 没消耗过就不用重置，省掉一次无意义的日志。
        assert!(!should_reset_budget(HEALTHY_RESET, 0, 0));
    }

    #[test]
    fn only_confirmed_offline_unblocks_jitter() {
        let recorder = test_recorder("jitter-plain");
        // 没经过「确认离线」的自然结束：按 S2 挡满 10 分钟。
        recorder.mark_ended("r1", Some(100));
        assert!(recorder.auto_blocked("r1", Some(100)));
    }

    #[test]
    fn jitter_after_confirmed_offline_unblocks_auto_start() {
        let recorder = test_recorder("jitter-confirmed");
        // 监控确认离线后才停的录：记下「确认下播」。
        recorder.note_offline_stop("r1");
        recorder.mark_ended("r1", Some(100));
        // 平台抖动：开播时间没变又报在播 → 立刻解除静默（A8）。
        assert!(!recorder.auto_blocked("r1", Some(100)));
        // 解除是一次性的：标记已消费，不会反复放行。
        assert!(!recorder.auto_blocked("r1", Some(100)));
    }

    #[test]
    fn confirmed_end_keeps_blocking_while_platform_says_offline() {
        let recorder = test_recorder("confirmed-offline");
        recorder.note_offline_stop("r1");
        recorder.mark_ended("r1", Some(100));
        // 平台仍说离线（live_started 为空）：静默照旧，等真正开播再放行。
        assert!(recorder.auto_blocked("r1", None));
        let plain = test_recorder("mark-consumed");
        plain.note_offline_stop("r2");
        plain.mark_ended("r2", Some(100));
        plain.mark_ended("r3", Some(100));
        assert!(plain.auto_blocked("r3", Some(100)), "标记不能被别场消费");
    }

    #[test]
    fn monitor_start_rechecks_removed_or_paused_rooms() {
        assert!(monitor_start_allowed(false, false));
        assert!(!monitor_start_allowed(true, false));
        assert!(!monitor_start_allowed(false, true));
        assert!(!monitor_start_allowed(true, true));
    }

    #[test]
    fn repeated_failures_enter_long_cooldown() {
        let recorder = test_recorder("hard-failures");
        assert!(!recorder.in_failure_cooldown("r1"));
        // 头两次只是重试，第三次才进入长冷却（R6/R7）。
        assert_eq!(recorder.note_hard_failure("r1"), 1);
        assert!(!recorder.in_failure_cooldown("r1"));
        assert_eq!(recorder.note_hard_failure("r1"), 2);
        assert!(!recorder.in_failure_cooldown("r1"));
        assert_eq!(recorder.note_hard_failure("r1"), 3);
        assert!(recorder.in_failure_cooldown("r1"));
        // 成功录完一场后从零开始。
        recorder.clear_hard_failure("r1");
        assert!(!recorder.in_failure_cooldown("r1"));
        assert_eq!(recorder.note_hard_failure("r1"), 1);
    }

    #[test]
    fn repeated_start_failures_enter_cooldown() {
        let recorder = test_recorder("start-failures");
        assert!(!recorder.in_start_cooldown("r1"));
        // 头两次仍重试，第三次才进冷却（G5）。
        recorder.note_start_failure("r1");
        recorder.note_start_failure("r1");
        assert!(!recorder.in_start_cooldown("r1"));
        recorder.note_start_failure("r1");
        assert!(recorder.in_start_cooldown("r1"));
        recorder.clear_start_failure("r1");
        assert!(!recorder.in_start_cooldown("r1"));
    }

    #[test]
    fn start_failure_classification_skips_user_intent() {
        // 房间没了：重试也不会变好，不该算进「连续开录失败」。
        assert!(!start_failure_is_counted(&AppError::User(
            "房间已移除或监控已暂停".into()
        )));
        // 鉴权 / 取流失败：冷却后再试才合理。
        assert!(start_failure_is_counted(&AppError::Platform {
            code: "auth",
            message: "登录已过期".into()
        }));
        assert!(start_failure_is_counted(&AppError::Ffmpeg(
            "no stream".into()
        )));
        // 库错误不归因于源。
        assert!(!start_failure_is_counted(&AppError::Db("disk io".into())));
    }

    #[test]
    fn stop_requests_are_idempotent() {
        // 第一次停录正常生效。
        assert!(should_restop(None, "disk_full"));
        // 同一理由重复请求（监控每秒一拍都会走到这里）直接忽略（B6）。
        assert!(!should_restop(Some("disk_full"), "disk_full"));
        assert!(!should_restop(Some("stopped"), "stopped"));
        // 用户手动停录可以覆盖磁盘红线，反过来不行。
        assert!(should_restop(Some("disk_full"), "stopped"));
        assert!(!should_restop(Some("stopped"), "disk_full"));
        assert!(!should_restop(Some("stream_ended"), "stalled"));
    }

    #[test]
    fn unconfirmed_clean_end_is_not_a_real_end() {
        // 平台在播 / 查不到：干净收尾也不能算确认下播（B1）。
        assert!(unconfirmed_end("finished", "stream_ended", false));
        // 有凭据、用户停录、异常收尾都不属于这一类。
        assert!(!unconfirmed_end("finished", "stream_ended", true));
        assert!(!unconfirmed_end("finished", "stopped", false));
        assert!(!unconfirmed_end("failed", "error", false));
        assert!(!unconfirmed_end("aborted", "app_exit", false));
    }

    #[test]
    fn shutdown_flag_rejects_new_starts_forever() {
        let recorder = test_recorder("shutdown");
        assert!(!recorder.is_shutting_down());
        recorder.begin_shutdown();
        assert!(recorder.is_shutting_down());
        // 再调用一次也不会被复位：退出流程可能重入（RunEvent::Exit）。
        recorder.begin_shutdown();
        assert!(recorder.is_shutting_down());
    }

    #[test]
    fn detect_errors_dedupe_by_message() {
        // 同一句检测失败只该告警/推事件一次；换了原因才再推一次（F1）。
        let recorder = test_recorder("detect");
        assert!(recorder.note_detect_error("r1", "房间不存在或已注销"));
        assert!(!recorder.note_detect_error("r1", "房间不存在或已注销"));
        assert!(recorder.note_detect_error("r1", "网络请求失败"));
        // 恢复正常只清一次。
        assert!(recorder.clear_detect_error("r1"));
        assert!(!recorder.clear_detect_error("r1"));
        // 清掉之后同样的错误可以重新告警。
        assert!(recorder.note_detect_error("r1", "网络请求失败"));
    }

    #[test]
    fn finished_start_consumes_both_stop_markers() {
        let recorder = test_recorder("start-guard");
        let guard = recorder.begin_start("r1");
        recorder.cancel_start("r1");
        recorder
            .pending_stop
            .lock()
            .unwrap()
            .insert("r1".into(), "stopped".into());
        assert_eq!(guard.finish(), (true, Some("stopped".into())));
        assert_eq!(recorder.begin_start("r1").finish(), (false, None));
    }

    #[test]
    fn failed_start_drops_its_pending_stop() {
        let recorder = test_recorder("start-guard-fail");
        {
            let _guard = recorder.begin_start("r2");
            recorder
                .pending_stop
                .lock()
                .expect("锁")
                .insert("r2".to_string(), "stopped".to_string());
        }
        // 这一路根本没开起来：停录请求不能留给以后的开录，否则会莫名秒停（C5）。
        assert!(recorder.pending_stop.lock().expect("锁").is_empty());
        assert!(!recorder.starting.lock().expect("锁").contains("r2"));
    }

    #[test]
    fn finalizing_marker_blocks_auto_start() {
        let recorder = test_recorder("finalizing");
        recorder
            .finalizing
            .lock()
            .expect("锁")
            .insert("r3".to_string());
        assert!(recorder.is_finalizing("r3"));
        assert!(!recorder.is_finalizing("r4"));
    }

    #[test]
    fn offline_confirm_counts_consecutive_checks() {
        let recorder = test_recorder("offline-b5");
        // 第一次离线只是候选，不能停录。
        assert!(!recorder.confirm_offline("r1"));
        // 中间查到在播：从零重新计。
        recorder.clear_offline("r1");
        assert!(!recorder.confirm_offline("r1"));
        assert!(recorder.confirm_offline("r1"));
        // 确认后清零，下一次重新从 1 开始。
        assert!(!recorder.confirm_offline("r1"));
    }

    #[test]
    fn split_manifest_round_trips_and_can_be_completed() {
        let dir = std::env::temp_dir().join(format!("hivecast-manifest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let file = dir.join("2026-01-01.split.json");
        let manifest = SplitManifest {
            version: 2,
            session_id: "s1".into(),
            room_id: "r1".into(),
            source: dir.join("2026-01-01.mp4").display().to_string(),
            status: "finished".into(),
            reason: "stream_ended".into(),
            error: None,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
            post: Default::default(),
            clips: vec![
                ManifestClip {
                    id: "s1".into(),
                    output: dir.join("2026-01-01.mp4").display().to_string(),
                    path: dir.join("2026-01-01.mp4").display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 0,
                    end: Some(2),
                    reason: None,
                    started_at: 100,
                    ended_at: 300,
                },
                ManifestClip {
                    id: "s2".into(),
                    output: dir.join("2026-01-01.clip01.mp4").display().to_string(),
                    path: dir.join("2026-01-01.clip01.ts").display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 2,
                    end: None,
                    reason: Some("duration".into()),
                    started_at: 300,
                    ended_at: 500,
                },
            ],
            rendered: 1,
            completed: false,
        };
        write_manifest(&file, &manifest).expect("写清单");
        let read = read_manifest(&file).expect("读清单");
        assert_eq!(read.clips.len(), 2);
        assert_eq!(read.rendered, 1);
        assert_eq!(read.clips[1].reason.as_deref(), Some("duration"));
        assert_eq!(read.clips[1].path, manifest.clips[1].path);
        assert!(!file.with_extension("split.tmp").exists());

        let mut done = read;
        done.completed = true;
        write_manifest(&file, &done).expect("完成清单");
        assert!(read_manifest(&file).expect("再读清单").completed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recovery_finishes_split_manifest_without_overwriting_the_first_clip() {
        let dir = std::env::temp_dir().join(format!("hivecast-recover-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let source = dir.join("live.mp4");
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        db.insert_room(
            &RoomRecord {
                id: "r1".into(),
                platform: "bilibili".into(),
                room_key: "1".into(),
                canonical_url: "https://live.bilibili.com/1".into(),
                title: "标题".into(),
                host_name: "主播".into(),
                avatar_url: None,
                cover_url: None,
                area_name: None,
                is_live: true,
                started_at: Some(1),
                online: Some(1),
                auto_record: true,
                monitor_paused: false,
                record_qn: 0,
                view_qn: 0,
                record_line: None,
                last_checked_at: Some(0),
                last_session_id: None,
                last_session_started_at: None,
                last_session_duration_ms: 0,
                last_session_status: None,
                last_session_reason: None,
                last_session_file_size: 0,
                last_session_quality: None,
                platform_uid: None,
                batch_follow: false,
                internal_room_id: None,
                removed_at: None,
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
            },
            &crate::platform::RoomInfo {
                host_name: "主播".into(),
                room_name: "标题".into(),
                avatar_url: None,
                cover_url: None,
                area_name: None,
                is_live: true,
                started_at: Some(1),
                online: Some(1),
                stable_id: None,
                extra: serde_json::Value::Null,
            },
        )
        .expect("建房间");
        let first = RecordSession {
            id: "s1".into(),
            room_id: "r1".into(),
            started_at: 100,
            ended_at: None,
            status: "recording".into(),
            reason: None,
            file_path: Some(source.display().to_string()),
            file_size: 0,
            duration_ms: 0,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
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
        };
        db.create_session(&first).expect("建会话");

        // 四个假分片：前两个属于首片段，后两个属于第二段。
        for (index, size) in [(0, 100usize), (1, 110), (2, 120), (3, 130)] {
            let part = dir.join(format!("live.part{index:04}.ts"));
            std::fs::write(part, vec![7u8; size]).expect("写分片");
        }
        let file = manifest_path(&source);
        let manifest = SplitManifest {
            version: 2,
            session_id: "s1".into(),
            room_id: "r1".into(),
            source: source.display().to_string(),
            status: "finished".into(),
            reason: "stream_ended".into(),
            error: None,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
            post: Default::default(),
            clips: vec![
                ManifestClip {
                    id: "s1".into(),
                    output: source.display().to_string(),
                    path: source.display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 0,
                    end: Some(2),
                    reason: None,
                    started_at: 100,
                    ended_at: 200,
                },
                ManifestClip {
                    id: "s2".into(),
                    output: dir.join("live.clip01.mp4").display().to_string(),
                    path: dir.join("live.clip01.mp4").display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 2,
                    end: None,
                    reason: Some("duration".into()),
                    started_at: 200,
                    ended_at: 300,
                },
            ],
            rendered: 0,
            completed: false,
        };
        write_manifest(&file, &manifest).expect("写清单");
        db.set_split_manifest("s1", Some(&file.display().to_string()))
            .expect("登记清单");

        let (count, bytes) = recover_split_manifest(&db, &file, "s1")
            .await
            .expect("恢复");
        assert_eq!(count, 2);
        assert_eq!(bytes, 460);
        assert!(!file.exists());
        assert!(db.manifest_sessions().expect("清单列表").is_empty());
        // 首片段保住了自己的 210 字节，没有被后面两个分片覆盖。
        let first = db.get_session("s1").expect("查库").expect("首片段");
        assert_eq!(first.status, "finished");
        assert!(first
            .file_path
            .as_deref()
            .unwrap_or("")
            .ends_with("live.ts"));
        assert_eq!(first.file_size, 210);
        let second = db.get_session("s2").expect("查库").expect("第二段");
        assert_eq!(second.file_size, 250);
        assert_eq!(db.media_group_items("s1").expect("整组").len(), 2);
        assert!(!dir.join("live.part0000.ts").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    pub(super) fn seed_recovery_session(db: &Db, source: &Path) {
        db.insert_room(
            &RoomRecord {
                id: "r1".into(),
                platform: "bilibili".into(),
                room_key: "1".into(),
                canonical_url: "https://live.bilibili.com/1".into(),
                title: "标题".into(),
                host_name: "主播".into(),
                avatar_url: None,
                cover_url: None,
                area_name: None,
                is_live: true,
                started_at: Some(1),
                online: Some(1),
                auto_record: true,
                monitor_paused: false,
                record_qn: 0,
                view_qn: 0,
                record_line: None,
                last_checked_at: Some(0),
                last_session_id: None,
                last_session_started_at: None,
                last_session_duration_ms: 0,
                last_session_status: None,
                last_session_reason: None,
                last_session_file_size: 0,
                last_session_quality: None,
                platform_uid: None,
                batch_follow: false,
                internal_room_id: None,
                removed_at: None,
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
            },
            &crate::platform::RoomInfo {
                host_name: "主播".into(),
                room_name: "标题".into(),
                avatar_url: None,
                cover_url: None,
                area_name: None,
                is_live: true,
                started_at: Some(1),
                online: Some(1),
                stable_id: None,
                extra: serde_json::Value::Null,
            },
        )
        .expect("建房间");
        db.create_session(&RecordSession {
            id: "s1".into(),
            room_id: "r1".into(),
            started_at: 100,
            ended_at: None,
            status: "recording".into(),
            reason: None,
            file_path: Some(source.display().to_string()),
            file_size: 0,
            duration_ms: 0,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
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
        })
        .expect("建会话");
    }

    #[tokio::test]
    async fn recovery_reuses_a_finished_clip_when_its_parts_are_already_gone() {
        let dir = std::env::temp_dir().join(format!("hivecast-recover2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let source = dir.join("live.mp4");
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        seed_recovery_session(&db, &source);
        // 首段已经合并成功并删掉分片，但 manifest 还停在 rendered=0。
        std::fs::write(&source, vec![8u8; 210]).expect("首段成品");
        for (index, size) in [(2, 120usize), (3, 130)] {
            std::fs::write(dir.join(format!("live.part{index:04}.ts")), vec![7u8; size])
                .expect("写分片");
        }
        let file = manifest_path(&source);
        let manifest = SplitManifest {
            version: 2,
            session_id: "s1".into(),
            room_id: "r1".into(),
            source: source.display().to_string(),
            status: "finished".into(),
            reason: "stream_ended".into(),
            error: None,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
            post: Default::default(),
            clips: vec![
                ManifestClip {
                    id: "s1".into(),
                    output: source.display().to_string(),
                    path: source.display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 0,
                    end: Some(2),
                    reason: None,
                    started_at: 100,
                    ended_at: 200,
                },
                ManifestClip {
                    id: "s2".into(),
                    output: dir.join("live.clip01.mp4").display().to_string(),
                    path: dir.join("live.clip01.mp4").display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 2,
                    end: None,
                    reason: Some("duration".into()),
                    started_at: 200,
                    ended_at: 300,
                },
            ],
            rendered: 0,
            completed: false,
        };
        write_manifest(&file, &manifest).expect("写清单");
        db.set_split_manifest("s1", Some(&file.display().to_string()))
            .expect("登记清单");

        let (count, bytes) = recover_split_manifest(&db, &file, "s1")
            .await
            .expect("恢复");
        assert_eq!(count, 2);
        assert_eq!(bytes, 460);
        let first = db.get_session("s1").expect("查库").expect("首片段");
        assert_eq!(first.file_size, 210);
        assert!(first
            .file_path
            .as_deref()
            .unwrap_or("")
            .ends_with("live.mp4"));
        let second = db.get_session("s2").expect("查库").expect("第二段");
        assert_eq!(second.file_size, 250);
        assert!(!dir.join("live.part0002.ts").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recovery_prefers_a_complete_clip_even_when_parts_remain() {
        let dir = std::env::temp_dir().join(format!("hivecast-recover4-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let source = dir.join("live.mp4");
        let second = dir.join("live.clip01.mp4");
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        seed_recovery_session(&db, &source);
        // 模拟「改名成功、清单还没更新、分片也还没删」的崩溃现场。
        std::fs::write(&source, vec![8u8; 210]).expect("首段成品");
        std::fs::write(&second, vec![9u8; 250]).expect("第二段成品");
        for (index, size) in [(0, 50usize), (1, 60), (2, 70), (3, 80)] {
            std::fs::write(dir.join(format!("live.part{index:04}.ts")), vec![7u8; size])
                .expect("写分片");
        }
        let file = manifest_path(&source);
        let manifest = SplitManifest {
            version: 2,
            session_id: "s1".into(),
            room_id: "r1".into(),
            source: source.display().to_string(),
            status: "finished".into(),
            reason: "stream_ended".into(),
            error: None,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
            post: Default::default(),
            clips: vec![
                ManifestClip {
                    id: "s1".into(),
                    output: source.display().to_string(),
                    path: source.display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 0,
                    end: Some(2),
                    reason: None,
                    started_at: 100,
                    ended_at: 200,
                },
                ManifestClip {
                    id: "s2".into(),
                    output: second.display().to_string(),
                    path: second.display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 2,
                    end: None,
                    reason: Some("duration".into()),
                    started_at: 200,
                    ended_at: 300,
                },
            ],
            rendered: 0,
            completed: false,
        };
        write_manifest(&file, &manifest).expect("写清单");
        db.set_split_manifest("s1", Some(&file.display().to_string()))
            .expect("登记清单");

        let (count, bytes) = recover_split_manifest(&db, &file, "s1")
            .await
            .expect("恢复");
        assert_eq!(count, 2);
        assert_eq!(bytes, 460);
        assert_eq!(
            db.get_session("s1").expect("查库").expect("首段").file_size,
            210
        );
        assert_eq!(
            db.get_session("s2").expect("查库").expect("二段").file_size,
            250
        );
        for index in 0..4 {
            assert!(
                !dir.join(format!("live.part{index:04}.ts")).exists(),
                "成品已完整，分片应清理"
            );
        }
        assert!(!file.exists(), "所有片段都恢复后清单应删除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn orphan_recovery_reuses_a_complete_file_without_remerging() {
        let dir = std::env::temp_dir().join(format!("hivecast-recover5-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let source = dir.join("live.mp4");
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        seed_recovery_session(&db, &source);
        std::fs::write(&source, vec![8u8; 210]).expect("成品");
        for (index, size) in [(0, 50usize), (1, 60)] {
            std::fs::write(dir.join(format!("live.part{index:04}.ts")), vec![7u8; size])
                .expect("写分片");
        }

        assert_eq!(recover_parts(&db, &HashSet::new()).await, (0, 0));
        db.close_orphan_sessions(&HashSet::new()).unwrap();
        let (count, bytes) = recover_parts(&db, &HashSet::new()).await;
        assert_eq!(count, 1);
        assert_eq!(bytes, 210);
        let row = db.get_session("s1").expect("查库").expect("会话");
        assert_eq!(row.file_size, 210);
        assert_eq!(row.status, "aborted");
        assert!(!dir.join("live.part0000.ts").exists());
        assert!(!dir.join("live.part0001.ts").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recovery_continues_past_a_missing_early_clip() {
        let dir = std::env::temp_dir().join(format!("hivecast-recover3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let source = dir.join("live.mp4");
        let db = Db::open(Path::new(":memory:")).expect("内存库");
        seed_recovery_session(&db, &source);
        // 第一段的分片和成品都没了（例如用户手工清过），后两段仍完好。
        for (index, size) in [(2, 120usize), (3, 130), (4, 140), (5, 150)] {
            std::fs::write(dir.join(format!("live.part{index:04}.ts")), vec![7u8; size])
                .expect("写分片");
        }
        let file = manifest_path(&source);
        let manifest = SplitManifest {
            version: 2,
            session_id: "s1".into(),
            room_id: "r1".into(),
            source: source.display().to_string(),
            status: "finished".into(),
            reason: "stream_ended".into(),
            error: None,
            quality: Some("原画".into()),
            quality_history: Vec::new(),
            line: Some("线路1".into()),
            post: Default::default(),
            clips: vec![
                ManifestClip {
                    id: "s1".into(),
                    output: source.display().to_string(),
                    path: source.display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 0,
                    end: Some(2),
                    reason: None,
                    started_at: 100,
                    ended_at: 200,
                },
                ManifestClip {
                    id: "s2".into(),
                    output: dir.join("live.clip01.mp4").display().to_string(),
                    path: dir.join("live.clip01.mp4").display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 2,
                    end: Some(4),
                    reason: Some("duration".into()),
                    started_at: 200,
                    ended_at: 300,
                },
                ManifestClip {
                    id: "s3".into(),
                    output: dir.join("live.clip02.mp4").display().to_string(),
                    path: dir.join("live.clip02.mp4").display().to_string(),
                    final_path: None,
                    post_state: None,
                    post_error: None,
                    start: 4,
                    end: None,
                    reason: Some("duration".into()),
                    started_at: 300,
                    ended_at: 400,
                },
            ],
            rendered: 0,
            completed: false,
        };
        write_manifest(&file, &manifest).expect("写清单");
        db.set_split_manifest("s1", Some(&file.display().to_string()))
            .expect("登记清单");

        let (count, bytes) = recover_split_manifest(&db, &file, "s1")
            .await
            .expect("恢复");
        assert_eq!(count, 2, "缺失的首段不能挡住后面两段");
        assert_eq!(bytes, 540);
        assert!(file.exists(), "还有片段没恢复时清单必须保留");
        assert!(!read_manifest(&file).expect("读清单").completed);
        assert!(db.get_session("s2").expect("查库").is_some());
        assert!(db.get_session("s3").expect("查库").is_some());
        assert!(!dir.join("live.part0002.ts").exists());
        assert!(!dir.join("live.part0004.ts").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "需要本机 ffmpeg"]
    async fn split_ranges_cut_real_ffmpeg_parts() {
        use std::time::{Duration, UNIX_EPOCH};
        let dir =
            std::env::temp_dir().join(format!("hivecast-split-real-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let source = dir.join("live.mp4");
        let status = tokio::process::Command::new(crate::ffmpeg::binary())
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "6",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "10",
                "-c:a",
                "aac",
                "-f",
                "segment",
                "-segment_time",
                "1",
                "-reset_timestamps",
                "1",
                "-segment_format",
                "mpegts",
            ])
            .arg(segment::parts_pattern(&source))
            .status()
            .await
            .expect("生成分片");
        assert!(status.success());
        let parts = segment::collect_parts(&source);
        assert!(parts.len() >= 4, "至少要切出四片，实际 {}", parts.len());
        // 模拟录制时逐片写出的完成时间：每片 2 秒。
        let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (index, part) in parts.iter().enumerate() {
            std::fs::File::options()
                .write(true)
                .open(part)
                .expect("打开分片")
                .set_modified(base + Duration::from_secs(index as u64 * 2))
                .expect("改时间");
        }

        let first = dir.join("live.clip00.mp4");
        let second = dir.join("live.clip01.mp4");
        let (path, size) = finish_range(&source, &first, 0, Some(2)).await;
        assert_eq!(path, first);
        assert!(size > 1000, "第一段太小：{size}");
        let (path, size) = finish_range(&source, &second, 2, None).await;
        assert_eq!(path, second);
        assert!(size > 1000, "第二段太小：{size}");
        assert!(first.is_file() && second.is_file());
        assert_eq!(
            segment::collect_parts(&source).len(),
            parts.len(),
            "登记前保留输入分片"
        );
        assert!(!dir.join(".live.clip00.merging.mp4").exists());
        assert!(!dir.join("live.clip00.concat.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_retry_lock_serializes_two_threads() {
        use std::sync::Barrier;
        use std::thread;

        let manifest = std::env::temp_dir()
            .join(format!(
                "hivecast-test-finalize-race-{}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let start = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let manifest = manifest.clone();
            let start = start.clone();
            handles.push(thread::spawn(move || {
                start.wait();
                match acquire_finalize_retry(&manifest) {
                    Ok(lease) => {
                        // 占住锁一会儿，保证另一个线程一定能撞上。
                        std::thread::sleep(Duration::from_millis(100));
                        drop(lease);
                        true
                    }
                    Err(_) => false,
                }
            }));
        }
        start.wait();
        let acquired = handles
            .into_iter()
            .map(|handle| handle.join().expect("线程不应 panic"))
            .filter(|acquired| *acquired)
            .count();
        assert_eq!(acquired, 1, "同一份清单只能有一个线程拿到锁");
        // 锁文件落在临时目录，测试结束顺手清掉，避免污染工作区。
        let _ = std::fs::remove_file(format!("{manifest}.lock"));
    }

    #[test]
    fn finalize_retry_file_lock_is_exclusive_across_handles() {
        let manifest = std::env::temp_dir()
            .join(format!(
                "hivecast-test-finalize-file-lock-{}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let first = lock_manifest_file(&manifest).expect("第一把文件锁应成功");
        assert!(
            lock_manifest_file(&manifest).is_err(),
            "另一个文件句柄不能同时锁住同一份清单"
        );
        drop(first);
        assert!(
            lock_manifest_file(&manifest).is_ok(),
            "文件锁释放后应能再次占用"
        );
        let _ = std::fs::remove_file(format!("{manifest}.lock"));
    }

    #[test]
    fn finalize_retry_lock_is_per_manifest_and_released_on_drop() {
        let manifest = std::env::temp_dir()
            .join(format!(
                "hivecast-test-finalize-lock-{}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let first = acquire_finalize_retry(&manifest).expect("第一份锁应成功");
        assert!(
            acquire_finalize_retry(&manifest).is_err(),
            "同一份清单不能同时重试"
        );
        // 不同清单互不影响，避免一个房间的收尾挡住另一个房间。
        let other = format!("{manifest}-other");
        let other_lease = acquire_finalize_retry(&other).expect("另一份清单应成功");
        drop(other_lease);
        drop(first);
        assert!(
            acquire_finalize_retry(&manifest).is_ok(),
            "lease 释放后应能再次占用"
        );
        let _ = std::fs::remove_file(format!("{manifest}.lock"));
        let _ = std::fs::remove_file(format!("{other}.lock"));
    }

    #[test]
    fn offline_needs_two_successful_checks() {
        let recorder = test_recorder("offline");
        assert!(!recorder.confirm_offline("r1"));
        assert!(recorder.confirm_offline("r1"));
        // 确认后计数清零，下一次重新从 1 开始。
        assert!(!recorder.confirm_offline("r1"));
        recorder.clear_offline("r1");
        assert!(!recorder.confirm_offline("r1"));
    }
}
