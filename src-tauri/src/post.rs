//! 录制后处理 v1：源封装母带 + 三种互斥模式（source / convert / custom）。
//!
//! 设计约束（docs/77）：
//! - 录制阶段只产出「源封装母带」，后处理产物是派生文件；
//! - 派生文件先写临时文件，ffprobe 校验通过后才改名；
//! - 只有 convert 模式允许把源移进系统回收站，且必须在数据库提交之后；
//! - custom 只执行 ffmpeg，不经过 shell，模板必须含 {input}/{output}，源永不自动删。

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use hivecast_ipc::{PostProgress, Settings, TrashInfo};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::ffmpeg;
use crate::platform::{Codec, StreamFormat};
use crate::settings;

pub const STATE_RECORDING: &str = "recording";
pub const STATE_DONE: &str = "done";
pub const STATE_FAILED: &str = "failed";
pub const STATE_CANCELLED: &str = "cancelled";

pub const EVENT_POST_PROGRESS: &str = "post://progress";

const DEFAULT_TIMEOUT_SEC: u32 = 600;
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(120);
/// 转换前预留：源体积的 15% + 256 MiB。容器转换基本等大，
/// 自定义编码可能更小，但预留宁多勿少，磁盘满时 ffmpeg 留下的半成品最难收拾。
const DISK_HEADROOM_BYTES: u64 = 256 * 1024 * 1024;
const DISK_HEADROOM_PERCENT: u64 = 15;

/// 正在排队/执行的后处理任务。取消标志与任务一起登记，
/// 进程退出或用户点「取消」时只需要翻标志，具体清理由执行线程完成。
struct TaskSlot {
    session_id: String,
    cancelled: Arc<AtomicBool>,
}

fn tasks() -> &'static Mutex<HashMap<String, TaskSlot>> {
    static TASKS: OnceLock<Mutex<HashMap<String, TaskSlot>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程退出时置位：之后才创建的恢复任务也必须立刻取消，不能继续起新 ffmpeg。
fn shutting_down() -> &'static AtomicBool {
    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
}

/// 一场收尾会按片段连续跑多个后处理任务；取消要覆盖整场，
/// 而不能只停掉当前正在跑的那一个。收尾开始时登记，结束时自动移除。
fn sessions() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct SessionGuard {
    session_id: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        sessions()
            .lock()
            .expect("后处理会话表被污染")
            .remove(&self.session_id);
    }
}

/// 标记一场录像进入收尾；期间收到的取消会传递给之后创建的每个任务。
pub fn begin_session(session_id: &str) -> SessionGuard {
    sessions()
        .lock()
        .expect("后处理会话表被污染")
        .insert(session_id.to_string(), Arc::new(AtomicBool::new(false)));
    SessionGuard {
        session_id: session_id.to_string(),
    }
}

fn auto_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(1))
}

fn manual_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(1))
}

fn global_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(2))
}

/// 取消某个会话名下所有后处理（自动收尾与手动转封装都可能各有任务）。
pub fn cancel_session(session_id: &str) -> usize {
    // 先单独处理会话级标志再锁任务表，避免与 run 的加锁顺序相反造成死锁。
    let scoped = {
        let guard = sessions().lock().expect("后处理会话表被污染");
        guard.get(session_id).map(|flag| {
            flag.store(true, Ordering::SeqCst);
            1
        })
    };
    let guard = tasks().lock().expect("后处理任务表被污染");
    let mut count = 0;
    for slot in guard.values() {
        if slot.session_id == session_id {
            slot.cancelled.store(true, Ordering::SeqCst);
            count += 1;
        }
    }
    count.max(scoped.unwrap_or(0))
}

/// 退出前取消全部后处理：执行方收到标志后会杀 ffmpeg 并删临时文件。
pub fn cancel_all() -> usize {
    shutting_down().store(true, Ordering::SeqCst);
    let scoped = {
        let guard = sessions().lock().expect("后处理会话表被污染");
        for flag in guard.values() {
            flag.store(true, Ordering::SeqCst);
        }
        guard.len()
    };
    let guard = tasks().lock().expect("后处理任务表被污染");
    let mut count = 0;
    for slot in guard.values() {
        slot.cancelled.store(true, Ordering::SeqCst);
        count += 1;
    }
    count.max(scoped)
}

/// 当前还在登记表里的后处理任务数（退出前等待清理用）。
pub fn active_count() -> usize {
    tasks().lock().expect("后处理任务表被污染").len()
}

