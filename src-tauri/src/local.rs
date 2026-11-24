//! 本地录像回放（M6 媒体库）：把一条录像的「一段」转成 HLS，复用观看端的播放器。
//!
//! 与观看端的区别只有输入源：这里是磁盘文件，观看端是平台直播流。
//! 允许同时回放多条（上限 MAX_ACTIVE）：媒体库会并排比较不同录像，但每条都要
//! 单独起一个 ffmpeg 转封装、单独占一份分片缓存，所以必须设上限。
//!
//! M13 起不再整段转封装：一小时录像整段转要额外占一份和录像等大的磁盘，
//! 现在只转播放点附近的一个窗口（WINDOW_MS），拖到窗口外就换一个窗口重开。
//! 窗口起点会比播放点提前 LEAD_MS，往回拖一小段不用重开。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hivecast_ipc::Playback;
use tokio::process::{Child, Command};

use crate::db::Db;
use crate::error::AppError;
use crate::ffmpeg;
use crate::process::{send_graceful, wait_exit};
use crate::stream::StreamServer;

const READY_TIMEOUT: Duration = Duration::from_secs(12);
const STOP_TIMEOUT: Duration = Duration::from_secs(4);
/// 同时回放上限：每条占一个 ffmpeg 进程和一整套 HLS 分片。
const MAX_ACTIVE: usize = 3;
/// 一个窗口的时长。
pub const WINDOW_MS: i64 = 180_000;
/// 窗口起点相对播放点的提前量：往回拖一点不必重开窗口。
pub const LEAD_MS: i64 = 45_000;

struct Active {
    key: String,
    dir: PathBuf,
    child: Child,
    start_ms: i64,
    window_ms: i64,
}

impl Active {
    fn contains(&self, position_ms: i64) -> bool {
        contains(self.start_ms, self.window_ms, position_ms)
    }
}

pub struct LocalPlayer {
    root: PathBuf,
    server: Arc<StreamServer>,
    current: Mutex<HashMap<String, Active>>,
    gate: tokio::sync::Mutex<()>,
}

impl LocalPlayer {
    pub fn new(root: PathBuf, server: Arc<StreamServer>) -> Arc<Self> {
        Arc::new(Self {
            root,
            server,
            current: Mutex::new(HashMap::new()),
            gate: tokio::sync::Mutex::new(()),
        })
    }

    /// 打开一条录像的某个位置；已在放且位置落在当前窗口里就直接复用。
    pub async fn open(
        &self,
        db: &Db,
        session_id: &str,
        position_ms: Option<i64>,
        source: bool,
    ) -> Result<Playback, AppError> {
        let _gate = self.gate.lock().await;

        let session = db.get_session(session_id)?.ok_or_else(|| {
            tracing::warn!(session_id, "本地回放：录像记录不存在");
            AppError::User("录像不存在".into())
        })?;
        if session.status == "recording" {
            tracing::info!(session_id, "本地回放被拒绝：录像仍在录制");
            return Err(AppError::User("这段还在录制，先停下再回放".into()));
        }
        // 主文件和源封装是两个独立变体：各自一个 HLS 会话，互不覆盖。
        // 同一会话里切换「看主文件 / 看源封装」时前端会先停旧变体。
        let slot = format!("{session_id}:{}", if source { "source" } else { "main" });
        let path = if source {
            session.source_path.clone()
        } else {
            session.file_path.clone()
        }
        .filter(|p| Path::new(p).is_file())
        .ok_or_else(|| {
            tracing::warn!(session_id, source, "本地回放：录像文件已丢失");
            AppError::User(if source {
                "源封装文件已不存在".into()
            } else {
                "录像文件已不存在".into()
            })
        })?;

        let duration_ms = session.duration_ms.max(0);
        let wanted = position_ms.unwrap_or(0).clamp(0, duration_ms);
        let title = db
            .get_room(&session.room_id)?
            .map(|room| room.title)
            .unwrap_or_default();

        // 已经在放同一条、且目标位置在窗口内：复用现成的 HLS，别重复转封装。
        let stale = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            match guard.get(&slot) {
                Some(active) if active.contains(wanted) => {
                    tracing::debug!(
                        session_id,
                        source,
                        position_ms = wanted,
                        window_start_ms = active.start_ms,
                        "本地回放命中当前窗口，复用现有转封装"
                    );
                    return Ok(playback(
                        self.server.playlist_url(&active.key),
                        session_id,
                        &title,
                        duration_ms,
                        active.start_ms,
                        active.window_ms,
                        wanted - active.start_ms,
                    ));
                }
                Some(_) => guard.remove(&slot),
                None => None,
            }
        };
        if let Some(active) = stale {
            tracing::debug!(
                session_id,
                window_start_ms = active.start_ms,
                "本地回放换窗口，先收掉旧转封装"
            );
            teardown(self.server.as_ref(), active).await;
        }

