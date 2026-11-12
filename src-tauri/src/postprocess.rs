//! 本地后处理（D5）：转 mp4 / 弹幕转 ASS / 多段合并。
//!
//! 全部在本地 ffmpeg 上跑，不上传任何数据；产物默认落在源文件旁边，
//! 命名沿用录制文件名，方便在访达里直接对照。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hivecast_ipc::{PostProgress, PostResult};
use tauri::{AppHandle, Emitter};
use tokio::process::Command;

use crate::error::AppError;
use crate::ffmpeg;

/// 后处理进度事件：前端据此在按钮上显示百分比。
pub const EVENT_POST_PROGRESS: &str = "post://progress";

/// 单个产物最长容忍 10 分钟：4 小时录像的 remux 也只是 IO 时间。
const TASK_TIMEOUT: Duration = Duration::from_secs(600);

/// 估算弹幕滚动速度（像素/秒），1080p 下观感接近 B 站。
const SCROLL_SPEED: f64 = 190.0;
/// ASS 画布宽度：与常见 16:9 一致，播放器会按视频等比缩放。
const PLAY_RES_X: i64 = 1280;
const PLAY_RES_Y: i64 = 720;
const FONT_SIZE: u32 = 42;
const LANE_HEIGHT: i64 = 54;
const LANE_TOP: i64 = 20;
/// 公告（礼物/上舰）停留 5 秒，同一泳道至少隔 0.8 秒。
const ANNOUNCE_HOLD_MS: i64 = 5000;
const ANNOUNCE_GAP_MS: i64 = 800;

fn sibling(src: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let name = if suffix.is_empty() {
        format!("{stem}.{ext}")
    } else {
        format!("{stem}{suffix}.{ext}")
    };
    src.with_file_name(name)
}

/// ffmpeg 的 out_time 是微秒；进度封顶 99，100 由「任务成功返回」表达。
fn percent_for(micros: i64, total_ms: i64) -> i64 {
    (micros / 1000 * 100 / total_ms.max(1)).clamp(0, 99)
}

fn describe_failure(label: &str, stderr: &str) -> String {
    let tail: String = stderr
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ");
    if tail.is_empty() {
        format!("{label} 失败（ffmpeg 无输出）")
    } else {
        format!("{label} 失败：{tail}")
    }
}

fn size_of(path: &Path) -> i64 {
    std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or(0)
}

async fn run_ffmpeg(args: Vec<OsString>, label: &str) -> Result<(), AppError> {
    let mut command = Command::new(ffmpeg::binary());
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(TASK_TIMEOUT, command.output())
        .await
        .map_err(|_| AppError::Ffmpeg(format!("{label} 超时")))?
        .map_err(AppError::Io)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = describe_failure(label, &stderr);
    tracing::warn!(
        target: "hivecast_lib::postprocess",
        label,
        status = %output.status,
        tail = %stderr.trim().chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>(),
        "{message}"
    );
    Err(AppError::Ffmpeg(message))
}

/// 进度上下文：有总时长才发进度（否则 ffmpeg 的 out_time 没有参照）。
pub struct Progress<'a> {
    pub app: &'a AppHandle,
    pub task: &'a str,
    pub total_ms: i64,
}

async fn run_ffmpeg_progress(
    args: Vec<OsString>,
    label: &str,
    progress: &Progress<'_>,
) -> Result<(), AppError> {
    let mut command = Command::new(ffmpeg::binary());
    command
        .args(&args)
        .args(["-progress", "pipe:1", "-stats_period", "0.2", "-nostats"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(AppError::Io)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        if let Some(mut stream) = stderr {
            let _ = stream.read_to_end(&mut buf).await;
        }
        buf
    });

    let mut last = 0i64;
    if let Some(stream) = stdout {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let value = match line.strip_prefix("out_time_us=") {
                Some(value) => value,
                None => continue,
            };
            let Ok(micros) = value.trim().parse::<i64>() else {
                continue;
            };
            let percent = percent_for(micros, progress.total_ms);
            if percent > last {
                last = percent;
                let _ = progress.app.emit(
                    EVENT_POST_PROGRESS,
                    PostProgress {
                        task: progress.task.to_string(),
                        session_id: progress.task.to_string(),
                        percent,
                        stage: "converting".into(),
                        state: "running".into(),
                        message: None,
                    },
                );
            }
        }
    }

    let status = tokio::time::timeout(TASK_TIMEOUT, child.wait())
        .await
        .map_err(|_| AppError::Ffmpeg(format!("{label} 超时")))?
        .map_err(AppError::Io)?;
    let stderr = stderr_task.await.unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&stderr);
    Err(AppError::Ffmpeg(describe_failure(label, &text)))
}

