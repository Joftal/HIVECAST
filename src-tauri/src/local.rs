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
    ) -> Result<Playback, AppError> {
        let _gate = self.gate.lock().await;

        let session = db
            .get_session(session_id)?
            .ok_or_else(|| AppError::User("录像不存在".into()))?;
        if session.status == "recording" {
            return Err(AppError::User("这段还在录制，先停下再回放".into()));
        }
        let path = session
            .file_path
            .clone()
            .filter(|p| Path::new(p).is_file())
            .ok_or_else(|| AppError::User("录像文件已不存在".into()))?;

        let duration_ms = session.duration_ms.max(0);
        let wanted = position_ms.unwrap_or(0).clamp(0, duration_ms);
        let title = db
            .get_room(&session.room_id)?
            .map(|room| room.title)
            .unwrap_or_default();

        // 已经在放同一条、且目标位置在窗口内：复用现成的 HLS，别重复转封装。
        let stale = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            match guard.get(session_id) {
                Some(active) if active.contains(wanted) => {
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
                Some(_) => guard.remove(session_id),
                None => None,
            }
        };
        if let Some(active) = stale {
            teardown(self.server.as_ref(), active).await;
        }

        {
            let guard = self.current.lock().expect("本地回放状态被污染");
            if guard.len() >= MAX_ACTIVE {
                return Err(AppError::User(format!(
                    "最多同时回放 {MAX_ACTIVE} 条，先停掉一条再开"
                )));
            }
        }

        let (start_ms, window_ms) = window_for(wanted, duration_ms);
        let dir = self.root.join(format!("{}-{}", session_id, now_ms()));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| AppError::from_io(err, &dir))?;

        let mut child = spawn_window(Path::new(&path), &dir, start_ms, window_ms)?;

        // 等第一个分片落盘再交给播放器，否则 hls.js 会拿到 404 的 m3u8。
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if dir.join("index.m3u8").is_file() {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(AppError::Ffmpeg(format!(
                    "本地回放启动失败（ffmpeg 退出码 {:?}）",
                    status.code()
                )));
            }
            if Instant::now() >= deadline {
                send_graceful(&mut child);
                let _ = wait_exit(&mut child, STOP_TIMEOUT).await;
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(AppError::User("本地回放启动超时".into()));
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }

        let key = format!("play-{}", now_ms());
        self.server.register(&key, dir.clone());
        {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            guard.insert(
                session_id.to_string(),
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
        let active = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            guard.remove(session_id)
        };
        if let Some(active) = active {
            teardown(self.server.as_ref(), active).await;
        }
    }

    /// 退出前停掉全部回放。
    pub async fn stop_all(&self) {
        let _gate = self.gate.lock().await;
        let all = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            std::mem::take(&mut *guard)
        };
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
    let _ = wait_exit(&mut active.child, STOP_TIMEOUT).await;
    server.unregister(&active.key);
    let _ = tokio::fs::remove_dir_all(&active.dir).await;
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
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
