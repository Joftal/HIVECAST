//! 媒体库缩略图：把一段录像按时间平均切 9 帧，拼成一张 3x3 联系表。
//!
//! 为什么是九宫格而不是单帧：直播录像前几秒经常是黑屏或过场，
//! 单帧抽到黑屏就整张卡都认不出来；九帧能同时看到场景、弹幕、字幕，
//! 一眼就知道是哪一场。
//!
//! 为什么用 9 次 -ss 精确跳转，而不是 fps=9/时长 一次解码：
//! fps 滤镜按帧序号取帧，输出帧落在哪一帧取决于起始时间与取整方式，
//! 实测 150 秒的片子最后一格落在 141 秒，尾部 9 秒完全没覆盖。
//! 改成按 t = 时长 x (k + 0.5) / 9 逐点跳转，九格严格等距，
//! 且每次只解码目标帧附近，两小时的录像也不用整段解码。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;

use crate::error::AppError;
use crate::ffmpeg;
use crate::stream::StreamServer;

/// 缩略图生成完成后广播：前端按 sessionId 替换卡片封面。
pub const EVENT_THUMB: &str = "media://thumb";
/// 单格宽度；3 格加两条 4px 缝 = 968 宽，卡片里够清晰又不至于太大。
pub const CELL_WIDTH: u32 = 320;
/// 单格高度（统一按 16:9 裁进格子，竖屏素材左右补边）。
pub const CELL_HEIGHT: u32 = 180;
/// 格子之间的缝。
pub const PAD: u32 = 4;
/// 同时最多跑两个 ffmpeg，避免进媒体库时 CPU 飙满。
const CONCURRENCY: usize = 2;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbReady {
    pub session_id: String,
    /// 回环服务上的图片地址
    pub url: String,
}

pub struct Thumbs {
    dir: PathBuf,
    slots: Arc<Semaphore>,
    /// 缩略图也走回环 HTTP：前端 img 直接吃 http://127.0.0.1，
    /// 不用为 asset 协议单独开权限，CSP 里现成的 img-src 就够。
    server: Arc<StreamServer>,
    jobs: Arc<crate::jobs::Jobs>,
}