fn ensure_file(src: &Path) -> Result<(), AppError> {
    if !src.is_file() {
        return Err(AppError::Path(format!("源文件不存在：{}", src.display())));
    }
    Ok(())
}

// 多段录制合并成一个 mp4：用 concat 解复用器直接拷流，不做二次编码。
pub async fn merge(
    sources: &[PathBuf],
    progress: Option<&Progress<'_>>,
) -> Result<PostResult, AppError> {
    if sources.len() < 2 {
        return Err(AppError::User("至少需要两段录像才能合并".into()));
    }
    for src in sources {
        ensure_file(src)?;
    }
    let first = &sources[0];
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let out = first.with_file_name(format!("合并-{stamp}.mp4"));
    let list_path = out.with_extension("concat.txt");
    let mut list = String::new();
    for src in sources {
        let escaped = src.to_string_lossy().replace('\'', "'\\''");
        list.push_str(&format!("file '{escaped}'\n"));
    }
    std::fs::write(&list_path, list)?;

    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_path.as_os_str().to_os_string(),
        "-c".into(),
        "copy".into(),
        "-movflags".into(),
        "+faststart".into(),
        out.as_os_str().to_os_string(),
    ];
    let started = std::time::Instant::now();
    let result = match progress {
        Some(progress) if progress.total_ms > 0 => {
            run_ffmpeg_progress(args, "合并", progress).await
        }
        _ => run_ffmpeg(args, "合并").await,
    };
    let _ = std::fs::remove_file(&list_path);
    result?;
    let size = size_of(&out);
    tracing::info!(
        target: "hivecast_lib::postprocess",
        task = "merge",
        sources = sources.len(),
        out = %out.display(),
        bytes = size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "多段合并完成"
    );
    Ok(PostResult {
        kind: "merge".into(),
        path: out.to_string_lossy().to_string(),
        size,
        message: Some(format!("共 {} 段", sources.len())),
    })
}

/// 按时间区间从整场 JSONL 切出单个片段，并把 t 归零到片段起点。
///
/// 分割录制只维护一条 RecordWriter；收尾时用它给每个片段生成独立 JSONL，
/// 这样每段 ASS 的时间轴都与自己的视频对齐，媒体库也能逐段显示弹幕数量。
/// 区间是 [start_ms, end_ms)，end 为 None 表示到文件末尾。
pub fn slice_danmaku(
    jsonl: &Path,
    out: &Path,
    start_ms: i64,
    end_ms: Option<i64>,
) -> std::io::Result<u64> {
    use std::io::Write as _;

    let raw = std::fs::read_to_string(jsonl)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut count = 0u64;
    for line in raw.lines() {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(at) = value.get("t").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        if at < start_ms || end_ms.is_some_and(|end| at >= end) {
            continue;
        }
        if let Some(object) = value.as_object_mut() {
            object.insert("t".into(), serde_json::Value::from((at - start_ms).max(0)));
        }
        writeln!(writer, "{value}")?;
        count += 1;
    }
    writer.flush()?;
    if count == 0 {
        let _ = std::fs::remove_file(out);
    }
    Ok(count)
}

