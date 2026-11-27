//! 日志内核：本地日期分文件、单日分段、保留清理、运行中目录热切换。
//!
//! 不用 tracing-appender 的 Rotation::DAILY：它按 UTC 切日，东八区会在早上 8 点
//! 换文件。这里用 jiff 的系统本地日期自己算日期，写入走独立线程，日志调用不会卡在磁盘 IO 上。

use std::collections::VecDeque;
use std::fmt::Write as FmtWrite;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hivecast_ipc::LogStatus;
use jiff::civil::Date;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::{DefaultVisitor, FormatFields, Writer};
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// 日志时间用本地时区：用户排障时看的应该是自己钟表上的时间，而不是 UTC。
struct LocalTime;

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(
            w,
            "{}",
            jiff::ZonedDateTime::now().strftime("%Y-%m-%d %H:%M:%S%.3f %:z")
        )
    }
}

/// 单个日志文件的目标大小；写满后自动续下一个分段，不限制每天的分段数量。
const SEGMENT_LIMIT: u64 = 16 * 1024 * 1024;
const PENDING_LIMIT: usize = 512 * 1024;
const CHANNEL_DEPTH: usize = 4096;
const DEFAULT_RETENTION_DAYS: u32 = 7;
/// 运行中目录可能被删除/卸载；每 5 秒最多 stat 一次，避免每条日志都做磁盘检查。
const DIR_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// 丢弃日志时的告警间隔，避免日志系统自己刷屏。
const DROP_WARN_INTERVAL: Duration = Duration::from_secs(60);
/// 退出时等待写线程收尾的上限；超过就分离线程，不能让磁盘故障卡住整个应用。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

static STATE: OnceLock<Arc<LogState>> = OnceLock::new();
static TX: OnceLock<SyncSender<LogMsg>> = OnceLock::new();
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

enum LogMsg {
    Write(Vec<u8>),
    Flush,
    Shutdown,
}

struct WriterState {
    dir: Option<PathBuf>,
    date: Option<Date>,
    segment: u32,
    file: Option<File>,
    written: u64,
    current: Option<PathBuf>,
    pending: VecDeque<Vec<u8>>,
    pending_bytes: usize,
    last_dir_check: Option<Instant>,
}

impl WriterState {
    fn new() -> Self {
        Self {
            dir: None,
            date: None,
            segment: 0,
            file: None,
            written: 0,
            current: None,
            pending: VecDeque::new(),
            pending_bytes: 0,
            last_dir_check: None,
        }
    }

    fn close(&mut self) {
        if let Some(mut file) = self.file.take() {
            let _ = file.flush();
        }
        self.current = None;
        self.written = 0;
    }

    fn reset_to(&mut self, dir: PathBuf) {
        self.close();
        self.dir = Some(dir);
        self.date = None;
        self.segment = 0;
        self.last_dir_check = None;
    }

    fn queue_pending(&mut self, buf: &[u8]) {
        if buf.len() > PENDING_LIMIT {
            return;
        }
        while self.pending_bytes + buf.len() > PENDING_LIMIT {
            if let Some(old) = self.pending.pop_front() {
                self.pending_bytes -= old.len();
            } else {
                break;
            }
        }
        self.pending.push_back(buf.to_vec());
        self.pending_bytes += buf.len();
    }

