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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

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
    /// 本场直播的开播时间（平台口径）：用来区分「同一场」与「新一场」
    live_started_at: Option<i64>,
    /// 已落盘字节数与最后一次增长的时刻，用来发现「连着但不给数据」
    last_bytes: u64,
    last_growth: Instant,
    /// 假死重启次数；只要文件重新增长就清零
    stalls: u32,
    /// 「平台仍显示在播」的干净 EOF 复核次数（R1）
    eof_retries: u32,
    /// 本场开录时的画质档位与线路：中途改设置不该渗进正在录的这一场（A3）
    qn: u32,
    line: Option<String>,
    /// ffmpeg 最近几十行 stderr：默认只进 debug，异常收尾时把尾部打进 WARN。
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
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

pub struct Recorder {
    db: Arc<Db>,
    /// 媒体库根目录：设置页可以改，开录时现取，避免改完要重启。
    media_root: std::sync::RwLock<PathBuf>,
    /// 文件名模板与清洗开关，同样开录时现取。
    naming: std::sync::RwLock<Naming>,
    /// 同时录制上限；0 表示不限。
    max_concurrent: std::sync::atomic::AtomicUsize,
    inner: Mutex<HashMap<String, Active>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// 最近一次失败原因，展示在监控页行内
    errors: Mutex<HashMap<String, String>>,
    /// 最近一次「检测失败」的文案：同一句失败只告警/推事件一次，
    /// 平台一直挂着坏房间时不该每拍都刷前端（F1）。
    /// 与 errors 分开记：开录失败（例如并发已满）也写 errors，
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
    ended: Mutex<HashMap<String, (Instant, Option<i64>, bool)>>,
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
    /// 正在开录的房间：停录请求要能看见「ffmpeg 已经在起、但还没进 active」的这段窗口（C5）。
    starting: Mutex<HashSet<String>>,
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
            max_concurrent: std::sync::atomic::AtomicUsize::new(4),
            inner: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            detect_errors: Mutex::new(HashMap::new()),
            start_locks: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashSet::new()),
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
        thumbs.ensure(app.clone(), vec![(session.id, path, session.duration_ms)]);
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

    /// 正在开录、但还没挂进 active 的会话数。用来在并发上限前占位（C2）。
    fn starting_count(&self) -> usize {
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

    /// 登记「这一路正在开录」：stop 靠它判断停录请求是在 start 之前还是之中到达（C5）。
    fn begin_start<'a>(&'a self, room_id: &str) -> StartGuard<'a> {
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
        self.start_inner(app, broker, room, false).await
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
        let result = self.start_inner(app, broker, room, true).await;
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
            if fresh.is_none() {
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
        let mut slot = self.begin_start(&room.id);

        // 并发上限：4 路 1080p 已经能吃掉一块 SSD 的写入带宽，默认不再往上加。
        // 计数要把「开录在途」也算上：两路同时开播时只看 active 会双双通过（C2），
        // 各自拉一个 ffmpeg 再挤掉一个。自己已占了一个在途名额，所以是 > 而不是 >=。
        let limit = self.max_concurrent();
        let occupied = self.active_count() + self.starting_count();
        if limit > 0 && occupied > limit {
            tracing::warn!(limit, occupied, "拒绝开录：同时录制已达上限");
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
            .map_err(|err| {
                tracing::warn!(room_id = %room.id, room = %room.host_name, error = %err, "开录解析线路失败");
                AppError::from(err)
            })?;

        let (candidate, group) =
            pick_candidate(&groups, room.record_line.as_deref()).ok_or_else(|| {
                tracing::warn!(
                    room_id = %room.id,
                    room = %room.host_name,
                    groups = groups.len(),
                    line = ?room.record_line,
                    "开录没有可用线路"
                );
                AppError::User("没有可用的线路".into())
            })?;

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
                qn: room.record_qn,
            },
            started_at,
            "mp4",
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

        let mut command = build_command(&path, &url, &headers, 0);
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
        write_pid_file(&path, child.id());

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
            line: Some(group.name.clone()),
            error: None,
            danmaku_path: danmaku.as_ref().map(|_| danmaku_path.display().to_string()),
            danmaku_count: 0,
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
            let _ = child.start_kill();
            remove_pid_file(&path);
            if let (Some(hub), Some(writer)) = (self.danmaku.as_ref(), danmaku.as_ref()) {
                hub.detach_record(&room.id, writer);
            }
            // 这次开录没成，留下的空文件/空弹幕也算残留，一并清掉。
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&danmaku_path);
            return Err(AppError::Internal(err.to_string()));
        }

        let recording = RecordingInfo {
            room_id: room.id.clone(),
            session_id,
            file_path: path.display().to_string(),
            started_at,
            quality: candidate.name.clone(),
            line: group.name.clone(),
        };

        let entry = Active {
            recording: recording.clone(),
            child,
            stop_reason: None,
            started: Instant::now(),
            host_name: room.host_name.clone(),
            danmaku,
            danmaku_path: Some(danmaku_path.display().to_string()),
            restarts: 0,
            live_started_at: room.started_at,
            last_bytes: 0,
            last_growth: Instant::now(),
            stalls: 0,
            eof_retries: 0,
            qn: room.record_qn,
            line: room.record_line.clone(),
            stderr_tail,
        };
        {
            let mut inner = self.inner.lock().expect("录制状态被污染");
            // 再查一次并发上限：上面那次在网络解析之前，两路同时开录都会看到
            // 「还没满」，各自拉起一个 ffmpeg（C2）。这里和 insert 在同一把锁里，
            // 后到的这一路会看到前一路已经占住名额。
            if limit > 0 && inner.len() >= limit {
                drop(inner);
                let mut entry = entry;
                let _ = entry.child.start_kill();
                remove_pid_file(&path);
                if let (Some(hub), Some(writer)) = (self.danmaku.as_ref(), entry.danmaku.as_ref()) {
                    hub.detach_record(&room.id, writer);
                }
                // ffmpeg 可能已经建了空分片，弹幕文件也建好了：都清掉，别留垃圾。
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(&danmaku_path);
                let _ = std::fs::remove_file(path.with_extension("part0000.ts"));
                let _ = self.db.delete_session(&entry.recording.session_id);
                tracing::warn!(
                    room = %room.host_name,
                    limit,
                    "并发已满，放弃本次开录（已清理会话与进程）"
                );
                return Err(AppError::User(format!(
                    "同时录制已达上限（{limit} 路），先停掉一路或到设置里调高上限"
                )));
            }
            inner.insert(room.id.clone(), entry);
        }
        slot.mark_inserted();
        drop(slot);

        // 开录在途时用户点了停录：会话既然已经挂上，就立刻按这个原因收尾（C5）。
        let pending = self
            .pending_stop
            .lock()
            .expect("停录标记被污染")
            .remove(&room.id);
        if let Some(reason) = pending {
            tracing::info!(
                room = %room.host_name,
                reason = %reason,
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
            qn = room.record_qn,
            line = ?room.record_line,
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
            for (_, active) in inner.iter_mut() {
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
                // 房间在录制过程中被删掉：删除流程自己会先停录，但那一步可能
                // 因为并发/异常丢掉。这里兜底按「用户停录」收尾——不续录、
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
        // 房间已经被删除就别再续录：用户已经不关心这场直播了（F2）。
        if self.room_removed(room_id) {
            tracing::info!(room_id, "房间已删除，不再续录");
            return false;
        }
        // 取快照后立刻放锁：下面要 await 解析线路，持有 std::sync::Mutex 会卡死整个录制表。
        // 画质与线路同样取快照：这中间用户可能在设置页改过，
        // 正在录的这一场不该跟着换档（A3）。
        let (session_id, path, start_number, restarts, qn, line) = {
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
                active.qn,
                active.line.clone(),
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
        tokio::time::sleep(Duration::from_secs(3)).await;
        let groups = match broker.groups_for_respawn(&reference, qn).await {
            Ok(groups) => groups,
            Err(err) => {
                tracing::warn!(%err, room_id, "续录解析线路失败");
                return false;
            }
        };
        let Some((candidate, _group)) = pick_candidate(&groups, line.as_deref()) else {
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
        let stderr_tail = drain_stderr(&mut child);
        write_pid_file(&path, child.id());

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
        let _finalizing = FinalizeGuard {
            recorder: self,
            room_id: room_id.to_string(),
        };

        let elapsed = active.started.elapsed();
        // 收尾产物：分片 → mp4；合成失败退化为字节拼接的 .ts；再不行只能报分片总量。
        let source_path = PathBuf::from(&active.recording.file_path);
        // 进程已经不在了，pid 文件没有保留价值；漏删会让下次启动误杀无关进程。
        remove_pid_file(&source_path);
        let auto_remux = crate::settings::load(&self.db).auto_remux;
        // 收尾里的转封装同样会写文件：登记成任务，退出时才会等它写完再走。
        let jobs = app.state::<crate::state::AppState>().jobs.clone();
        let remux_label = source_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| active.recording.session_id.clone());
        let _remux_job = auto_remux
            .then(|| jobs.begin("remux", remux_label, Some(active.recording.quality.clone())));
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
            Some("disk_lost") => ("aborted", "disk_lost", None),
            Some("stalled") => ("failed", "stalled", Some("直播流长时间没有数据".into())),
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

        if final_status == "failed" {
            let tail = tail_text(&active.stderr_tail);
            if !tail.is_empty() {
                tracing::warn!(room_id, tail = %tail, "ffmpeg 错误输出");
            }
        }

        // 自然结束（下播 / 断流收尾）后先静默一会儿：平台状态常滞后一两轮，
        // 不挡住的话监控会立刻再开一条秒退的会话（S2）。
        if final_status == "finished" && reason == "stream_ended" && confirmed_end {
            self.mark_ended(room_id, active.live_started_at);
        }

        // 平台在播、却只能以「自然结束」收尾（B1）：没有确认凭据就不能算成功。
        // 既不能弹结束通知、也不能写 10 分钟静默，但也不能让监控每隔一轮就重开
        // 一条秒退会话——把它计入连续失败，攒够阈值进长冷却。
        let unconfirmed = unconfirmed_end(final_status, reason, confirmed_end);

        // 连续非正常收尾：攒到阈值就进长冷却，避免源不可用时每分钟一条会话（R6/R7）。
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
            // 未确认收尾刚刚记了一次失败，别在这里又清掉，否则永远攒不到阈值。
            if failed_streak == 0 {
                self.clear_failure(room_id);
                self.clear_hard_failure(room_id);
            }
            self.queue_thumb(app, &active.recording.session_id);
        }

        // 每次收尾跑一次保留策略：录制是媒体库唯一的增长来源。
        crate::retention::enforce_from_settings(&self.db);

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
                emit_notice(app, "ended", room_id, &active.host_name, None)
            }
            ("finished", "stream_ended") => tracing::warn!(
                room_id,
                failed_streak,
                confirmed_end,
                "未能确认平台已下播，本次收尾不弹结束通知"
            ),
            ("aborted", "disk_full") => {
                emit_notice(app, "disk_full", room_id, &active.host_name, None)
            }
            ("aborted", "disk_lost") => {
                emit_notice(app, "disk_lost", room_id, &active.host_name, None)
            }
            ("failed", _) => {
                // 只提醒第 1 次与进入长冷却那一次；中间的重试不打扰用户。
                if failed_streak == 1 || failed_streak == FAILED_STREAK_LIMIT {
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
/// 用户意图 / 并发上限 / 房间已移除这类失败，重试也不会变好，每拍重试只是刷日志；
/// 鉴权、取流地址、文件写入这些才值得冷却之后再试。
fn start_failure_is_counted(err: &AppError) -> bool {
    match err {
        AppError::Db(_) | AppError::Internal(_) => false,
        AppError::User(message) => ![
            "房间已移除",
            "房间不存在",
            "应用正在退出",
            "上一场正在收尾",
            "同时录制已达上限",
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
    fn mark_inserted(&mut self) {
        self.inserted = true;
    }
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        self.recorder
            .starting
            .lock()
            .expect("开录标记被污染")
            .remove(&self.room_id);
        // 开录失败：这条停录请求没有对应的会话可停，别留给下一次开录。
        if !self.inserted {
            self.recorder
                .pending_stop
                .lock()
                .expect("停录标记被污染")
                .remove(&self.room_id);
        }
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

fn write_pid_file(path: &Path, pid: Option<u32>) {
    let Some(pid) = pid else { return };
    if let Err(err) = std::fs::write(pid_file(path), pid.to_string()) {
        tracing::debug!(%err, file = %path.display(), "写入 ffmpeg pid 文件失败");
    }
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
pub fn reap_orphans(db: &Db) {
    let Ok(rows) = db.recording_sessions() else {
        return;
    };
    for (_id, file) in rows {
        let path = PathBuf::from(&file);
        let pid_path = pid_file(&path);
        let Ok(raw) = std::fs::read_to_string(&pid_path) else {
            continue;
        };
        let Ok(pid) = raw.trim().parse::<u32>() else {
            let _ = std::fs::remove_file(&pid_path);
            continue;
        };
        if process_matches(pid, &path) {
            tracing::warn!(pid, file = %path.display(), "清理上次崩溃遗留的 ffmpeg");
            terminate(pid);
        }
        let _ = std::fs::remove_file(&pid_path);
    }
}

/// 这个 pid 现在还活着、而且确实是录这条文件的 ffmpeg。
#[cfg(unix)]
fn process_matches(pid: u32, path: &Path) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
        return false;
    };
    let Ok(output) = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return false;
    };
    let command = String::from_utf8_lossy(&output.stdout);
    command.contains("ffmpeg") && command.contains(stem)
}

#[cfg(windows)]
fn process_matches(pid: u32, path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
        return false;
    };
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
    else {
        return false;
    };
    let command = String::from_utf8_lossy(&output.stdout).to_lowercase();
    // tasklist 只给映像名；命令行要另查，这里退一步只认 ffmpeg.exe 与同名录像目录。
    let looks_like_ffmpeg = command.contains("ffmpeg");
    let parent = path
        .parent()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    looks_like_ffmpeg
        && (!stem.is_empty())
        && std::process::Command::new("wmic")
            .args([
                "process",
                "where",
                &format!("ProcessId={pid}"),
                "get",
                "CommandLine",
            ])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains(&parent))
            .unwrap_or(false)
}

/// 先礼后兵：SIGTERM，等 300ms，还在就 SIGKILL。
#[cfg(unix)]
fn terminate(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(100));
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return;
        }
    }
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn terminate(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
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
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(ffmpeg::binary());
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
/// 在 close_orphan_sessions 之后调用（I2）：崩溃会话已被标成 aborted，
/// 这里先从「失败/中止」的最近记录里捞回还带分片的，再合并。
/// 由 state.rs 放在后台任务里跑，不挡窗口出现。
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

    // 失败 / 中止的收尾也可能没走完合并：进程崩在「分片写完、还没合并」之间。
    // 只扫 status='recording' 会把这些分片永远留在磁盘上（G4）。
    match db.unfinished_sessions() {
        Ok(rows) => {
            for (id, file_path) in rows {
                let path = PathBuf::from(&file_path);
                if segment::collect_parts(&path).is_empty() {
                    continue;
                }
                let (target, size) = finish_file(&path, true).await;
                if size <= 0 {
                    continue;
                }
                let _ = db.update_session_file(&id, &target.display().to_string(), size);
                tracing::info!(
                    id = %id,
                    file = %target.display(),
                    size,
                    "恢复失败会话遗留的分片"
                );
                recovered += 1;
                bytes += size as u64;
            }
        }
        Err(err) => tracing::warn!(%err, "读取失败会话失败"),
    }
    (recovered, bytes)
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
        let parts = crate::filename::NameParts {
            platform_id: "bilibili",
            platform: "哔哩哔哩",
            host: "主播",
            title: "标题",
            area: "",
            room_key: "6",
            qn: 10000,
        };
        let naming = Naming::default();
        let path = session_path(Path::new("/tmp/lib"), &parts, 1_760_000_000, "flv", &naming);
        let text = path.display().to_string();
        assert!(text.starts_with("/tmp/lib/哔哩哔哩/主播/哔哩哔哩-主播-标题-"));
        assert!(text.ends_with(".flv"));
        // 关掉分目录后直接落在根目录。
        let flat = Naming {
            group_dirs: false,
            ..naming.clone()
        };
        let path = session_path(Path::new("/tmp/lib"), &parts, 1_760_000_000, "flv", &flat);
        assert_eq!(path.parent(), Some(Path::new("/tmp/lib")));
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
        // 并发上限 / 房间没了：重试也不会变好，不该算进「连续开录失败」。
        assert!(!start_failure_is_counted(&AppError::User(
            "同时录制已达上限（1 路），先停掉一路或到设置里调高上限".into()
        )));
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
    fn start_guard_clears_inflight_and_keeps_pending_stop() {
        let recorder = test_recorder("start-guard");
        {
            let mut guard = recorder.begin_start("r1");
            assert!(recorder.starting.lock().expect("锁").contains("r1"));
            // 开录在途时来了停录请求：记进 pending_stop。
            recorder
                .pending_stop
                .lock()
                .expect("锁")
                .insert("r1".to_string(), "stopped".to_string());
            guard.mark_inserted();
            assert!(!recorder.is_finalizing("r1"));
        }
        // 会话已挂上：在途标记要摘掉，但停录请求要留给 start 消费（C5）。
        assert!(!recorder.starting.lock().expect("锁").contains("r1"));
        assert_eq!(
            recorder
                .pending_stop
                .lock()
                .expect("锁")
                .get("r1")
                .map(String::as_str),
            Some("stopped")
        );
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