/// 退出前取消全部后处理，最多等 6 秒让执行方杀掉 ffmpeg、删掉临时文件。
pub async fn shutdown() {
    let cancelled = cancel_all();
    if cancelled == 0 {
        return;
    }
    tracing::info!(cancelled, "退出前取消后处理");
    let deadline = Instant::now() + std::time::Duration::from_secs(6);
    while active_count() > 0 && Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[derive(Clone)]
struct Ctx {
    id: String,
    session_id: String,
    cancelled: Arc<AtomicBool>,
    app: Option<AppHandle>,
    jobs: Option<Arc<crate::jobs::Jobs>>,
}

impl Ctx {
    fn emit(&self, percent: i64, stage: &str, state: &str, message: Option<&str>) {
        if let Some(jobs) = self.jobs.as_ref() {
            jobs.progress(&self.id, percent, stage, state, message);
        }
        let Some(app) = self.app.as_ref() else { return };
        let _ = app.emit(
            EVENT_POST_PROGRESS,
            PostProgress {
                task: self.id.clone(),
                session_id: self.session_id.clone(),
                percent: percent.clamp(0, 100),
                stage: stage.to_string(),
                state: state.to_string(),
                message: message.map(str::to_string),
            },
        );
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

async fn acquire(sem: &'static Semaphore, flag: &AtomicBool) -> Option<SemaphorePermit<'static>> {
    loop {
        if flag.load(Ordering::SeqCst) {
            return None;
        }
        tokio::select! {
            permit = sem.acquire() => return permit.ok(),
            _ = tokio::time::sleep(Duration::from_millis(120)) => {}
        }
    }
}

/// 执行后处理。失败不抛错：返回源文件 + failed 状态，调用方照常写历史。
///
/// 自动收尾任务 id 以 "auto:" 开头，串行执行（同一时刻最多一个自动任务），
/// 手动任务走 "manual:" 通道；两者合计最多两个，给录制与手动操作都留出余量。
/// "session_id" 只用于取消与进度事件，必须是被处理录像的会话 id。
pub async fn run(
    id: &str,
    session_id: &str,
    src: &Path,
    cfg: &PostConfig,
    app: Option<&AppHandle>,
) -> PostOutcome {
    let app_state = app.and_then(|handle| handle.try_state::<crate::state::AppState>());
    let jobs = app_state.as_ref().map(|state| state.jobs.clone());
    if let Some(jobs) = jobs.as_ref() {
        let title = src
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "后处理".to_string());
        let room_id = app_state
            .as_ref()
            .and_then(|state| state.db.get_session(session_id).ok().flatten())
            .map(|session| session.room_id);
        jobs.start_external(
            id,
            task_kind(id),
            title,
            Some(session_id.to_string()),
            Some(session_id.to_string()),
            room_id,
        );
    }
    if !src.is_file() {
        emit_one(
            app,
            id,
            session_id,
            0,
            "failed",
            STATE_FAILED,
            Some("源文件不存在"),
        );
        if let Some(jobs) = jobs.as_ref() {
            jobs.finish_external(id, STATE_FAILED, Some("源文件不存在"));
        }
        return PostOutcome::source_only(src, STATE_FAILED, Some("源文件不存在".into()));
    }
    let initially_cancelled = shutting_down().load(Ordering::SeqCst)
        || sessions()
            .lock()
            .expect("后处理会话表被污染")
            .get(session_id)
            .map(|flag| flag.load(Ordering::SeqCst))
            .unwrap_or(false);
    let cancelled = Arc::new(AtomicBool::new(initially_cancelled));
    {
        let mut guard = tasks().lock().expect("后处理任务表被污染");
        guard.insert(
            id.to_string(),
            TaskSlot {
                session_id: session_id.to_string(),
                cancelled: cancelled.clone(),
            },
        );
    }
    let ctx = Ctx {
        id: id.to_string(),
        session_id: session_id.to_string(),
        cancelled: cancelled.clone(),
        app: app.cloned(),
        jobs: jobs.clone(),
    };
    ctx.emit(0, "queued", "queued", None);

    let auto = id.starts_with("auto:");
    // 自动任务先拿专用槽，再拿全局槽：手动任务最多只能占一个全局槽，
    // 另一个始终留给录制收尾，避免用户连点转封装把收尾饿死。
    let _exclusive = if auto {
        acquire(auto_slots(), &cancelled).await
    } else {
        acquire(manual_slots(), &cancelled).await
    };
    let _global = acquire(global_slots(), &cancelled).await;

    let outcome = if ctx.cancelled() {
        PostOutcome::source_only(src, STATE_CANCELLED, Some("后处理已取消".into()))
    } else {
        ctx.emit(2, "preparing", "running", None);
        execute(src, cfg, &ctx).await
    };

    if outcome.state == STATE_DONE {
        ctx.emit(100, "done", "done", None);
    } else if outcome.state == STATE_CANCELLED {
        ctx.emit(0, "cancelled", "cancelled", outcome.error.as_deref());
    } else {
        ctx.emit(0, "failed", "failed", outcome.error.as_deref());
    }
    tasks().lock().expect("后处理任务表被污染").remove(id);
    if let Some(jobs) = jobs.as_ref() {
        jobs.finish_external(id, &outcome.state, outcome.error.as_deref());
    }
    outcome
}

/// 任务中心显示用的分类：自动收尾、手动转封装、合并。
fn task_kind(id: &str) -> &'static str {
    if id.starts_with("manual:") {
        "remux"
    } else if id.starts_with("merge:") {
        "merge"
    } else {
        "post"
    }
}

fn emit_one(
    app: Option<&AppHandle>,
    id: &str,
    session_id: &str,
    percent: i64,
    stage: &str,
    state: &str,
    message: Option<&str>,
) {
    let Some(app) = app else { return };
    let _ = app.emit(
        EVENT_POST_PROGRESS,
        PostProgress {
            task: id.to_string(),
            session_id: session_id.to_string(),
            percent,
            stage: stage.to_string(),
            state: state.to_string(),
            message: message.map(str::to_string),
        },
    );
}

async fn execute(src: &Path, cfg: &PostConfig, ctx: &Ctx) -> PostOutcome {
    match cfg.mode.as_str() {
        settings::POST_MODE_CONVERT => run_convert(src, cfg, ctx).await,
        settings::POST_MODE_CUSTOM => run_custom(src, cfg, ctx).await,
        _ => {
            // 源封装就是母带，仍然要确认容器可读：合并命令退出码为 0 不等于文件完整。
            match validate_media(src).await {
                Ok(()) => PostOutcome::source_only(src, STATE_DONE, None),
                Err(err) => PostOutcome::source_only(
                    src,
                    STATE_FAILED,
                    Some(format!("源封装校验失败：{err}")),
                ),
            }
        }
    }
}

/// 一场录制在开录时快照下来的后处理配置；中途改设置只影响新会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostConfig {
    pub mode: String,
    pub convert_format: String,
    pub delete_source: bool,
    pub custom_command: String,
    pub custom_output_ext: String,
    pub custom_timeout_sec: u32,
}

impl Default for PostConfig {
    fn default() -> Self {
        Self {
            mode: settings::POST_MODE_SOURCE.to_string(),
            convert_format: "mp4".into(),
            delete_source: false,
            custom_command: "-i {input} -c copy {output}".into(),
            custom_output_ext: "mp4".into(),
            custom_timeout_sec: DEFAULT_TIMEOUT_SEC,
        }
    }
}

impl PostConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            mode: settings.post_mode.clone(),
            convert_format: settings.post_convert_format.clone(),
            delete_source: settings.post_delete_source,
            custom_command: settings.post_custom_command.clone(),
            custom_output_ext: sanitize_output_ext(&settings.post_custom_output_ext),
            custom_timeout_sec: settings.post_custom_timeout_sec.clamp(
                settings::POST_CUSTOM_TIMEOUT_MIN,
                settings::POST_CUSTOM_TIMEOUT_MAX,
            ),
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(
            self.custom_timeout_sec
                .max(settings::POST_CUSTOM_TIMEOUT_MIN),
        ))
    }
}