    /// 目录被删除/卸载后，已打开的文件描述符仍可写（内容会写进已删除的 inode），
    /// 因此不能等 open 报错。这里限频检查目录是否还在。
    fn check_dir_alive(&mut self) -> io::Result<()> {
        if self
            .last_dir_check
            .is_some_and(|at| at.elapsed() < DIR_CHECK_INTERVAL)
        {
            return Ok(());
        }
        self.last_dir_check = Some(Instant::now());
        let Some(dir) = self.dir.as_ref() else {
            return Ok(());
        };
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("日志目录已不可用：{}", dir.display()),
            ));
        }
        // 用户也可能只删掉当前日志文件；关闭旧描述符，下一次 ensure_file 会在
        // 原目录重建文件，而不是继续写进已经 unlink 的 inode。
        if self.current.as_ref().is_some_and(|path| !path.is_file()) {
            self.close();
        }
        Ok(())
    }

    fn ensure_file(&mut self) -> io::Result<()> {
        if self.dir.is_none() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "日志目录尚未就绪"));
        }
        let today = jiff::ZonedDateTime::now().date();
        // 文件已打开也要跨天检查：只看 file.is_some() 会让日志从零点一直写进昨天那个文件。
        if self.file.is_some() {
            if self.date == Some(today) {
                return Ok(());
            }
            self.close();
        }
        if self.date != Some(today) {
            self.close();
            self.date = Some(today);
            self.segment = 0;
        }
        self.open_from(self.segment)
    }

    /// 从 start 开始找第一个未满的分段；所有已有分段都满时自然打开下一个新文件。
    /// 这里不设上限：保留清理负责回收空间，日志不能因为当天写得多而丢掉。
    fn open_from(&mut self, start: u32) -> io::Result<()> {
        let dir = self
            .dir
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "日志目录尚未就绪"))?;
        let Some(date) = self.date else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "日志日期尚未就绪"));
        };
        let mut segment = start;
        loop {
            let path = segment_path(&dir, date, segment);
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if len < SEGMENT_LIMIT {
                let mut options = OpenOptions::new();
                options.create(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let file = options.open(&path)?;
                // 旧版本创建的文件可能是 0644；打开时顺手收紧，避免同机其他用户读取日志。
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
                }
                self.segment = segment;
                self.file = Some(file);
                self.written = len;
                self.current = Some(path);
                return Ok(());
            }
            segment = segment
                .checked_add(1)
                .ok_or_else(|| io::Error::other("当天日志分段编号已耗尽"))?;
        }
    }

    fn advance_segment(&mut self) -> io::Result<()> {
        let next = self
            .segment
            .checked_add(1)
            .ok_or_else(|| io::Error::other("当天日志分段编号已耗尽"))?;
        self.close();
        self.segment = next;
        self.open_from(next)
    }

    /// 清空当前文件句柄并换到另一个目录；调用方应确保目录已创建。
    fn restart_in(&mut self, dir: PathBuf, buf: &[u8]) -> io::Result<()> {
        self.reset_to(dir);
        self.write_now(buf)
    }

    fn write_now(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.dir.is_none() {
            self.queue_pending(buf);
            return Ok(());
        }
        self.check_dir_alive()?;
        self.ensure_file()?;
        if self.written > 0 && self.written + buf.len() as u64 > SEGMENT_LIMIT {
            self.advance_segment()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "日志文件不可用"))?;
        file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(())
    }
}

struct LogState {
    default_dir: PathBuf,
    inner: Mutex<WriterState>,
    dropped: AtomicU64,
    drop_warned: Mutex<Instant>,
    last_error: Mutex<Option<String>>,
    fallback: Mutex<Option<(String, String)>>,
    retention_days: AtomicU32,
}

impl LogState {
    fn new(default_dir: PathBuf) -> Self {
        Self {
            default_dir,
            inner: Mutex::new(WriterState::new()),
            dropped: AtomicU64::new(0),
            drop_warned: Mutex::new(Instant::now() - DROP_WARN_INTERVAL),
            last_error: Mutex::new(None),
            fallback: Mutex::new(None),
            retention_days: AtomicU32::new(DEFAULT_RETENTION_DAYS),
        }
    }

    fn note_error(&self, err: &io::Error) {
        if let Ok(mut last) = self.last_error.lock() {
            *last = Some(err.to_string());
        }
    }