impl Thumbs {
    pub fn new(dir: PathBuf, server: Arc<StreamServer>, jobs: Arc<crate::jobs::Jobs>) -> Self {
        // 把缩略图目录登记成回环服务的一个只读会话。
        server.register("thumbs", dir.clone());
        Self {
            dir,
            slots: Arc::new(Semaphore::new(CONCURRENCY)),
            server,
            jobs,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 已经生成过的直接返回可访问地址，省一次 ffmpeg。
    pub fn url_for(&self, session_id: &str) -> Option<String> {
        let path = self.dir.join(format!("{session_id}.jpg"));
        path.is_file().then(|| {
            self.server
                .asset_url("thumbs", &format!("{session_id}.jpg"))
        })
    }

    /// 删掉一条录像时顺手清掉它的缩略图，避免缓存越积越多。
    pub fn remove(&self, session_id: &str) {
        let path = self.dir.join(format!("{session_id}.jpg"));
        if path.is_file() {
            match std::fs::remove_file(&path) {
                Ok(()) => tracing::debug!(id = %session_id, "已删除录像缩略图"),
                Err(err) => tracing::warn!(id = %session_id, error = %err, "删除录像缩略图失败"),
            }
        }
    }

    /// 给这一页里缺缩略图的录像排队生成；调用方不等结果，完成后走事件。
    pub fn ensure(self: &Arc<Self>, app: AppHandle, items: Vec<(String, String, i64)>) {
        self.ensure_inner(app, items, false);
    }

    /// 录制收尾自带的封面：同时把 thumbnail 阶段写进任务时间线。
    pub fn ensure_tracked(self: &Arc<Self>, app: AppHandle, items: Vec<(String, String, i64)>) {
        self.ensure_inner(app, items, true);
    }

    fn ensure_inner(
        self: &Arc<Self>,
        app: AppHandle,
        items: Vec<(String, String, i64)>,
        tracked: bool,
    ) {
        for (session_id, source, duration_ms) in items {
            if self.url_for(&session_id).is_some() {
                continue;
            }
            tracing::debug!(id = %session_id, duration_ms, "缩略图已排队");
            if tracked {
                self.jobs.emit_session(
                    &session_id,
                    "thumbnail",
                    "thumbnail",
                    "running",
                    0,
                    Some("生成封面缩略图"),
                );
            }
            let this = self.clone();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let Ok(_permit) = this.slots.acquire().await else {
                    tracing::warn!(id = %session_id, "缩略图队列已关闭，跳过生成");
                    return;
                };
                // 排队期间可能已经被别的请求生成好了。
                if this.url_for(&session_id).is_some() {
                    if tracked {
                        this.jobs.emit_session(
                            &session_id,
                            "thumbnail",
                            "thumbnail",
                            "done",
                            100,
                            None,
                        );
                    }
                    return;
                }
                match this
                    .generate(&session_id, Path::new(&source), duration_ms)
                    .await
                {
                    Ok(_) => {
                        if tracked {
                            this.jobs.emit_session(
                                &session_id,
                                "thumbnail",
                                "thumbnail",
                                "done",
                                100,
                                None,
                            );
                        }
                        if let Some(url) = this.url_for(&session_id) {
                            let event = ThumbReady {
                                session_id: session_id.clone(),
                                url,
                            };
                            if let Err(err) = app.emit(EVENT_THUMB, event) {
                                tracing::warn!(id = %session_id, error = %err, "缩略图完成事件发送失败");
                            } else {
                                tracing::debug!(id = %session_id, "缩略图已生成");
                            }
                        }
                    }
                    Err(err) => {
                        if tracked {
                            this.jobs.emit_session(
                                &session_id,
                                "thumbnail",
                                "thumbnail",
                                "failed",
                                0,
                                Some(&err.to_string()),
                            );
                        }
                        tracing::warn!(id = %session_id, error = %err, "缩略图生成失败");
                    }
                }
            });
        }
    }

    async fn generate(
        &self,
        session_id: &str,
        source: &Path,
        duration_ms: i64,
    ) -> Result<PathBuf, AppError> {
        if !source.is_file() {
            return Err(AppError::Path(format!(
                "源文件不存在：{}",
                source.display()
            )));
        }
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|err| AppError::from_io(err, &self.dir))?;
        let out = self.dir.join(format!("{session_id}.jpg"));
        let mut command = tokio::process::Command::new(ffmpeg::binary());
        command
            .arg("-hide_banner")
            .arg("-nostdin")
            .arg("-loglevel")
            .arg("error")
            .arg("-y");
        if duration_ms > 0 {
            // 每个采样点各开一个输入：-ss 放在 -i 前面走快速跳转，只解到目标帧。
            for point in seek_points(duration_ms) {
                command
                    .arg("-ss")
                    .arg(format!("{:.3}", point as f64 / 1000.0))
                    .arg("-i")
                    .arg(source);
            }
            command
                .arg("-filter_complex")
                .arg(grid_filter())
                .arg("-frames:v")
                .arg("1")
                .arg("-q:v")
                .arg("4")
                .arg(&out);
        } else {
            // 时长未知（正在录、或者容器没写时长）时退回一次解码取 9 帧。
            command
                .arg("-i")
                .arg(source)
                .arg("-vf")
                .arg(tile_filter(0))
                .arg("-frames:v")
                .arg("1")
                .arg("-q:v")
                .arg("4")
                .arg(&out);
        }
        let output = command
            .output()
            .await
            .map_err(|err| AppError::Internal(format!("无法启动 ffmpeg：{err}")))?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::Ffmpeg(format!("缩略图生成失败：{}", text.trim())));
        }
        if !out.is_file() {
            return Err(AppError::Ffmpeg("缩略图生成失败：没有输出文件".into()));
        }
        Ok(out)
    }
}