/// 后处理结果：无论成功失败，source_path 都指向可播放的源封装母带。
#[derive(Debug, Clone)]
pub struct PostOutcome {
    pub path: PathBuf,
    pub size: i64,
    pub source_path: PathBuf,
    pub state: String,
    pub error: Option<String>,
    /// 调用方在数据库提交之后才执行删除；失败时始终为 None。
    pub delete_source: Option<PathBuf>,
}

impl PostOutcome {
    fn source_only(src: &Path, state: &str, error: Option<String>) -> Self {
        let size = file_size(src);
        Self {
            path: src.to_path_buf(),
            size,
            source_path: src.to_path_buf(),
            state: state.to_string(),
            error,
            delete_source: None,
        }
    }
}

/// 源封装扩展名：FLV + HEVC/AV1 不能装进 FLV，统一用 MKV。
pub fn source_extension(format: StreamFormat, codec: Option<Codec>) -> &'static str {
    match format {
        // 音频流只有 AAC 能直接装进 m4a；其它编码用 Matroska 音频容器。
        StreamFormat::AudioOnly => match codec {
            Some(Codec::Aac) => "m4a",
            _ => "mka",
        },
        StreamFormat::Flv => match codec {
            Some(Codec::H265) | Some(Codec::Av1) => "mkv",
            _ => "flv",
        },
        // DASH 本身就是 fMP4 分片，直接落 mp4 最稳；HLS/HTTP-TS 保留 ts。
        StreamFormat::Dash => "mp4",
        StreamFormat::Hls | StreamFormat::HttpTs => "ts",
    }
}

async fn run_convert(src: &Path, cfg: &PostConfig, ctx: &Ctx) -> PostOutcome {
    let format = if settings::POST_CONVERT_FORMATS.contains(&cfg.convert_format.as_str()) {
        cfg.convert_format.as_str()
    } else {
        "mp4"
    };
    if src.extension().and_then(|ext| ext.to_str()) == Some(format) {
        return match validate_media(src).await {
            Ok(()) => PostOutcome::source_only(src, STATE_DONE, None),
            Err(err) => PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("源文件已经是 {format}，但校验失败：{err}")),
            ),
        };
    }
    let target = target_for(src, format);
    if let Some(outcome) = reuse_existing(src, &target, cfg).await {
        return outcome;
    }
    if let Err(reason) = ensure_disk_headroom(src) {
        return PostOutcome::source_only(src, STATE_FAILED, Some(reason));
    }
    let temp = temp_path(src, format);
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-nostdin"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        src.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from("0"),
        OsString::from("-c"),
        OsString::from("copy"),
        OsString::from("-avoid_negative_ts"),
        OsString::from("make_zero"),
    ];
    if matches!(format, "mp4" | "mov" | "m4v") {
        args.push(OsString::from("-movflags"));
        args.push(OsString::from("+faststart"));
    }
    // -progress 输出到 stderr，不占 stdout；加在这里而不是命令尾部，
    // 保证输出路径仍然是最后一个参数。
    args.push(OsString::from("-progress"));
    args.push(OsString::from("pipe:2"));
    args.push(OsString::from("-nostats"));
    args.push(temp.as_os_str().to_os_string());
    finish_command(src, cfg, &target, &temp, args, "转封装", ctx).await
}

async fn run_custom(src: &Path, cfg: &PostConfig, ctx: &Ctx) -> PostOutcome {
    let ext = sanitize_output_ext(&cfg.custom_output_ext);
    let target = target_for(src, &ext);
    if let Some(outcome) = reuse_existing(src, &target, cfg).await {
        return outcome;
    }
    if let Err(reason) = ensure_disk_headroom(src) {
        return PostOutcome::source_only(src, STATE_FAILED, Some(reason));
    }
    let temp = temp_path(src, &ext);
    let tokens = match parse_command(&cfg.custom_command) {
        Ok(tokens) => tokens,
        Err(err) => return PostOutcome::source_only(src, STATE_FAILED, Some(err)),
    };
    let mut args = Vec::new();
    for token in tokens {
        if args.is_empty() && is_ffmpeg_token(&token) {
            continue;
        }
        // 先替换 output 再替换 input：即使源文件名里恰好含有 "{output}"，
        // 也不会在第二步被再次替换。
        let replaced = token
            .replace("{output}", &temp.display().to_string())
            .replace("{input}", &src.display().to_string());
        args.push(OsString::from(replaced));
    }
    if args.is_empty() {
        return PostOutcome::source_only(src, STATE_FAILED, Some("自定义命令没有有效参数".into()));
    }
    inject_progress_args(&mut args, &temp);
    finish_command(src, cfg, &target, &temp, args, "自定义后处理", ctx).await
}