    fn note_dropped(&self, count: u64, reason: &str) {
        if count == 0 {
            return;
        }
        let total = self.dropped.fetch_add(count, Ordering::Relaxed) + count;
        let should_warn = self
            .drop_warned
            .lock()
            .map(|mut last| {
                if last.elapsed() >= DROP_WARN_INTERVAL {
                    *last = Instant::now();
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if should_warn {
            tracing::warn!(
                count,
                total,
                reason,
                "日志被丢弃，日志系统可能跟不上或内存缓冲已满"
            );
        }
    }

    fn note_fallback(&self, from: PathBuf, to: PathBuf) {
        if let Ok(mut slot) = self.fallback.lock() {
            *slot = Some((from.display().to_string(), to.display().to_string()));
        }
    }

    fn set_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)?;
        if !path.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "不是目录"));
        }
        let mut inner = self.inner.lock().expect("日志状态被污染");
        if inner.dir.as_deref() == Some(path) && inner.file.is_some() && inner.pending.is_empty() {
            return Ok(());
        }
        inner.reset_to(path.to_path_buf());
        if let Ok(mut last) = self.last_error.lock() {
            *last = None;
        }
        // 逐条搬运 pending：中途写失败时把当前块留在队首，调用方（apply_settings）
        // 还会尝试默认目录，不能把尚未落盘的启动日志一起丢掉。
        while let Some(chunk) = inner.pending.pop_front() {
            inner.pending_bytes = inner.pending_bytes.saturating_sub(chunk.len());
            match inner.write_now(&chunk) {
                Ok(()) => {}
                Err(err) => {
                    inner.pending_bytes += chunk.len();
                    inner.pending.push_front(chunk);
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    fn write_chunk(&self, buf: &[u8]) {
        let mut inner = self.inner.lock().expect("日志状态被污染");
        let was_pending = inner.dir.is_none();
        let pending_before = inner.pending.len();
        let result = inner.write_now(buf);
        if was_pending {
            // 目录尚未就绪时才会进 pending；超限丢的是最旧的块，这里如实计数。
            let lost = (pending_before + 1).saturating_sub(inner.pending.len());
            if lost > 0 {
                self.note_dropped(lost as u64, "pending 缓冲已满或单条日志过大");
            }
        }
        match result {
            Ok(()) => {}
            Err(err) => {
                self.note_error(&err);
                let from = inner.dir.clone();
                if let Some(from) = from {
                    if from != self.default_dir {
                        let default = self.default_dir.clone();
                        // 默认目录也可能在上次运行后被清理掉，回落前先确保它存在。
                        let _ = std::fs::create_dir_all(&default);
                        self.note_fallback(from.clone(), default.clone());
                        // 这条 WARN 会经 channel 回到本线程写入新的默认文件，让日志本身留下切换证据。
                        tracing::warn!(
                            from = %from.display(),
                            to = %default.display(),
                            "日志目录不可用，已回落到默认目录"
                        );
                        match inner.restart_in(default, buf) {
                            Ok(()) => {}
                            Err(retry) => {
                                self.note_error(&retry);
                                self.note_dropped(1, "写默认目录仍然失败");
                            }
                        }
                    } else {
                        // 默认目录本身被删掉时，下一次写入应原地重建；否则一次误删会让
                        // 日志一直丢到下次重启，恰好错过最需要排查的那段时间。
                        let default = self.default_dir.clone();
                        if std::fs::create_dir_all(&default).is_ok() {
                            match inner.restart_in(default, buf) {
                                Ok(()) => {}
                                Err(retry) => {
                                    self.note_error(&retry);
                                    self.note_dropped(1, "默认目录原地重建后仍写入失败");
                                }
                            }
                        } else {
                            self.note_dropped(1, "默认日志目录无法重建");
                        }
                    }
                }
            }
        }
    }

    fn flush_inner(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(file) = inner.file.as_mut() {
                let _ = file.flush();
            }
        }
    }

    fn cleanup_current(&self) {
        let dir = self
            .inner
            .lock()
            .expect("日志状态被污染")
            .dir
            .clone()
            .unwrap_or_else(|| self.default_dir.clone());
        let current = self.inner.lock().expect("日志状态被污染").current.clone();
        if let Err(err) = cleanup_dir(
            &dir,
            self.retention_days.load(Ordering::Relaxed),
            current.as_deref(),
        ) {
            self.note_error(&err);
        }
    }
}

/// 把日志字段里的换行、制表符和其他控制字符转义，避免一条事件在文件里被拆成多行，
/// 或被伪造成另一条日志。只作用于文件层；控制台仍然保留原样，方便开发时阅读。
fn escape_log_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

struct SanitizedFields;

impl<'writer> FormatFields<'writer> for SanitizedFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut buf = String::new();
        {
            let mut visitor = DefaultVisitor::new(Writer::new(&mut buf), true);
            fields.record(&mut visitor);
        }
        FmtWrite::write_str(&mut writer, &escape_log_text(&buf))
    }
}

struct LogSink {
    tx: SyncSender<LogMsg>,
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.tx.try_send(LogMsg::Write(buf.to_vec())) {
            Ok(()) => Ok(buf.len()),
            Err(TrySendError::Full(_)) => {
                if let Some(state) = STATE.get() {
                    state.note_dropped(1, "日志通道已满");
                }
                Ok(buf.len())
            }
            Err(TrySendError::Disconnected(_)) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.tx.try_send(LogMsg::Flush);
        Ok(())
    }
}

fn writer_loop(state: Arc<LogState>, rx: std::sync::mpsc::Receiver<LogMsg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            LogMsg::Write(buf) => state.write_chunk(&buf),
            LogMsg::Flush => state.flush_inner(),
            LogMsg::Shutdown => {
                state.flush_inner();
                break;
            }
        }
    }
    state.flush_inner();
}