/// 弹幕 JSONL → ASS 字幕：聊天从右向左滚动，礼物/上舰固定在图上方居中。
///
/// 直播里的礼物与上舰本来就是「悬停公告」，跟着聊天一起滚会看不清，
/// 所以给它们单独的 `\an8\pos` 定位和 5 秒停留时间。
pub fn danmaku_to_ass(jsonl: &Path) -> Result<PostResult, AppError> {
    ensure_file(jsonl)?;
    let raw = std::fs::read_to_string(jsonl)?;
    // 第四位表示「固定不滚动」：chat 之外（gift / super_chat）都按公告处理。
    let mut lines: Vec<(i64, String, u32, bool)> = Vec::new();
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text = value["c"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let at = value["t"].as_i64().unwrap_or(0);
        let color = value["col"].as_u64().unwrap_or(0) as u32;
        let kind = value["k"].as_str().unwrap_or("chat");
        lines.push((at, text, color, kind != "chat"));
    }
    if lines.is_empty() {
        return Err(AppError::User("这场没有弹幕".into()));
    }
    lines.sort_by_key(|(at, _, _, _)| *at);

    let mut ass = String::new();
    ass.push_str("[Script Info]\n");
    ass.push_str("ScriptType: v4.00+\n");
    ass.push_str(&format!("PlayResX: {PLAY_RES_X}\n"));
    ass.push_str(&format!("PlayResY: {PLAY_RES_Y}\n"));
    ass.push_str("WrapStyle: 2\n");
    ass.push_str("ScaledBorderAndShadow: yes\n\n");
    ass.push_str("[V4+ Styles]\n");
    ass.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    ass.push_str(&format!(
        "Style: Danmaku,PingFang SC,{FONT_SIZE},&H00FFFFFF,&H00FFFFFF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,1,7,0,0,0,1\n\n"
    ));
    ass.push_str("[Events]\n");
    ass.push_str(
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );

    let line_count = lines.len();
    let lane_count = ((PLAY_RES_Y - LANE_TOP) / LANE_HEIGHT).max(1);
    let mut lane_free = vec![0i64; lane_count as usize];
    // 公告只占顶部三条泳道，避免盖住画面主体。
    let announce_lanes = 3usize.min(lane_count as usize);
    let mut announce_free = vec![0i64; announce_lanes];
    for (at, text, color, fixed) in lines {
        let body = escape_text(&text);
        let colour = ass_colour(color);
        if fixed {
            let lane = pick_lane(&announce_free, at);
            let y = LANE_TOP + lane as i64 * LANE_HEIGHT;
            announce_free[lane] = at + ANNOUNCE_GAP_MS;
            ass.push_str(&format!(
                "Dialogue: 0,{},{},Danmaku,,0,0,0,,{{\\an8\\pos({},{y})\\c{colour}}}{body}\n",
                ass_time(at),
                ass_time(at + ANNOUNCE_HOLD_MS),
                PLAY_RES_X / 2
            ));
            continue;
        }
        let width = estimate_width(&text);
        let lane = pick_lane(&lane_free, at);
        let y = LANE_TOP + lane as i64 * LANE_HEIGHT;
        let duration = ((PLAY_RES_X as f64 + width as f64) / SCROLL_SPEED * 1000.0) as i64;
        lane_free[lane] = at + duration.min(4000);
        ass.push_str(&format!(
            "Dialogue: 0,{},{},Danmaku,,0,0,0,,{{\\move({PLAY_RES_X},{y},-{width},{y})\\c{colour}}}{body}\n",
            ass_time(at),
            ass_time(at + duration)
        ));
    }

    let out = sibling(jsonl, "", "ass");
    std::fs::write(&out, ass)?;
    tracing::info!(
        target: "hivecast_lib::postprocess",
        task = "ass",
        src = %jsonl.display(),
        out = %out.display(),
        lines = line_count,
        bytes = size_of(&out),
        "弹幕字幕导出完成"
    );
    Ok(PostResult {
        kind: "ass".into(),
        path: out.to_string_lossy().to_string(),
        size: size_of(&out),
        message: None,
    })
}