/// 自定义命令也要能显示百分比；把 -progress 插到输出参数之前，
/// 保证输出路径仍然是 ffmpeg 的最后一个参数。用户自己写了 -progress 就不重复加。
fn inject_progress_args(args: &mut Vec<OsString>, output: &Path) {
    if args
        .iter()
        .any(|arg| arg == "-progress" || arg == "-nostats")
    {
        return;
    }
    // 没有输出参数的命令交给 ffmpeg 自己报错；不能猜位置，否则会把 -progress
    // 插到 -i 的输入位置，错误信息会变成“输入文件无法打开”。
    let Some(at) = args.iter().rposition(|arg| arg == output.as_os_str()) else {
        return;
    };
    args.insert(at, OsString::from("-nostats"));
    args.insert(at, OsString::from("pipe:2"));
    args.insert(at, OsString::from("-progress"));
}

async fn finish_command(
    src: &Path,
    cfg: &PostConfig,
    target: &Path,
    temp: &Path,
    args: Vec<OsString>,
    label: &str,
    ctx: &Ctx,
) -> PostOutcome {
    let _ = tokio::fs::remove_file(temp).await;
    let duration = media_duration_ms(src).await;
    let mut command = Command::new(ffmpeg::binary());
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = tokio::fs::remove_file(temp).await;
            return PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("启动 ffmpeg 失败：{err}")),
            );
        }
    };
    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let reader = child
        .stderr
        .take()
        .map(|stderr| spawn_progress_reader(stderr, duration, true, ctx.clone(), tail.clone()));

    let started = Instant::now();
    let timeout = cfg.timeout();
    let mut cancelled = false;
    let mut timed_out = false;
    let status = loop {
        if ctx.cancelled() {
            cancelled = true;
            terminate(&mut child).await;
            break None;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            terminate(&mut child).await;
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => tokio::time::sleep(Duration::from_millis(150)).await,
            Err(err) => {
                terminate(&mut child).await;
                let _ = tokio::fs::remove_file(temp).await;
                return PostOutcome::source_only(
                    src,
                    STATE_FAILED,
                    Some(format!("等待 ffmpeg 退出失败：{err}")),
                );
            }
        }
    };
    if let Some(reader) = reader {
        let _ = tokio::time::timeout(Duration::from_secs(3), reader).await;
    }

    if cancelled {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_CANCELLED, Some("后处理已取消".into()));
    }
    if timed_out {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(
            src,
            STATE_FAILED,
            Some(format!("{label}超时（{} 秒）", timeout.as_secs())),
        );
    }
    let Some(status) = status else {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("{label}意外结束")));
    };
    if !status.success() {
        let stderr = tail_text(&tail);
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(
            src,
            STATE_FAILED,
            Some(explain_failure(label, &stderr, status.code())),
        );
    }
    if ctx.cancelled() {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_CANCELLED, Some("后处理已取消".into()));
    }
    if !temp.is_file() || file_size(temp) <= 0 {
        return PostOutcome::source_only(
            src,
            STATE_FAILED,
            Some(format!("{label}失败：未生成输出文件")),
        );
    }
    ctx.emit(93, "validating", "running", None);
    if let Err(err) = validate_media(temp).await {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("产物校验失败：{err}")));
    }
    let sync = match tokio::fs::File::open(temp).await {
        Ok(file) => file.sync_all().await,
        Err(err) => Err(err),
    };
    if let Err(err) = sync {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("产物落盘失败：{err}")));
    }
    if ctx.cancelled() {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_CANCELLED, Some("后处理已取消".into()));
    }
    // 目标可能来自上一次失败尝试：先移除再改名，Windows 的 rename 不覆盖。
    if target.exists() {
        if let Err(err) = tokio::fs::remove_file(target).await {
            let _ = tokio::fs::remove_file(temp).await;
            return PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("替换旧产物失败：{err}")),
            );
        }
    }
    if let Err(err) = tokio::fs::rename(temp, target).await {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("产物改名失败：{err}")));
    }
    let delete_source =
        (cfg.mode == settings::POST_MODE_CONVERT && cfg.delete_source && target != src)
            .then(|| src.to_path_buf());
    PostOutcome {
        path: target.to_path_buf(),
        size: file_size(target),
        source_path: src.to_path_buf(),
        state: STATE_DONE.to_string(),
        error: None,
        delete_source,
    }
}

/// 读取 ffmpeg stderr：一边保留最后几行给失败文案，一边把进度上报给界面。
fn spawn_progress_reader(
    stderr: tokio::process::ChildStderr,
    duration_ms: Option<u64>,
    report: bool,
    ctx: Ctx,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut last: Option<Instant> = None;
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                let mut guard = tail.lock().expect("后处理 stderr 缓存被污染");
                guard.push_back(line.clone());
                while guard.len() > 8 {
                    guard.pop_front();
                }
            }
            if !report {
                continue;
            }
            let Some(percent) = parse_progress(&line, duration_ms) else {
                continue;
            };
            let ready = last.is_none_or(|at| at.elapsed() >= Duration::from_millis(250));
            if ready {
                ctx.emit(percent, "converting", "running", None);
                last = Some(Instant::now());
            }
        }
    })
}

fn parse_progress(line: &str, duration_ms: Option<u64>) -> Option<i64> {
    let duration_ms = duration_ms?;
    if duration_ms == 0 {
        return None;
    }
    let micros = if let Some(raw) = line.strip_prefix("out_time_us=") {
        raw.trim().parse::<i64>().ok()?
    } else if let Some(raw) = line.strip_prefix("out_time_ms=") {
        // ffmpeg 的 out_time_ms 实际也是微秒，历史命名遗留。
        raw.trim().parse::<i64>().ok()?
    } else {
        let raw = line.split("time=").nth(1)?;
        let stamp = raw.split_whitespace().next()?.trim();
        parse_timestamp_ms(stamp)? * 1000
    };
    if micros <= 0 {
        return None;
    }
    let produced_ms = micros as f64 / 1000.0;
    let percent = 5.0 + produced_ms / duration_ms as f64 * 85.0;
    Some(percent.clamp(5.0, 90.0) as i64)
}