/// 平台默认日志目录。开发构建放工程根目录 logs/；Windows 安装版优先放安装目录 logs/，不可写时回落系统日志目录；macOS 放 ~/Library/Logs。
pub fn default_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("HIVECAST_LOG_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    if cfg!(debug_assertions) {
        if let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
            return root.join("logs");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Logs")
                .join("com.hivecast.desktop");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(root) = crate::paths::portable_root() {
            return root.join("logs");
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local)
                .join("com.hivecast.desktop")
                .join("logs");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("com.hivecast.desktop")
                .join("logs");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("logs")
}

/// 设置里空串表示默认目录。
pub fn target_dir(configured: &str) -> PathBuf {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        default_dir()
    } else {
        PathBuf::from(trimmed)
    }
}

/// 初始化文件日志；控制台保持原样（不显示 target）。
pub fn init() {
    if STATE.get().is_some() {
        return;
    }
    let dir = default_dir();
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("HIVECAST 日志目录不可用 {}：{err}", dir.display());
    }
    let state = Arc::new(LogState::new(dir));
    let (tx, rx) = sync_channel::<LogMsg>(CHANNEL_DEPTH);
    let writer_state = state.clone();
    let handle = std::thread::Builder::new()
        .name("hivecast-log".into())
        .spawn(move || writer_loop(writer_state, rx))
        .ok();
    if let Some(handle) = handle {
        if let Ok(mut slot) = THREAD.lock() {
            *slot = Some(handle);
        }
    }
    let _ = STATE.set(state);
    let _ = TX.set(tx.clone());

    // 只认 HIVECAST_LOG：RUST_LOG 常被开发环境的其他项目设成某个 crate，
    // 用户从终端启动时会让整个 HIVECAST 静默（实测 RUST_LOG=livewall=info 时一条日志都没有）。
    let filter = EnvFilter::try_from_env("HIVECAST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,hivecast_lib=debug"));
    let console = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_timer(LocalTime)
        .with_writer(std::io::stderr);
    let file_tx = tx;
    let file = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_timer(LocalTime)
        .fmt_fields(SanitizedFields)
        .with_writer(move || LogSink {
            tx: file_tx.clone(),
        });
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file)
        .try_init()
        .is_ok();
    if !installed {
        if let Some(state) = STATE.get() {
            state.note_error(&io::Error::new(
                io::ErrorKind::AlreadyExists,
                "tracing 订阅者已存在，文件日志未启用",
            ));
        }
        return;
    }
    install_panic_hook();
    spawn_cleanup();
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|one| format!("{}:{}", one.file(), one.line()))
            .unwrap_or_else(|| "unknown".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|one| (*one).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic".into());
        tracing::error!(target: "hivecast_lib::panic", location = %location, payload = %payload, "未捕获的 panic");
        previous(info);
    }));
}

fn spawn_cleanup() {
    let Some(state) = STATE.get().cloned() else {
        return;
    };
    std::thread::Builder::new()
        .name("hivecast-log-cleanup".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30 * 60));
            state.cleanup_current();
        })
        .ok();
}

pub fn set_dir(path: &Path) -> io::Result<()> {
    let state = STATE
        .get()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "日志系统尚未初始化"))?;
    state.set_dir(path)
}

/// 启动/保存设置时应用目录与保留天数；目录不可用时自动回落到默认目录。
pub fn apply_settings(configured: &str, retention_days: u32) -> Result<PathBuf, String> {
    set_retention(retention_days);
    let target = target_dir(configured);
    match set_dir(&target) {
        Ok(()) => Ok(target),
        Err(err) => {
            let default = default_dir();
            let _ = std::fs::create_dir_all(&default);
            let retry = set_dir(&default);
            if let Some(state) = STATE.get() {
                state.note_fallback(target.clone(), default.clone());
                state.note_error(&err);
            }
            match retry {
                Ok(()) => Err(format!(
                    "{} 不可用（{err}），已回落到 {}",
                    target.display(),
                    default.display()
                )),
                Err(retry_err) => Err(format!(
                    "{} 与默认目录 {} 都不可用：{err} / {retry_err}",
                    target.display(),
                    default.display()
                )),
            }
        }
    }
}

pub fn set_retention(days: u32) {
    if let Some(state) = STATE.get() {
        state.retention_days.store(days, Ordering::Relaxed);
    }
}

pub fn cleanup(retention_days: u32) {
    set_retention(retention_days);
    if let Some(state) = STATE.get() {
        state.cleanup_current();
    }
}

pub fn flush() {
    if let Some(tx) = TX.get() {
        let _ = tx.try_send(LogMsg::Flush);
    }
}

