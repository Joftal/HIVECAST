//! 录制引擎（M1 版）：一个房间一个 ffmpeg 进程，FLV 直录。
//!
//! 为什么先只做 FLV：B 站 flv 线路最稳、断流时已写入的部分可直接播放；
//! 分段 / 转封装 / 弹幕在 M2 之后再叠加（见 docs/08）。
//!
//! 状态机：idle → recording → (stopped | stream_ended | failed)。
//! 进程退出由 1 秒轮询发现（tokio::process 的 wait 不能与 stop 并存取同一子进程），
//! 退出后统一收尾：写 session、算文件大小、广播事件。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;
use std::time::{Duration, Instant};

use hivecast_ipc::{RecordSession, RecordingInfo, RoomRecord};
use tauri::{AppHandle, Emitter};
use tokio::process::Child;

use crate::broker::StreamBroker;
use crate::db::Db;
use crate::error::AppError;
use crate::ffmpeg;
use crate::platform::StreamGroup;

/// 进程存活不足这个时长就退出，视为「秒退」，需要换线路重试。
const EARLY_EXIT: Duration = Duration::from_secs(12);
/// 连续失败后的冷却时间，避免对着同一个坏房间疯狂重试。
const FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

pub const EVENT_ROOMS_CHANGED: &str = "rooms://changed";

struct Active {
    recording: RecordingInfo,
    child: Child,
    /// 用户主动停止 / 平台下播时写入，用于收尾判定
    stop_reason: Option<String>,
    started: Instant,
}

pub struct Recorder {
    db: Arc<Db>,
    media_root: PathBuf,
    inner: Mutex<HashMap<String, Active>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// 最近一次失败原因，展示在监控页行内
    errors: Mutex<HashMap<String, String>>,
    /// 每房间一把开录锁：监控循环、添加房间、手动开录可能同时触发 start，
    /// 只有先拿到锁的那次能真正拉起 ffmpeg，其余走到里面会发现已有进程直接返回。
    start_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// 用户手动停录过的房间：在主播下播前不再自动开录，避免「停了又被拉起来」。
    suppressed: Mutex<HashSet<String>>,
}

impl Recorder {
    pub fn new(db: Arc<Db>, media_root: PathBuf) -> Self {
        Self {
            db,
            media_root,
            inner: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            start_locks: Mutex::new(HashMap::new()),
            suppressed: Mutex::new(HashSet::new()),
        }
    }

    #[allow(dead_code)] // M2 媒体库扫描会用
    pub fn media_root(&self) -> &Path {
        &self.media_root
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
            return Err(AppError::Internal("该房间刚刚连续失败，稍后再试".into()));
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

        let (candidate, group) = pick_candidate(&groups)
            .ok_or_else(|| AppError::Internal("没有可用的线路".into()))?;

        let started_at = crate::db::now_ts();
        let path = session_path(
            &self.media_root,
            platform_label(&room.platform),
            &room.host_name,
            &room.title,
            started_at,
            "flv",
        );
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| AppError::from_io(err, parent))?;
        }

        let mut command = tokio::process::Command::new(ffmpeg::binary());
        command
            .arg("-hide_banner")
            .arg("-nostdin")
            .arg("-loglevel")
            .arg("warning")
            .arg("-y");

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
            .arg("-c")
            .arg("copy")
            .arg("-f")
            .arg("flv")
            .arg("-flvflags")
            .arg("no_duration_filesize")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        let child = command
            .spawn()
            .map_err(|err| AppError::Ffmpeg(format!("启动 ffmpeg 失败：{err}")))?;

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
        let file_size = tokio::fs::metadata(&active.recording.file_path)
            .await
            .map(|meta| meta.len() as i64)
            .unwrap_or(0);

        let (final_status, reason, error) = match stop_reason.as_deref() {
            Some("stopped") => ("finished", "stopped", None),
            Some("stream_ended") => ("finished", "stream_ended", None),
            Some("app_exit") => ("aborted", "app_exit", None),
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

        if let Err(err) = self.db.finish_session(
            &active.recording.session_id,
            final_status,
            reason,
            file_size,
            error.as_deref(),
        ) {
            tracing::warn!(%err, "写入录制历史失败");
        }

        if final_status == "finished" {
            self.clear_failure(room_id);
        }

        tracing::info!(
            room_id,
            status = final_status,
            reason,
            file_size,
            "录制结束"
        );
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
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

/// 选线：优先 H.264（FLV 兼容最好），其次第一个有地址的候选。
fn pick_candidate(groups: &[StreamGroup]) -> Option<(&crate::platform::StreamCandidate, &StreamGroup)> {
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

fn platform_label(platform: &str) -> &'static str {
    match platform {
        "bilibili" => "哔哩哔哩",
        "douyu" => "斗鱼",
        "huya" => "虎牙",
        "douyin" => "抖音",
        _ => "其他",
    }
}

/// 生成录制文件路径：媒体库/平台/主播/[时间][主播][标题].flv
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

fn send_graceful(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SIGINT 让 ffmpeg 正常 flush 后退出；失败再退化为强杀。
            let _ = unsafe { libc::kill(pid as i32, libc::SIGINT) };
            return;
        }
    }
    let _ = child.start_kill();
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
    fn session_path_shape() {
        let path = session_path(Path::new("/tmp/lib"), "哔哩哔哩", "主播", "标题", 1_760_000_000, "flv");
        let text = path.display().to_string();
        assert!(text.starts_with("/tmp/lib/哔哩哔哩/主播/["));
        assert!(text.ends_with("][主播][标题].flv"));
    }

    #[test]
    fn manual_stop_suppresses_auto_start() {
        let db = Arc::new(Db::open(Path::new(":memory:")).expect("内存库"));
        let recorder = Recorder::new(db, PathBuf::from("/tmp"));
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