fn parse_timestamp_ms(raw: &str) -> Option<i64> {
    let mut parts = raw.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    let ms = (hours * 3600.0 + minutes * 60.0 + seconds) * 1000.0;
    ms.is_finite().then_some(ms as i64)
}

pub(crate) async fn media_duration_ms(path: &Path) -> Option<u64> {
    let probe = ffmpeg::probe_binary()?;
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(probe)
            .arg("-v")
            .arg("error")
            .arg("-show_entries")
            .arg("format=duration")
            .arg("-of")
            .arg("default=noprint_wrappers=1:nokey=1")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let seconds = text.trim().parse::<f64>().ok()?;
    (seconds.is_finite() && seconds > 0.0).then_some((seconds * 1000.0) as u64)
}

fn tail_text(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    let guard = tail.lock().expect("后处理 stderr 缓存被污染");
    guard
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 失败原因先归类成用户能看懂的一句话，再附上 ffmpeg 的最后几行用于排查。
pub fn explain_failure(label: &str, stderr: &str, code: Option<i32>) -> String {
    let lower = stderr.to_ascii_lowercase();
    let reason = if lower.contains("no space left on device") {
        "磁盘空间不足"
    } else if lower.contains("unknown encoder") || lower.contains("encoder not found") {
        "编码器不可用"
    } else if lower.contains("invalid data found")
        || lower.contains("could not find codec parameters")
        || lower.contains("moov atom not found")
    {
        "输入容器或编码无法识别"
    } else if lower.contains("no such file or directory") || lower.contains("error opening input") {
        "输入文件无法打开"
    } else if lower.contains("permission denied") {
        "没有写入权限"
    } else if lower.contains("at least one output file")
        || lower.contains("no output file")
        || lower.contains("output file #0 does not exist")
    {
        "没有生成输出文件"
    } else if lower.contains("does not contain any stream")
        || lower.contains("output file is empty")
    {
        "没有生成有效的音视频流"
    } else {
        "ffmpeg 执行失败"
    };
    let mut message = format!("{label}失败：{reason}");
    if let Some(code) = code {
        message.push_str(&format!("（退出码 {code}）"));
    }
    let detail: String = stderr
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if !detail.is_empty() {
        message.push('；');
        message.push_str(&detail.chars().take(300).collect::<String>());
    }
    message
}

async fn terminate(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

fn ensure_disk_headroom(src: &Path) -> Result<(), String> {
    let size = std::fs::metadata(src)
        .map(|meta| meta.len())
        .map_err(|err| format!("读取源文件大小失败：{err}"))?;
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let free =
        fs4::available_space(parent).map_err(|err| format!("读取磁盘剩余空间失败：{err}"))?;
    let need = size + size * DISK_HEADROOM_PERCENT / 100 + DISK_HEADROOM_BYTES;
    if free < need {
        return Err(format!(
            "磁盘剩余空间不足：需要约 {}，当前可用 {}",
            human_bytes(need),
            human_bytes(free)
        ));
    }
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or(0)
}

/// 派生文件的确定目标：同名同扩展名且不早于源文件时才复用。
///
/// 崩溃恢复会重跑后处理；每次都换 `.1` 会让数据库指向新文件、旧产物永远留下。
/// 但同名文件也可能来自上一场录制（源已删、只剩派生），所以必须比较 mtime：
/// 早于当前源文件的派生文件是别人的，跳过，不能覆盖也不能直接复用。
fn target_for(src: &Path, ext: &str) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let mut candidate = parent.join(format!("{stem}.{ext}"));
    let mut index = 1;
    // 自定义后处理的输出扩展名可能与源相同；此时绝不能把源本身当目标，
    // 否则 finish_command 会先删掉源再改名。显式跳过源文件。
    while candidate == src || candidate.exists() {
        if candidate != src && is_current_derived(&candidate, src) {
            return candidate;
        }
        candidate = parent.join(format!("{stem}.{index}.{ext}"));
        index += 1;
    }
    candidate
}

/// 派生文件是否由当前源文件产生：mtime 不早于源文件。
///
/// 崩溃恢复重跑时，源文件刚合并完，派生文件是上一次尝试留下的，mtime 更晚；
/// 上一场录制留下的同名派生文件则早于本场源文件，必须避开。
fn is_current_derived(target: &Path, src: &Path) -> bool {
    let target_time = std::fs::metadata(target).and_then(|meta| meta.modified());
    let source_time = std::fs::metadata(src).and_then(|meta| meta.modified());
    match (target_time, source_time) {
        (Ok(target_time), Ok(source_time)) => target_time >= source_time,
        _ => false,
    }
}

/// 目标已经存在且能通过校验时直接复用，避免恢复重跑生成重复文件。
async fn reuse_existing(src: &Path, target: &Path, cfg: &PostConfig) -> Option<PostOutcome> {
    if target == src || !target.is_file() || file_size(target) <= 0 {
        return None;
    }
    if !is_current_derived(target, src) {
        return None;
    }
    if validate_media(target).await.is_err() {
        return None;
    }
    let delete_source =
        (cfg.mode == settings::POST_MODE_CONVERT && cfg.delete_source && target != src)
            .then(|| src.to_path_buf());
    Some(PostOutcome {
        path: target.to_path_buf(),
        size: file_size(target),
        source_path: src.to_path_buf(),
        state: STATE_DONE.to_string(),
        error: None,
        delete_source,
    })
}

fn temp_path(src: &Path, ext: &str) -> PathBuf {
    let name = src
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".into());
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    src.with_file_name(format!(".{name}.post-{}.{}", &nonce[..8], ext))
}

/// 自定义命令模板只做分词，不经过 shell。支持单/双引号与反斜杠转义。
pub fn parse_command(template: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;
    for ch in template.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        match quote {
            Some(q) if ch == q => {
                quote = None;
                started = true;
            }
            Some(_) if ch == '\\' => escaped = true,
            Some(_) => {
                current.push(ch);
                started = true;
            }
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch == '\\' => {
                escaped = true;
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("引号没有闭合".into());
    }
    if escaped {
        return Err("结尾的反斜杠没有转义对象".into());
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

pub fn validate_custom_command(template: &str) -> Result<(), String> {
    let template = template.trim();
    if template.is_empty() {
        return Err("命令不能为空".into());
    }
    if !template.contains("{input}") || !template.contains("{output}") {
        return Err("命令必须同时包含 {input} 与 {output}".into());
    }
    let tokens = parse_command(template)?;
    if tokens.is_empty() {
        return Err("命令不能为空".into());
    }
    for token in &tokens {
        if matches!(token.as_str(), "|" | "||" | "&&" | ";" | ">" | ">>" | "<") {
            return Err("不支持 shell 管道/重定向，请只写 ffmpeg 参数".into());
        }
        if token.contains('$') && !token.contains("{input}") && !token.contains("{output}") {
            return Err("不支持 shell 变量".into());
        }
    }
    Ok(())
}

fn is_ffmpeg_token(token: &str) -> bool {
    let name = Path::new(token)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(token);
    name.eq_ignore_ascii_case("ffmpeg") || name.eq_ignore_ascii_case("ffmpeg.exe")
}

pub fn sanitize_output_ext(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "mp4".into()
    } else {
        cleaned
    }
}

/// ffprobe 校验：至少有一个音/视频流，且容器能读出时长或流信息。
pub async fn validate_media(path: &Path) -> Result<(), String> {
    let probe = ffmpeg::probe_binary();
    if probe.is_some() {
        return validate_with_ffprobe(path, probe.as_deref().unwrap()).await;
    }
    // 没有 ffprobe 时用 ffmpeg 只读前 1 秒，至少能确认容器头和第一条流可用。
    let mut command = Command::new(ffmpeg::binary());
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-t")
        .arg("1")
        .arg("-map")
        .arg("0")
        .arg("-c")
        .arg("copy")
        .arg("-f")
        .arg("null")
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(VALIDATE_TIMEOUT, command.output())
        .await
        .map_err(|_| "校验超时".to_string())?
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap_or("容器无法读取")
            .to_string())
    }
}

async fn validate_with_ffprobe(path: &Path, probe: &str) -> Result<(), String> {
    let mut command = Command::new(probe);
    command
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration:stream=index")
        .arg("-of")
        .arg("default=noprint_wrappers=1")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(VALIDATE_TIMEOUT, command.output())
        .await
        .map_err(|_| "校验超时".to_string())?
        .map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && text.lines().any(|line| line.starts_with("index=")) {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .last()
        .unwrap_or("没有可用的音视频流")
        .to_string())
}

/// 当前平台的回收站能力与位置。前端删除确认、设置页都用这份文案，避免三端说法不一。
pub fn trash_info() -> TrashInfo {
    #[cfg(target_os = "macos")]
    {
        TrashInfo {
            platform: "macos".into(),
            mode: "system".into(),
            can_restore: true,
            location: "~/.Trash".into(),
            note: "删除后可在「访达 → 废纸篓」中还原；清空废纸篓后不可恢复。".into(),
        }
    }
    #[cfg(target_os = "windows")]
    {
        TrashInfo {
            platform: "windows".into(),
            mode: "system".into(),
            can_restore: true,
            location: "回收站".into(),
            note: "删除后可在系统「回收站」中还原；清空回收站后不可恢复。".into(),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let location = trash_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "~/.local/share/Trash".into());
        TrashInfo {
            platform: "linux".into(),
            mode: "freedesktop".into(),
            can_restore: true,
            location,
            note: "按 FreeDesktop 规范移入回收站，文件管理器可还原；删除 info 目录下的 .trashinfo 会失去还原信息。".into(),
        }
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        TrashInfo {
            platform: std::env::consts::OS.to_string(),
            mode: "plain".into(),
            can_restore: false,
            location: String::new(),
            note: "当前系统没有可用的回收站实现，删除后不可恢复。".into(),
        }
    }
}

/// 回收站根目录；Linux 用于写 .trashinfo，设置页用于「打开」。
pub fn trash_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| Path::new(&home).join(".Trash"))
    }
    #[cfg(target_os = "windows")]
    {
        None
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
            .map(|data| data.join("Trash"))
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        None
    }
}