fn pick_lane(free: &[i64], at: i64) -> usize {
    if let Some(index) = free.iter().position(|until| *until <= at) {
        return index;
    }
    free.iter()
        .enumerate()
        .min_by_key(|(_, until)| **until)
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// 粗略估宽：中日韩字符按字宽算，ASCII 折半。
fn estimate_width(text: &str) -> u32 {
    let mut width = 0.0f64;
    for ch in text.chars() {
        width += if (ch as u32) < 0x2e80 {
            FONT_SIZE as f64 * 0.55
        } else {
            FONT_SIZE as f64
        };
    }
    width.max(40.0) as u32
}

fn ass_time(ms: i64) -> String {
    let ms = ms.max(0);
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let centis = (ms % 1000) / 10;
    format!("{hours}:{minutes:02}:{seconds:02}.{centis:02}")
}

/// ASS 用 BGR 顺序；0 表示默认白色（B 站把白色存成 0xFFFFFF，也有用 0 的）。
fn ass_colour(value: u32) -> String {
    let rgb = if value == 0 {
        0xFFFFFF
    } else {
        value & 0xFFFFFF
    };
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    format!("&H00{b:02X}{g:02X}{r:02X}&")
}

fn escape_text(text: &str) -> String {
    text.replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_time_pads_centiseconds() {
        assert_eq!(ass_time(0), "0:00:00.00");
        assert_eq!(ass_time(3_723_456), "1:02:03.45");
    }

    #[test]
    fn percent_clamps_between_zero_and_ninety_nine() {
        assert_eq!(percent_for(0, 60_000), 0);
        assert_eq!(percent_for(30_000_000, 60_000), 50);
        assert_eq!(percent_for(90_000_000, 60_000), 99);
        assert_eq!(percent_for(-1_000, 60_000), 0);
    }

    #[test]
    fn ass_colour_swaps_to_bgr() {
        assert_eq!(ass_colour(0), "&H00FFFFFF&");
        assert_eq!(ass_colour(0xFF0000), "&H000000FF&");
    }

    #[test]
    fn slices_jsonl_and_rebases_time() {
        let dir = std::env::temp_dir().join(format!("hivecast-slice-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("full.jsonl");
        std::fs::write(
            &src,
            concat!(
                "{\"t\":0,\"k\":\"chat\",\"c\":\"a\"}\n",
                "{\"t\":1500,\"k\":\"chat\",\"c\":\"b\"}\n",
                "{\"t\":3000,\"k\":\"gift\",\"c\":\"c\"}\n"
            ),
        )
        .unwrap();
        let out = dir.join("clip1.jsonl");
        let count = slice_danmaku(&src, &out, 1000, Some(2500)).unwrap();
        assert_eq!(count, 1);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("\"t\":500"));
        assert!(text.contains("\"c\":\"b\""));
        assert_eq!(slice_danmaku(&src, &out, 9000, None).unwrap(), 0);
        assert!(!out.exists(), "空片段不留 0 字节 JSONL");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lane_prefers_free_row_then_earliest() {
        let mut free = vec![0, 5_000, 0];
        assert_eq!(pick_lane(&free, 100), 0);
        free[0] = 9_000;
        free[2] = 9_000;
        assert_eq!(pick_lane(&free, 100), 1);
    }

    async fn make_clip(path: &Path, seconds: u32) {
        let mut command = Command::new(ffmpeg::binary());
        command
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180:rate=15",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                &seconds.to_string(),
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
            .arg(path);
        let status = command.status().await.unwrap();
        assert!(status.success(), "生成测试片失败");
    }

    /// 真跑 ffmpeg 的端到端冒烟：合并要产出可读文件。
    /// 依赖本机 ffmpeg，默认跳过；手动跑时加 --ignored。
    #[tokio::test]
    #[ignore = "需要本机 ffmpeg"]
    async fn ffmpeg_pipeline_on_synthetic_clips() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("a.flv");
        let second = dir.join("b.flv");
        make_clip(&first, 3).await;
        make_clip(&second, 3).await;

        let merged = merge(&[first.clone(), second.clone()], None).await.unwrap();
        let source_total = size_of(&first) + size_of(&second);
        assert_eq!(merged.kind, "merge");
        assert!(
            merged.size as f64 > source_total as f64 * 0.6,
            "合并结果偏小：{} vs {}",
            merged.size,
            source_total
        );
        assert!(Path::new(&merged.path).is_file());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_ass_for_sample_danmaku() {
        let dir = std::env::temp_dir().join(format!("hivecast-ass-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("demo.jsonl");
        std::fs::write(
            &file,
            r#"{"t":1200,"k":"chat","u":"小明","c":"哈哈哈","col":16711680}
{"t":9000,"k":"chat","u":"小红","c":"{\\move(0,0)} 注入","col":0}
{"t":2000,"k":"gift","u":"小刚","c":"赠送 辣条 ×3","col":16776960}
{"t":3000,"k":"super_chat","u":"小蓝","c":"[30 元] 加油","col":123456}
{"t":4000,"k":"guard","u":"小绿","c":"开通了 舰长 × 1","col":123456}
"#,
        )
        .unwrap();
        let result = danmaku_to_ass(&file).unwrap();
        let text = std::fs::read_to_string(&result.path).unwrap();
        assert!(text.contains("Dialogue: 0,0:00:01.20"));
        assert!(text.contains("&H000000FF&"));
        assert!(text.contains("｛"));
        assert!(!text.contains("{\\move(0,0)}"));
        // 礼物走固定公告：居中、an8 顶部对齐，不带 move。
        assert!(text.contains("\\an8\\pos(640,"));
        assert!(text.contains("赠送 辣条 ×3"));
        assert!(text.contains("[30 元] 加油"), "SC 不能丢");
        assert!(text.contains("开通了 舰长 × 1"), "舰长不能丢");
        std::fs::remove_dir_all(&dir).ok();
    }
}