        {
            let guard = self.current.lock().expect("本地回放状态被污染");
            if guard.len() >= MAX_ACTIVE {
                tracing::warn!(
                    session_id,
                    active = guard.len(),
                    limit = MAX_ACTIVE,
                    "本地回放被拒绝：并发已达上限"
                );
                return Err(AppError::User(format!(
                    "最多同时回放 {MAX_ACTIVE} 条，先停掉一条再开"
                )));
            }
        }

        let (start_ms, window_ms) = window_for(wanted, duration_ms);
        let dir = self.root.join(format!("{}-{}", session_id, now_ms()));
        tokio::fs::create_dir_all(&dir).await.map_err(|err| {
            tracing::warn!(session_id, dir = %dir.display(), error = %err, "本地回放：创建分片目录失败");
            AppError::from_io(err, &dir)
        })?;

        let mut child = spawn_window(Path::new(&path), &dir, start_ms, window_ms).map_err(|err| {
            tracing::warn!(session_id, file = %path, error = %err, "本地回放：启动 ffmpeg 失败");
            err
        })?;

        // 等第一个分片落盘再交给播放器，否则 hls.js 会拿到 404 的 m3u8。
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if dir.join("index.m3u8").is_file() {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                tracing::warn!(session_id, code = ?status.code(), "本地回放：ffmpeg 在出片前退出");
                if let Err(err) = tokio::fs::remove_dir_all(&dir).await {
                    tracing::debug!(dir = %dir.display(), error = %err, "清理本地回放临时目录失败");
                }
                return Err(AppError::Ffmpeg(format!(
                    "本地回放启动失败（ffmpeg 退出码 {:?}）",
                    status.code()
                )));
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    session_id,
                    timeout_secs = READY_TIMEOUT.as_secs(),
                    "本地回放：等待首个分片超时"
                );
                send_graceful(&mut child);
                if wait_exit(&mut child, STOP_TIMEOUT).await.is_none() {
                    tracing::debug!(
                        session_id,
                        timeout_secs = STOP_TIMEOUT.as_secs(),
                        "等待本地回放 ffmpeg 退出超时"
                    );
                }
                if let Err(err) = tokio::fs::remove_dir_all(&dir).await {
                    tracing::debug!(dir = %dir.display(), error = %err, "清理本地回放临时目录失败");
                }
                return Err(AppError::User("本地回放启动超时".into()));
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }

        let key = format!("play-{}", now_ms());
        self.server.register(&key, dir.clone());
        tracing::info!(
            session_id,
            file = %path,
            start_ms,
            window_ms,
            dir = %dir.display(),
            "本地回放已就绪"
        );
        {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            guard.insert(
                slot.clone(),
                Active {
                    key: key.clone(),
                    dir,
                    child,
                    start_ms,
                    window_ms,
                },
            );
        }

        Ok(playback(
            self.server.playlist_url(&key),
            session_id,
            &title,
            duration_ms,
            start_ms,
            window_ms,
            wanted - start_ms,
        ))
    }

    /// 只停指定的一条（媒体库删某条录像时用，不能牵连其它正在放的）。
    pub async fn stop(&self, session_id: &str) {
        let _gate = self.gate.lock().await;
        let removed = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            let prefix = format!("{session_id}:");
            let keys: Vec<String> = guard
                .keys()
                .filter(|key| key.as_str() == session_id || key.starts_with(&prefix))
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| guard.remove(&key))
                .collect::<Vec<_>>()
        };
        if removed.is_empty() {
            tracing::debug!(session_id, "停止本地回放：当前没有在放");
        } else {
            tracing::info!(session_id, count = removed.len(), "停止本地回放");
            for active in removed {
                teardown(self.server.as_ref(), active).await;
            }
        }
    }

    /// 退出前停掉全部回放。
    pub async fn stop_all(&self) {
        let _gate = self.gate.lock().await;
        let all = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            std::mem::take(&mut *guard)
        };
        if !all.is_empty() {
            tracing::info!(count = all.len(), "退出前停止全部本地回放");
        }
        for (_, active) in all {
            teardown(self.server.as_ref(), active).await;
        }
    }
}

/// 左闭右开的窗口判定。
pub fn contains(start_ms: i64, window_ms: i64, position_ms: i64) -> bool {
    position_ms >= start_ms && position_ms < start_ms + window_ms
}