/// 把文件移入系统回收站。失败时返回错误，调用方保留源文件。
pub fn trash(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let dir = trash_dir().ok_or_else(|| "找不到 HOME".to_string())?;
        move_into(&dir, path)
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile('{}','OnlyErrorDialogs','SendToRecycleBin')",
            path.display().to_string().replace('\'', "''")
        );
        let output = std::process::Command::new("powershell")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(script)
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let root = trash_dir().ok_or_else(|| "找不到回收站目录".to_string())?;
        let files = root.join("files");
        let info = root.join("info");
        std::fs::create_dir_all(&files).map_err(|err| format!("创建回收站失败：{err}"))?;
        std::fs::create_dir_all(&info).map_err(|err| format!("创建回收站失败：{err}"))?;
        let target = unique_target(&files, path);
        let name = target
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| "recording".into());
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            url_encode_path(&absolute),
            crate::clock::format_now_local("%Y-%m-%dT%H:%M:%S")
        );
        let info_path = info.join(format!("{name}.trashinfo"));
        std::fs::write(&info_path, body).map_err(|err| format!("写回收站信息失败：{err}"))?;
        match move_to_target(path, &target) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(&info_path);
                Err(err)
            }
        }
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Err("当前系统不支持回收站".into())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn url_encode_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(unix)]
fn move_into(dir: &Path, path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("创建回收站失败：{err}"))?;
    move_to_target(path, &unique_target(dir, path))
}

