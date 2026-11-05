//! 本地录像回放（M6 媒体库）：把一条已完成的录像转成 HLS，复用观看端的播放器。
//!
//! 与观看端的区别只有输入源：这里是磁盘文件（-re 按帧率读），观看端是平台直播流。
//! 同一时刻只允许一个本地回放：媒体库是「点一条看一条」，没必要占两份缓存。

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

struct Active {
    key: String,
    dir: PathBuf,
    child: Child,
}

pub struct LocalPlayer {
    root: PathBuf,
    server: Arc<StreamServer>,
    current: Mutex<Option<Active>>,
    gate: tokio::sync::Mutex<()>,
}

impl LocalPlayer {
    pub fn new(root: PathBuf, server: Arc<StreamServer>) -> Arc<Self> {
        Arc::new(Self {
            root,
            server,
            current: Mutex::new(None),
            gate: tokio::sync::Mutex::new(()),
        })
    }

    /// 打开一条录像；同一时刻只保留一个回放，旧的会被停掉。
    pub async fn open(&self, db: &Db, session_id: &str) -> Result<Playback, AppError> {
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

        self.stop_inner().await;

        let dir = self.root.join(format!("{}-{}", session_id, now_ms()));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| AppError::from_io(err, &dir))?;

        let mut command = Command::new(ffmpeg::binary());
        command
            .arg("-hide_banner")
            .arg("-nostdin")
            .arg("-loglevel")
            .arg("warning")
            .arg("-y")
            .arg("-i")
            .arg(&path)
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
            // 本地回放是点播：整套分片保留，播放器可以任意拖动进度条。
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

        let mut child = command
            .spawn()
            .map_err(|err| AppError::Ffmpeg(format!("启动本地回放失败：{err}")))?;

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
            *guard = Some(Active {
                key: key.clone(),
                dir,
                child,
            });
        }

        let title = db
            .get_room(&session.room_id)?
            .map(|room| room.title)
            .unwrap_or_default();

        Ok(Playback {
            url: self.server.playlist_url(&key),
            session_id: session_id.to_string(),
            title,
            duration_ms: session.duration_ms,
        })
    }

    pub async fn stop(&self) {
        let _gate = self.gate.lock().await;
        self.stop_inner().await;
    }

    async fn stop_inner(&self) {
        let active = {
            let mut guard = self.current.lock().expect("本地回放状态被污染");
            guard.take()
        };
        let Some(mut active) = active else {
            return;
        };
        send_graceful(&mut active.child);
        let _ = wait_exit(&mut active.child, STOP_TIMEOUT).await;
        self.server.unregister(&active.key);
        let _ = tokio::fs::remove_dir_all(&active.dir).await;
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
