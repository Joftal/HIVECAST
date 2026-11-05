use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::error::AppError;

/// M0 演示流：用本机 ffmpeg 造一段 720p 的 HLS（fMP4），
/// 走「ffmpeg 转封装 → 本地 HTTP → hls.js」这条真实管道，
/// 避免用假数据把整条链路的问题藏起来。
pub async fn ensure_demo(cache_root: &Path) -> Result<PathBuf, AppError> {
    let dir = cache_root.join("demo-hls");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| AppError::from_io(err, &dir))?;

    let playlist = dir.join("index.m3u8");
    if tokio::fs::metadata(&playlist)
        .await
        .map(|m| m.len() > 0)
        .unwrap_or(false)
    {
        return Ok(dir);
    }

    if crate::ffmpeg::resolve().is_none() {
        return Err(AppError::FfmpegMissing);
    }

    let child = tokio::process::Command::new(crate::ffmpeg::binary())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "24",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "60",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_list_size",
            "0",
            "-hls_playlist_type",
            "event",
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            "init.mp4",
        ])
        .arg("-hls_segment_filename")
        .arg(dir.join("seg_%05d.m4s"))
        .arg(&playlist)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| AppError::from_io(err, "ffmpeg"))?;

    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| AppError::Ffmpeg("timeout after 120s".into()))?
        .map_err(|err| AppError::from_io(err, "ffmpeg"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(AppError::Ffmpeg(tail));
    }

    Ok(dir)
}