#[cfg(unix)]
fn unique_target(dir: &Path, path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "recording".into());
    let mut target = dir.join(&name);
    let mut index = 1;
    while target.exists() {
        target = dir.join(format!("{name}.{index}"));
        index += 1;
    }
    target
}

#[cfg(unix)]
fn move_to_target(path: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("创建回收站失败：{err}"))?;
    }
    match std::fs::rename(path, target) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            // 跨卷时 rename 会失败：复制后删除，仍保留「回收站」语义。
            std::fs::copy(path, target).map_err(|err| format!("复制到回收站失败：{err}"))?;
            std::fs::remove_file(path)
                .map_err(|err| format!("移入回收站后删除源失败：{err}（{rename_err}）"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_mapping_covers_flv_hevc_and_audio() {
        assert_eq!(
            source_extension(StreamFormat::Flv, Some(Codec::H264)),
            "flv"
        );
        assert_eq!(
            source_extension(StreamFormat::Flv, Some(Codec::H265)),
            "mkv"
        );
        assert_eq!(source_extension(StreamFormat::Flv, Some(Codec::Av1)), "mkv");
        assert_eq!(source_extension(StreamFormat::Hls, Some(Codec::H264)), "ts");
        assert_eq!(
            source_extension(StreamFormat::HttpTs, Some(Codec::H265)),
            "ts"
        );
        assert_eq!(
            source_extension(StreamFormat::Dash, Some(Codec::H264)),
            "mp4"
        );
        assert_eq!(
            source_extension(StreamFormat::AudioOnly, Some(Codec::Aac)),
            "m4a"
        );
        assert_eq!(source_extension(StreamFormat::AudioOnly, None), "mka");
    }

    #[test]
    fn custom_template_requires_both_placeholders() {
        assert!(validate_custom_command("-i {input} -c copy {output}").is_ok());
        assert!(validate_custom_command("-i {input} -c copy out.mp4").is_err());
        assert!(validate_custom_command("-i {input} | cat > {output}").is_err());
    }

    #[test]
    fn shell_words_respect_quotes() {
        let tokens = parse_command(r#"-i "{input}" -metadata title="a b" {output}"#).unwrap();
        assert_eq!(tokens[1], "{input}");
        assert_eq!(tokens[3], "title=a b");
        assert_eq!(tokens[4], "{output}");
    }

    #[test]
    fn custom_progress_flags_are_inserted_before_output() {
        let output = Path::new("/tmp/hivecast 直播.mp4");
        let mut args = vec![
            OsString::from("-i"),
            OsString::from("/tmp/in.flv"),
            OsString::from("-c"),
            OsString::from("copy"),
            output.as_os_str().to_os_string(),
        ];
        inject_progress_args(&mut args, output);
        let text: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            &text[4..7],
            &["-progress".to_string(), "pipe:2".into(), "-nostats".into()]
        );
        assert_eq!(text.last().unwrap(), "/tmp/hivecast 直播.mp4");

        // 用户模板已经写了 -progress 时不重复插入。
        let mut custom = vec![
            OsString::from("-progress"),
            OsString::from("pipe:1"),
            output.as_os_str().to_os_string(),
        ];
        inject_progress_args(&mut custom, output);
        assert_eq!(custom.len(), 3);
    }

    #[test]
    fn target_never_collides_with_source_when_ext_matches() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        std::fs::write(&src, b"recording").unwrap();

        let target = target_for(&src, "flv");
        assert_eq!(target, dir.join("live.1.flv"));
        assert!(!target.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_derived_file_from_previous_recording_is_skipped() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        let stale = dir.join("live.mp4");
        std::fs::write(&src, b"new recording").unwrap();
        std::fs::write(&stale, b"previous recording").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let target = target_for(&src, "mp4");
        assert_eq!(target, dir.join("live.1.mp4"));
        assert!(!target.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failure_reasons_are_mapped_for_users() {
        let cases = [
            ("No space left on device", "磁盘空间不足"),
            ("Unknown encoder 'libx265'", "编码器不可用"),
            ("moov atom not found", "输入容器或编码无法识别"),
            (
                "Error opening input: No such file or directory",
                "输入文件无法打开",
            ),
            ("Permission denied", "没有写入权限"),
            (
                "At least one output file must be specified",
                "没有生成输出文件",
            ),
            ("Output file is empty", "没有生成有效的音视频流"),
            ("something odd", "ffmpeg 执行失败"),
        ];
        for (stderr, expected) in cases {
            let message = explain_failure("转封装", stderr, Some(2));
            assert!(
                message.contains(expected),
                "{stderr} 应映射为 {expected}，实际 {message}"
            );
            assert!(message.contains("退出码 2"));
        }
    }

    #[test]
    fn cancel_session_flags_only_its_own_tasks() {
        let mine = Arc::new(AtomicBool::new(false));
        let other = Arc::new(AtomicBool::new(false));
        {
            let mut guard = tasks().lock().unwrap();
            guard.insert(
                "test:cancel:mine".into(),
                TaskSlot {
                    session_id: "session-mine".into(),
                    cancelled: mine.clone(),
                },
            );
            guard.insert(
                "test:cancel:other".into(),
                TaskSlot {
                    session_id: "session-other".into(),
                    cancelled: other.clone(),
                },
            );
        }
        assert_eq!(cancel_session("session-mine"), 1);
        assert!(mine.load(Ordering::SeqCst));
        assert!(!other.load(Ordering::SeqCst));
        {
            let mut guard = tasks().lock().unwrap();
            guard.remove("test:cancel:mine");
            guard.remove("test:cancel:other");
        }
    }

    #[tokio::test]
    async fn session_cancel_stops_tasks_created_after_the_request() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        std::fs::write(&src, b"not-a-real-video").unwrap();

        let guard = begin_session("session-scope");
        assert_eq!(
            cancel_session("session-scope"),
            1,
            "收尾期间没有任务也应受理取消"
        );
        let outcome = run(
            "auto:session-scope:0",
            "session-scope",
            &src,
            &PostConfig::default(),
            None,
        )
        .await;
        assert_eq!(outcome.state, STATE_CANCELLED);
        assert_eq!(outcome.path, src);
        assert!(outcome.size > 0, "取消不应删掉源封装");
        drop(guard);
        assert!(
            !sessions().lock().unwrap().contains_key("session-scope"),
            "收尾结束后会话级取消标志必须清理"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_ext_is_sanitized() {
        assert_eq!(sanitize_output_ext(".MP4"), "mp4");
        assert_eq!(sanitize_output_ext("m kv"), "mkv");
        assert_eq!(sanitize_output_ext(""), "mp4");
    }

    async fn make_test_flv(path: &Path) {
        let status = Command::new(ffmpeg::binary())
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
                "1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-f",
                "flv",
            ])
            .arg(path)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "生成测试片失败");
    }

    /// 真跑 ffmpeg 的后处理冒烟：source 校验、convert 转封装、custom 模板都要产出可读文件。
    #[tokio::test]
    #[ignore = "需要本机 ffmpeg"]
    async fn real_ffmpeg_source_convert_and_custom_are_playable() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        make_test_flv(&src).await;

        let source = run("test:source", "s1", &src, &PostConfig::default(), None).await;
        assert_eq!(source.state, STATE_DONE, "{:?}", source.error);
        assert_eq!(source.path, src);

        let convert = run(
            "test:convert",
            "s1",
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CONVERT.into(),
                convert_format: "mp4".into(),
                ..PostConfig::default()
            },
            None,
        )
        .await;
        assert_eq!(convert.state, STATE_DONE, "{:?}", convert.error);
        assert_eq!(convert.path, dir.join("live.mp4"));
        assert!(convert.size > 0);

        let custom = run(
            "test:custom",
            "s1",
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CUSTOM.into(),
                custom_command: "-i {input} -map 0 -c copy {output}".into(),
                custom_output_ext: "mkv".into(),
                ..PostConfig::default()
            },
            None,
        )
        .await;
        assert_eq!(custom.state, STATE_DONE, "{:?}", custom.error);
        assert_eq!(custom.path, dir.join("live.mkv"));
        assert!(custom.size > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "需要本机 ffmpeg：真实编码器与超时行为"]
    async fn real_custom_transcode_handles_awkward_paths_and_failures() {
        let dir = std::env::temp_dir().join(format!(
            "hivecast post 验收 空格 🎬 {}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("直播 源 🎥.flv");
        make_test_flv(&src).await;

        // 转码型 custom：不是 -c copy，路径里同时有空格、中文和 emoji。
        let transcode = run(
            "test:custom:transcode",
            "awkward",
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CUSTOM.into(),
                custom_command: "-hide_banner -nostdin -y -i {input} -map 0 -c:v libx264 -preset ultrafast -crf 35 -pix_fmt yuv420p -c:a aac -b:a 64k {output}".into(),
                custom_output_ext: "mp4".into(),
                custom_timeout_sec: 120,
                ..PostConfig::default()
            },
            None,
        )
        .await;
        assert_eq!(transcode.state, STATE_DONE, "{:?}", transcode.error);
        assert!(transcode.path.is_file());
        assert!(transcode.size > 0);
        assert!(src.is_file(), "custom 模式永远保留源封装");

        // 只给输入、不给输出：必须映射成“没有生成输出文件”，而不是泛泛的 ffmpeg 失败。
        let no_output = run(
            "test:custom:no-output",
            "awkward",
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CUSTOM.into(),
                custom_command: "-hide_banner -nostdin -y -i {input}".into(),
                custom_output_ext: "mkv".into(),
                ..PostConfig::default()
            },
            None,
        )
        .await;
        assert_eq!(no_output.state, STATE_FAILED);
        assert!(
            no_output
                .error
                .as_deref()
                .unwrap_or("")
                .contains("没有生成输出文件"),
            "{:?}",
            no_output.error
        );

        // 不存在的编码器：映射成“编码器不可用”。
        let bad_encoder = run(
            "test:custom:bad-encoder",
            "awkward",
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CUSTOM.into(),
                custom_command:
                    "-hide_banner -nostdin -y -i {input} -c:v hivecast_no_such_encoder {output}"
                        .into(),
                custom_output_ext: "mov".into(),
                ..PostConfig::default()
            },
            None,
        )
        .await;
        assert_eq!(bad_encoder.state, STATE_FAILED);
        assert!(
            bad_encoder
                .error
                .as_deref()
                .unwrap_or("")
                .contains("编码器不可用"),
            "{:?}",
            bad_encoder.error
        );

        // 循环输入 + 10 秒超时：验证到点杀 ffmpeg 并删半成品临时文件。
        let started = Instant::now();
        let timeout = run(
            "test:custom:timeout",
            "awkward",
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CUSTOM.into(),
                custom_command: "-hide_banner -nostdin -y -re -stream_loop -1 -i {input} -t 30 -c:v libx264 -preset ultrafast -c:a aac {output}".into(),
                custom_output_ext: "ts".into(),
                custom_timeout_sec: settings::POST_CUSTOM_TIMEOUT_MIN,
                ..PostConfig::default()
            },
            None,
        )
        .await;
        assert_eq!(timeout.state, STATE_FAILED);
        assert!(
            timeout.error.as_deref().unwrap_or("").contains("超时"),
            "{:?}",
            timeout.error
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        assert!(
            !std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .any(|entry| entry.file_name().to_string_lossy().contains(".post-")),
            "失败/超时后不能留下临时文件"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