pub fn shutdown() {
    // 应用在读到设置前就退出（DB 打不开、单实例让位）时目录仍是 None；
    // 此时启动日志还在 pending 里，先落到默认目录，别让最关键的启动失败没有文件记录。
    if let Some(state) = STATE.get() {
        let unresolved = state
            .inner
            .lock()
            .map(|inner| inner.dir.is_none())
            .unwrap_or(false);
        if unresolved {
            let _ = state.set_dir(&state.default_dir.clone());
        }
    }
    // 先排队一次 flush，保证退出前最后几条日志落盘，再让写线程收尾。
    // 这里不能用阻塞 send：通道满时主线程会卡在这里，正好是日志系统最忙的时候。
    flush();
    if let Some(tx) = TX.get() {
        if tx.try_send(LogMsg::Shutdown).is_err() {
            eprintln!("日志通道已满，跳过 Shutdown 消息，等待写线程按超时退出");
        }
    }
    let handle = THREAD.lock().ok().and_then(|mut slot| slot.take());
    let Some(handle) = handle else {
        return;
    };
    // join 也可能被卡死的磁盘 I/O 拖住；把 join 放到旁路线程，主线程最多等 2 秒。
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let joiner = std::thread::Builder::new()
        .name("hivecast-log-join".into())
        .spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });
    match joiner {
        Ok(joiner) => {
            if done_rx.recv_timeout(SHUTDOWN_TIMEOUT).is_err() {
                eprintln!(
                    "日志写线程在 {} 秒内未退出，已分离；继续退出应用",
                    SHUTDOWN_TIMEOUT.as_secs()
                );
            } else {
                let _ = joiner.join();
            }
        }
        Err(err) => eprintln!("无法创建日志收尾线程：{err}；继续退出应用"),
    }
}

pub fn status() -> LogStatus {
    let Some(state) = STATE.get() else {
        return LogStatus {
            dir: default_dir().display().to_string(),
            default_dir: default_dir().display().to_string(),
            configured: String::new(),
            retention_days: DEFAULT_RETENTION_DAYS,
            current_file: None,
            file_count: 0,
            total_bytes: 0,
            dropped: 0,
            last_error: Some("日志系统尚未初始化".into()),
            fallback: None,
        };
    };
    let inner = state.inner.lock().expect("日志状态被污染");
    let dir = inner
        .dir
        .clone()
        .unwrap_or_else(|| state.default_dir.clone());
    let current = inner.current.clone();
    drop(inner);
    let (file_count, total_bytes) = scan_dir(&dir);
    LogStatus {
        dir: dir.display().to_string(),
        default_dir: state.default_dir.display().to_string(),
        configured: String::new(),
        retention_days: state.retention_days.load(Ordering::Relaxed),
        current_file: current.map(|path| path.display().to_string()),
        file_count,
        total_bytes,
        dropped: state.dropped.load(Ordering::Relaxed),
        last_error: state.last_error.lock().ok().and_then(|one| one.clone()),
        fallback: state
            .fallback
            .lock()
            .ok()
            .and_then(|one| one.clone())
            .map(|(from, to)| format!("{from} → {to}")),
    }
}

pub fn take_fallback() -> Option<(String, String)> {
    STATE
        .get()
        .and_then(|state| state.fallback.lock().ok().and_then(|mut one| one.take()))
}

fn days_before(date: Date, days: i64) -> Date {
    date.checked_sub(jiff::Span::new().days(days))
        .unwrap_or(date)
}

fn segment_path(dir: &Path, date: Date, segment: u32) -> PathBuf {
    let stem = format!("hivecast-{}", date.strftime("%Y-%m-%d"));
    if segment == 0 {
        dir.join(format!("{stem}.log"))
    } else {
        dir.join(format!("{stem}.{segment}.log"))
    }
}

fn parse_log_date(name: &str) -> Option<Date> {
    let rest = name.strip_prefix("hivecast-")?.strip_suffix(".log")?;
    let (date, segment) = match rest.split_once('.') {
        Some((date, segment)) => (date, Some(segment)),
        None => (rest, None),
    };
    if date.len() != 10 {
        return None;
    }
    if let Some(segment) = segment {
        if segment.is_empty() || !segment.chars().all(|one| one.is_ascii_digit()) {
            return None;
        }
    }
    Date::strptime("%Y-%m-%d", date).ok()
}

fn scan_dir(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut count = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|one| one.to_str()) else {
            continue;
        };
        if parse_log_date(name).is_none() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                count += 1;
                bytes += meta.len();
            }
        }
    }
    (count, bytes)
}