/// 算出窗口：比播放点提前 LEAD_MS，窗口长度 WINDOW_MS，末尾贴住录像结尾。
pub fn window_for(position_ms: i64, duration_ms: i64) -> (i64, i64) {
    let duration = duration_ms.max(0);
    let window_ms = if duration > 0 {
        WINDOW_MS.min(duration.max(1_000))
    } else {
        WINDOW_MS
    };
    let max_start = (duration - window_ms).max(0);
    let start = (position_ms.max(0) - LEAD_MS).clamp(0, max_start);
    (start, window_ms)
}

fn spawn_window(path: &Path, dir: &Path, start_ms: i64, window_ms: i64) -> Result<Child, AppError> {
    let mut command = Command::new(ffmpeg::binary());
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("warning")
        .arg("-y");
    // -ss 放在 -i 前面：按关键帧快速定位，不用把前面整段解一遍。
    if start_ms > 0 {
        command
            .arg("-ss")
            .arg(format!("{:.3}", start_ms as f64 / 1000.0));
    }
    command
        .arg("-i")
        .arg(path)
        .arg("-t")
        .arg(format!("{:.3}", window_ms as f64 / 1000.0))
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a:0?")
        .arg("-c")
        .arg("copy")
        .arg("-f")
        .arg("hls")
        .arg("-hls_time")
        .arg("2")
        // 窗口内是点播：整套分片保留，播放器可以在窗口里自由拖动。
        // 用 mpegts 而不是 fmp4 —— ffmpeg 9 把 flv 文件转 fmp4 时会写出缺 tfhd
        // 的 moof，hls.js 直接 mediaSourceRequiresReset，实测不可用。
        .arg("-hls_playlist_type")
        .arg("vod")
        .arg("-hls_flags")
        .arg("independent_segments")
        .arg("-hls_segment_filename")
        .arg(dir.join("seg_%05d.ts"))
        .arg(dir.join("index.m3u8"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    command
        .spawn()
        .map_err(|err| AppError::Ffmpeg(format!("启动本地回放失败：{err}")))
}

fn playback(
    url: String,
    session_id: &str,
    title: &str,
    duration_ms: i64,
    start_ms: i64,
    window_ms: i64,
    offset_ms: i64,
) -> Playback {
    Playback {
        url,
        session_id: session_id.to_string(),
        title: title.to_string(),
        duration_ms,
        start_ms,
        window_ms,
        lead_ms: LEAD_MS,
        offset_ms,
    }
}

/// 收尾一个回放：先礼后兵，再注销 URL、删掉分片目录。
async fn teardown(server: &StreamServer, mut active: Active) {
    send_graceful(&mut active.child);
    if wait_exit(&mut active.child, STOP_TIMEOUT).await.is_none() {
        tracing::debug!(session = %active.key, timeout_secs = STOP_TIMEOUT.as_secs(), "等待本地回放 ffmpeg 退出超时，继续清理");
    }
    server.unregister(&active.key);
    if let Err(err) = tokio::fs::remove_dir_all(&active.dir).await {
        tracing::debug!(session = %active.key, dir = %active.dir.display(), error = %err, "删除本地回放分片目录失败");
    }
}

fn now_ms() -> i64 {
    crate::clock::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_starts_a_bit_before_the_playhead() {
        // 10 分钟录像，从第 5 分钟开始看：窗口 4:15–7:15。
        let (start, len) = window_for(300_000, 600_000);
        assert_eq!(start, 255_000);
        assert_eq!(len, WINDOW_MS);
    }

    #[test]
    fn window_clamps_to_the_tail() {
        // 靠近结尾：窗口末尾贴住录像结尾，不越过。
        let (start, len) = window_for(595_000, 600_000);
        assert_eq!(start + len, 600_000);
        assert_eq!(start, 420_000);
    }

    #[test]
    fn window_starts_at_zero_near_the_head() {
        let (start, len) = window_for(5_000, 600_000);
        assert_eq!(start, 0);
        assert_eq!(len, WINDOW_MS);
    }

    #[test]
    fn short_recording_uses_the_whole_file() {
        let (start, len) = window_for(10_000, 42_000);
        assert_eq!(start, 0);
        assert_eq!(len, 42_000);
    }

    #[test]
    fn unknown_duration_falls_back_to_one_window() {
        let (start, len) = window_for(0, 0);
        assert_eq!(start, 0);
        assert_eq!(len, WINDOW_MS);
    }

    #[test]
    fn window_covers_a_half_open_range() {
        assert!(contains(255_000, WINDOW_MS, 255_000));
        assert!(contains(255_000, WINDOW_MS, 434_999));
        assert!(!contains(255_000, WINDOW_MS, 435_000));
        assert!(!contains(255_000, WINDOW_MS, 254_999));
    }
}