/// 九个采样点：每格取该段的中点，t = 时长 x (k + 0.5) / 9。
///
/// 取中点而不是等分端点：第一格不会是 0 秒的黑屏，最后一格也不会贴着
/// 文件尾（录像结尾常常是断开连接的黑帧）。
pub fn seek_points(duration_ms: i64) -> Vec<i64> {
    let last = (duration_ms - 1).max(0);
    (0..9)
        .map(|k| {
            let point = (duration_ms as f64) * (k as f64 + 0.5) / 9.0;
            (point.round() as i64).clamp(0, last)
        })
        .collect()
}

/// 九格在 968x548 画布上的位置：每格 320x180，缝 4px。
pub fn xstack_layout() -> String {
    let mut parts = Vec::with_capacity(9);
    for row in 0..3u32 {
        for col in 0..3u32 {
            let x = col * (CELL_WIDTH + PAD);
            let y = row * (CELL_HEIGHT + PAD);
            parts.push(format!("{x}_{y}"));
        }
    }
    parts.join("|")
}

/// 每格统一缩到 320x180 并补黑边，保证九宫格行列严格对齐。
fn cell_filter(index: usize) -> String {
    format!(
        "[{index}:v]scale={CELL_WIDTH}:{CELL_HEIGHT}:force_original_aspect_ratio=decrease,\
pad={CELL_WIDTH}:{CELL_HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=0x101010,\
trim=end_frame=1,setpts=PTS-STARTPTS[v{index}]"
    )
}

/// 完整滤镜图：9 路输入各自取 1 帧，再按固定坐标拼成 3x3。
pub fn grid_filter() -> String {
    let mut graph = String::new();
    for index in 0..9 {
        graph.push_str(&cell_filter(index));
        graph.push(';');
    }
    for index in 0..9 {
        graph.push_str(&format!("[v{index}]"));
    }
    graph.push_str(&format!(
        "xstack=inputs=9:layout={}:fill=0x101010",
        xstack_layout()
    ));
    graph
}

/// 时长未知时的兜底滤镜：按固定速率取帧再拼九宫格。
pub fn tile_filter(duration_ms: i64) -> String {
    let seconds = (duration_ms.max(0) as f64) / 1000.0;
    let fps = if seconds > 0.0 {
        (9.0 / seconds).clamp(0.000_1, 1.0)
    } else {
        0.1
    };
    format!("fps={fps:.6},scale={CELL_WIDTH}:-2,tile=3x3:padding={PAD}:margin=0:color=0x101010")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_seeks_sit_in_the_middle_of_equal_slices() {
        assert_eq!(
            seek_points(150_000),
            vec![8_333, 25_000, 41_667, 58_333, 75_000, 91_667, 108_333, 125_000, 141_667]
        );
    }

    #[test]
    fn seeks_never_leave_the_file() {
        for duration in [1_i64, 900, 5_000, 600_000, 10_800_000] {
            let points = seek_points(duration);
            assert_eq!(points.len(), 9);
            assert!(points[0] >= 0, "首点越界：{points:?}");
            assert!(
                points[8] <= (duration - 1).max(0),
                "末点越界：{points:?} / {duration}"
            );
        }
    }

    #[test]
    fn grid_places_nine_cells_with_four_pixel_gaps() {
        let filter = grid_filter();
        assert!(filter.contains("[8:v]"), "{filter}");
        assert!(filter.contains("xstack=inputs=9"), "{filter}");
        assert!(
            filter.contains("0_0|324_0|648_0|0_184|324_184|648_184|0_368|324_368|648_368"),
            "{filter}"
        );
    }

    #[test]
    fn unknown_duration_falls_back_to_a_fixed_rate() {
        let filter = tile_filter(0);
        assert!(filter.starts_with("fps=0.100000,"), "{filter}");
        assert!(filter.contains("tile=3x3"));
    }
}