fn cleanup_dir(dir: &Path, retention_days: u32, current: Option<&Path>) -> io::Result<()> {
    if retention_days == 0 {
        return Ok(());
    }
    let today = jiff::ZonedDateTime::now().date();
    let keep_from = days_before(today, (retention_days.saturating_sub(1)) as i64);
    let entries = std::fs::read_dir(dir)?;
    let mut first_error = None;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                first_error.get_or_insert(err);
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|one| one.to_str()) else {
            continue;
        };
        let Some(date) = parse_log_date(name) else {
            continue;
        };
        if date >= keep_from {
            continue;
        }
        if current.is_some_and(|one| one == path) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(err) => {
                first_error.get_or_insert(err);
                continue;
            }
        };
        if !meta.is_file() {
            continue;
        }
        if let Err(err) = std::fs::remove_file(&path) {
            first_error.get_or_insert(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hivecast-log-{tag}-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn log_names_are_recognized_strictly() {
        assert!(parse_log_date("hivecast-2026-11-07.log").is_some());
        assert!(parse_log_date("hivecast-2026-11-07.3.log").is_some());
        assert!(parse_log_date("hivecast-2026-11-07.log.1").is_none());
        assert!(parse_log_date("hivecast-2026-13-07.log").is_none());
        assert!(parse_log_date("other-2026-11-07.log").is_none());
        assert!(parse_log_date("hivecast-2026-11-07.txt").is_none());
    }

    #[test]
    fn log_field_values_escape_newlines_and_controls() {
        assert_eq!(
            escape_log_text("line1\nline2\r\tend\u{1b}"),
            "line1\\nline2\\r\\tend\\u{1b}"
        );
        assert_eq!(escape_log_text("普通日志"), "普通日志");
    }

    #[test]
    fn cleanup_keeps_today_and_retention_window() {
        let dir = temp_dir("cleanup");
        let today = jiff::ZonedDateTime::now().date();
        let old = days_before(today, 8);
        let keep = days_before(today, 3);
        std::fs::write(segment_path(&dir, old, 0), b"old").unwrap();
        std::fs::write(segment_path(&dir, old, 1), b"old").unwrap();
        std::fs::write(segment_path(&dir, keep, 0), b"keep").unwrap();
        std::fs::write(segment_path(&dir, today, 0), b"now").unwrap();
        cleanup_dir(&dir, 7, None).unwrap();
        assert!(!segment_path(&dir, old, 0).exists());
        assert!(!segment_path(&dir, old, 1).exists());
        assert!(segment_path(&dir, keep, 0).exists());
        assert!(segment_path(&dir, today, 0).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_zero_keeps_everything() {
        let dir = temp_dir("keep");
        let old = days_before(jiff::ZonedDateTime::now().date(), 30);
        std::fs::write(segment_path(&dir, old, 0), b"old").unwrap();
        cleanup_dir(&dir, 0, None).unwrap();
        assert!(segment_path(&dir, old, 0).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_pends_until_a_directory_is_set() {
        let dir = temp_dir("pending");
        let state = LogState::new(dir.clone());
        state.write_chunk(b"before settings\n");
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner.dir.is_none());
            assert_eq!(inner.pending.len(), 1);
        }
        state.set_dir(&dir).unwrap();
        assert!(segment_path(&dir, jiff::ZonedDateTime::now().date(), 0).exists());
        assert!(
            std::fs::read_to_string(segment_path(&dir, jiff::ZonedDateTime::now().date(), 0))
                .unwrap()
                .contains("before settings")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_rolls_over_when_the_local_date_changes() {
        let dir = temp_dir("rollover");
        let state = LogState::new(dir.clone());
        state.set_dir(&dir).unwrap();
        let yesterday = days_before(jiff::ZonedDateTime::now().date(), 1);
        let old_path = segment_path(&dir, yesterday, 0);
        let old_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&old_path)
            .unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.date = Some(yesterday);
            inner.segment = 0;
            inner.file = Some(old_file);
            inner.current = Some(old_path.clone());
        }
        state.write_chunk(b"new day\n");
        let today_path = segment_path(&dir, jiff::ZonedDateTime::now().date(), 0);
        assert!(today_path.exists(), "跨天后要新开今天的文件");
        assert!(std::fs::read_to_string(&today_path)
            .unwrap()
            .contains("new day"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sparse_segment(dir: &Path, date: Date, segment: u32, len: u64) -> PathBuf {
        let path = segment_path(dir, date, segment);
        let file = File::create(&path).unwrap();
        file.set_len(len).unwrap();
        path
    }

    #[test]
    fn full_segments_continue_into_a_new_segment_without_fallback() {
        let custom = temp_dir("full-custom");
        let fallback = temp_dir("full-default");
        let today = jiff::ZonedDateTime::now().date();
        let existing = 4;
        for segment in 0..existing {
            sparse_segment(&custom, today, segment, SEGMENT_LIMIT);
        }

        let state = LogState::new(fallback.clone());
        state.set_dir(&custom).unwrap();
        state.write_chunk(b"after all segments are full\n");

        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.dir.as_deref(),
            Some(custom.as_path()),
            "不能切到默认目录"
        );
        assert!(inner.file.is_some(), "应继续打开下一个分段");
        assert_eq!(inner.segment, existing);
        drop(inner);
        assert!(state.fallback.lock().unwrap().is_none(), "不应记录目录回落");
        assert_eq!(state.dropped.load(Ordering::Relaxed), 0, "丢日志才计数");
        assert_eq!(
            std::fs::read_to_string(segment_path(&custom, today, existing)).unwrap(),
            "after all segments are full\n"
        );
        assert!(
            !segment_path(&fallback, today, 0).exists(),
            "续段不是目录故障，默认目录不应被写入"
        );
        let _ = std::fs::remove_dir_all(&custom);
        let _ = std::fs::remove_dir_all(&fallback);
    }

    #[test]
    fn writer_rotates_when_the_next_line_would_exceed_the_limit() {
        let dir = temp_dir("rotate");
        let today = jiff::ZonedDateTime::now().date();
        let first = sparse_segment(&dir, today, 0, SEGMENT_LIMIT - 10);
        let state = LogState::new(dir.clone());
        state.set_dir(&dir).unwrap();
        state.write_chunk(b"next segment line\n");

        let second = segment_path(&dir, today, 1);
        assert_eq!(
            std::fs::metadata(&first).unwrap().len(),
            SEGMENT_LIMIT - 10,
            "旧分段不应再被写入"
        );
        assert_eq!(
            std::fs::read_to_string(&second).unwrap(),
            "next segment line\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_appends_to_an_existing_file_after_restart() {
        let dir = temp_dir("append");
        let today = jiff::ZonedDateTime::now().date();
        let path = segment_path(&dir, today, 0);
        std::fs::write(&path, b"first process\n").unwrap();
        let state = LogState::new(dir.clone());
        state.set_dir(&dir).unwrap();
        state.write_chunk(b"second process\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first process\nsecond process\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_buffer_is_bounded_and_drains_oldest_first() {
        let dir = temp_dir("pending-limit");
        let state = LogState::new(dir.clone());
        let chunk = vec![b'x'; 100 * 1024];
        for _ in 0..6 {
            state.write_chunk(&chunk);
        }
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner.pending_bytes <= PENDING_LIMIT);
            assert_eq!(inner.pending.len(), 5, "超限时丢最旧的一条");
        }
        assert_eq!(state.dropped.load(Ordering::Relaxed), 1);
        state.set_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(segment_path(&dir, jiff::ZonedDateTime::now().date(), 0))
                .unwrap()
                .len(),
            5 * 100 * 1024
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_directory_switch_keeps_pending_for_the_fallback() {
        let custom = temp_dir("pending-switch-custom");
        let default = temp_dir("pending-switch-default");
        let state = LogState::new(default.clone());
        state.write_chunk(b"startup line\n");

        // 让自定义目录里的当天日志文件被一个同名目录占住，open 必然失败。
        let blocked = segment_path(&custom, jiff::ZonedDateTime::now().date(), 0);
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(state.set_dir(&custom).is_err());
        {
            let inner = state.inner.lock().unwrap();
            assert_eq!(inner.pending.len(), 1, "失败时启动日志不应被丢弃");
            assert_eq!(inner.pending_bytes, b"startup line\n".len());
        }

        // 与 apply_settings 的回落路径一致：换到默认目录后继续搬运。
        state.set_dir(&default).unwrap();
        let path = segment_path(&default, jiff::ZonedDateTime::now().date(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "startup line\n");
        assert_eq!(state.dropped.load(Ordering::Relaxed), 0);
        assert!(state.inner.lock().unwrap().pending.is_empty());

        let _ = std::fs::remove_dir_all(&custom);
        let _ = std::fs::remove_dir_all(&default);
    }

    #[test]
    fn write_failure_falls_back_to_the_default_directory() {
        let custom = temp_dir("fallback-custom");
        let default = temp_dir("fallback-default");
        let state = LogState::new(default.clone());
        state.set_dir(&custom).unwrap();
        std::fs::remove_dir_all(&custom).unwrap();
        state.write_chunk(b"fallback line\n");

        let path = segment_path(&default, jiff::ZonedDateTime::now().date(), 0);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("fallback line"));
        assert_eq!(
            state.fallback.lock().unwrap().clone(),
            Some((custom.display().to_string(), default.display().to_string()))
        );
        assert_eq!(state.dropped.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(&default);
    }

    #[test]
    fn deleting_the_active_directory_is_detected_without_waiting_for_rotation() {
        let custom = temp_dir("deleted-custom");
        let default = temp_dir("deleted-default");
        let state = LogState::new(default.clone());
        state.set_dir(&custom).unwrap();
        state.write_chunk(b"before delete\n");
        std::fs::remove_dir_all(&custom).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.last_dir_check =
                Some(Instant::now() - DIR_CHECK_INTERVAL - Duration::from_secs(1));
        }
        state.write_chunk(b"after delete\n");

        let path = segment_path(&default, jiff::ZonedDateTime::now().date(), 0);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("after delete"));
        assert!(state.fallback.lock().unwrap().is_some());
        let _ = std::fs::remove_dir_all(&default);
    }

    #[test]
    fn deleting_the_default_directory_is_recreated_in_place() {
        let dir = temp_dir("deleted-default-dir");
        let state = LogState::new(dir.clone());
        state.set_dir(&dir).unwrap();
        state.write_chunk(b"before delete\n");
        std::fs::remove_dir_all(&dir).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.last_dir_check =
                Some(Instant::now() - DIR_CHECK_INTERVAL - Duration::from_secs(1));
        }
        state.write_chunk(b"after delete\n");

        let path = segment_path(&dir, jiff::ZonedDateTime::now().date(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after delete\n");
        assert_eq!(state.dropped.load(Ordering::Relaxed), 0);
        assert!(state.fallback.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_only_the_current_file_reopens_it_in_place() {
        let dir = temp_dir("deleted-file");
        let default = temp_dir("deleted-file-default");
        let state = LogState::new(default.clone());
        state.set_dir(&dir).unwrap();
        state.write_chunk(b"before delete\n");
        let path = segment_path(&dir, jiff::ZonedDateTime::now().date(), 0);
        std::fs::remove_file(&path).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.last_dir_check =
                Some(Instant::now() - DIR_CHECK_INTERVAL - Duration::from_secs(1));
        }
        state.write_chunk(b"after delete\n");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after delete\n");
        assert!(state.fallback.lock().unwrap().is_none());
        assert!(!segment_path(&default, jiff::ZonedDateTime::now().date(), 0).exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&default);
    }

    #[cfg(unix)]
    #[test]
    fn log_files_are_owner_only_and_existing_files_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = segment_path(&dir, jiff::ZonedDateTime::now().date(), 0);
        std::fs::write(&path, b"old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let state = LogState::new(dir.clone());
        state.set_dir(&dir).unwrap();
        state.write_chunk(b"new\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_uses_an_inclusive_retention_window() {
        let dir = temp_dir("cleanup-boundary");
        let today = jiff::ZonedDateTime::now().date();
        let drop = days_before(today, 7);
        let keep = days_before(today, 6);
        std::fs::write(segment_path(&dir, drop, 0), b"drop").unwrap();
        std::fs::write(segment_path(&dir, keep, 3), b"keep").unwrap();
        cleanup_dir(&dir, 7, None).unwrap();
        assert!(!segment_path(&dir, drop, 0).exists());
        assert!(segment_path(&dir, keep, 3).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_only_touches_regular_files_with_log_names() {
        let dir = temp_dir("cleanup-shape");
        let old = days_before(jiff::ZonedDateTime::now().date(), 30);
        let real = segment_path(&dir, old, 0);
        std::fs::write(&real, b"old").unwrap();
        std::fs::write(segment_path(&dir, old, 1), b"old").unwrap();
        std::fs::write(dir.join("hivecast-2020-01-01.log.bak"), b"keep").unwrap();
        std::fs::create_dir(dir.join("hivecast-2020-01-02.log")).unwrap();
        cleanup_dir(&dir, 7, None).unwrap();
        assert!(!real.exists());
        assert!(!segment_path(&dir, old, 1).exists());
        assert!(dir.join("hivecast-2020-01-01.log.bak").exists());
        assert!(dir.join("hivecast-2020-01-02.log").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
