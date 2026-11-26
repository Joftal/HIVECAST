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
#[cfg(test)]
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
    let mut command = crate::process::background_async(ffmpeg::binary());
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(AppError::Io)?;
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        if let Some(mut stream) = stderr {
            let _ = stream.read_to_end(&mut buf).await;
        }
        buf
    });
    let status = match tokio::time::timeout(TASK_TIMEOUT, child.wait()).await {
        Ok(status) => status.map_err(AppError::Io)?,
        Err(_) => {
            // 超时必须真的杀掉子进程：只丢掉 future 会让 ffmpeg 继续写文件。
            let _ = child.start_kill();
            let _ = child.wait().await;
            stderr_task.abort();
            return Err(AppError::Ffmpeg(format!("{label} 超时")));
        }
    };
    let stderr = stderr_task.await.unwrap_or_default();
    if status.success() && stderr.is_empty() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&stderr);
    let message = describe_failure(label, &text);
    tracing::warn!(
        target: "hivecast_lib::postprocess",
        label,
        status = %status,
        tail = %text.trim().chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>(),
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
    let mut command = crate::process::background_async(ffmpeg::binary());
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

    // 进度读取和等待必须并发：先读到 stdout EOF 再 wait，会把超时起点拖到
    // ffmpeg 自己退出之后，等于没有超时（文档 196-13）。
    let app = progress.app.clone();
    let task = progress.task.to_string();
    let total_ms = progress.total_ms;
    let progress_task = tokio::spawn(async move {
        let Some(stream) = stdout else { return };
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stream).lines();
        let mut last = 0i64;
        while let Ok(Some(line)) = lines.next_line().await {
            let value = match line.strip_prefix("out_time_us=") {
                Some(value) => value,
                None => continue,
            };
            let Ok(micros) = value.trim().parse::<i64>() else {
                continue;
            };
            let percent = percent_for(micros, total_ms);
            if percent > last {
                last = percent;
                let _ = app.emit(
                    EVENT_POST_PROGRESS,
                    PostProgress {
                        task: task.clone(),
                        session_id: task.clone(),
                        percent,
                        stage: "converting".into(),
                        state: "running".into(),
                        message: None,
                    },
                );
            }
        }
    });

    let status = match tokio::time::timeout(TASK_TIMEOUT, child.wait()).await {
        Ok(status) => status.map_err(AppError::Io)?,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            progress_task.abort();
            stderr_task.abort();
            return Err(AppError::Ffmpeg(format!("{label} 超时")));
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), progress_task).await;
    let stderr = stderr_task.await.unwrap_or_default();
    if status.success() && stderr.is_empty() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&stderr);
    Err(AppError::Ffmpeg(describe_failure(label, &text)))
}

fn reserve_merge_output(first: &Path, stamp: &str) -> Result<PathBuf, AppError> {
    for index in 0..1000u32 {
        let name = if index == 0 {
            format!("合并-{stamp}.mp4")
        } else {
            format!("合并-{stamp}-{index}.mp4")
        };
        let candidate = first.with_file_name(name);
        // These names also belong to the finished video. Skip stems already in use.
        let mut conflict = false;
        for ext in ["jsonl", "ass", "concat.txt"] {
            match candidate.with_extension(ext).symlink_metadata() {
                Ok(_) => {
                    conflict = true;
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(AppError::from_io(err, &candidate)),
            }
        }
        if conflict {
            continue;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(AppError::from_io(err, &candidate)),
        }
    }
    Err(AppError::User("同一秒合并次数过多，请稍后重试".into()))
}

/// 合并失败时清掉本次占位的成品和清单，别把 0 字节 MP4 留在录制目录。
struct MergeArtifacts {
    out: PathBuf,
    list: PathBuf,
    keep: bool,
}

impl Drop for MergeArtifacts {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = std::fs::remove_file(&self.out);
        let _ = std::fs::remove_file(&self.list);
    }
}

fn ensure_file(src: &Path) -> Result<(), AppError> {
    if !src.is_file() {
        return Err(AppError::Path(format!("源文件不存在：{}", src.display())));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeDurationExpectation {
    pub total_ms: u64,
    pub min_clip_ms: Option<u64>,
}

/// 拷流拼接会在各段首尾音视频帧边界产生约 10–30 ms 舍入误差。
/// 容差随段数温和增长（封顶 2.5 秒），同时不超过最短单段时长的一半（下限 250 ms），
/// 既避免几十段真实直播因累计舍入被误拒，又能拦截任意非极短片段的静默漏片。
fn merge_duration_tolerance_ms(clip_count: usize, min_clip_ms: Option<u64>) -> u64 {
    let scaled = 250u64
        .saturating_add(clip_count.saturating_sub(2) as u64 * 40)
        .min(2_500);
    match min_clip_ms {
        Some(shortest) => scaled.min((shortest / 2).max(250)),
        None => scaled,
    }
}

fn merge_duration_is_short(
    actual_ms: u64,
    clip_count: usize,
    expected: MergeDurationExpectation,
) -> bool {
    let tolerance = merge_duration_tolerance_ms(clip_count, expected.min_clip_ms);
    actual_ms.saturating_add(tolerance) < expected.total_ms
}

// 多段录制合并成一个 mp4：用 concat 解复用器直接拷流，不做二次编码。
pub async fn merge(
    sources: &[PathBuf],
    expected: Option<MergeDurationExpectation>,
    progress: Option<&Progress<'_>>,
) -> Result<PostResult, AppError> {
    if sources.len() < 2 {
        return Err(AppError::User("至少需要两段录像才能合并".into()));
    }
    for src in sources {
        ensure_file(src)?;
    }
    let first = &sources[0];
    let stamp = crate::clock::format_now_local("%Y%m%d-%H%M%S");
    // 同一秒可能点两次合并：原子占位唯一目标，避免互相覆盖清单与成品，
    // 再让 ffmpeg 的 -y 覆盖我们自己刚占的空文件（文档 196-18）。
    let out = reserve_merge_output(first, &stamp)?;
    let list_path = out.with_extension("concat.txt");
    let mut list_file = std::fs::File::create_new(&list_path).map_err(|err| {
        let _ = std::fs::remove_file(&out);
        AppError::from_io(err, &list_path)
    })?;
    let mut artifacts = MergeArtifacts {
        out: out.clone(),
        list: list_path.clone(),
        keep: false,
    };
    let mut list = String::new();
    for src in sources {
        // concat 清单按清单所在目录解析相对路径；源可能是旧版本留下的相对
        // 路径，直接写进去会把目录拼两次。统一转成绝对路径再写。
        let absolute = std::path::absolute(src).unwrap_or_else(|_| src.clone());
        let escaped = absolute.to_string_lossy().replace('\'', "'\\''");
        list.push_str(&format!("file '{escaped}'\n"));
    }
    std::io::Write::write_all(&mut list_file, list.as_bytes())?;
    drop(list_file);

    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-xerror".into(),
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
    result?;
    let size = size_of(&out);
    if size == 0 {
        return Err(AppError::Ffmpeg("合并输出为空".into()));
    }
    if let Some(expected) = expected {
        let actual_ms = crate::post::media_duration_ms(&out)
            .await
            .ok_or_else(|| AppError::Ffmpeg("无法验证合并产物时长".into()))?;
        if merge_duration_is_short(actual_ms, sources.len(), expected) {
            let tolerance = merge_duration_tolerance_ms(sources.len(), expected.min_clip_ms);
            return Err(AppError::Ffmpeg(format!(
                "合并产物时长不足：预期 {} 毫秒，实际 {} 毫秒（允许误差 {} 毫秒）",
                expected.total_ms, actual_ms, tolerance
            )));
        }
    }
    let _ = std::fs::remove_file(&list_path);
    artifacts.keep = true;
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

/// 合并录像时按顺序拼接各段弹幕；t 按前一段的时长整体后移。
///
/// 某段没有弹幕文件时仍然推进时间轴，保证后续弹幕不会错位。
pub struct DanmakuSource {
    pub jsonl: Option<PathBuf>,
    pub duration_ms: i64,
}

pub fn merge_danmaku(
    sources: &[DanmakuSource],
    out_video: &Path,
) -> std::io::Result<Option<PathBuf>> {
    use std::io::{BufRead, BufReader, Write};
    let out = out_video.with_extension("jsonl");
    let mut writer = std::io::BufWriter::new(std::fs::File::create_new(&out)?);
    let mut offset = 0i64;
    let mut count = 0u64;
    for source in sources {
        if let Some(jsonl) = source.jsonl.as_ref().filter(|path| path.is_file()) {
            // 长直播的 JSONL 可达几十 MB：逐行读，别整文件读进内存再切。
            if let Ok(file) = std::fs::File::open(jsonl) {
                for line in BufReader::new(file).lines() {
                    let Ok(line) = line else { continue };
                    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    let Some(t) = value.get("t").and_then(serde_json::Value::as_i64) else {
                        continue;
                    };
                    value["t"] = serde_json::json!(t + offset);
                    writeln!(writer, "{value}")?;
                    count += 1;
                }
            }
        }
        offset += source.duration_ms.max(0);
    }
    writer.flush()?;
    drop(writer);
    if count == 0 {
        let _ = std::fs::remove_file(&out);
        return Ok(None);
    }
    tracing::info!(
        target: "hivecast_lib::postprocess",
        task = "merge-danmaku",
        out = %out.display(),
        count,
        "合并弹幕完成"
    );
    Ok(Some(out))
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
    use std::io::{BufRead, BufReader, Write as _};

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut count = 0u64;
    for line in BufReader::new(std::fs::File::open(jsonl)?).lines() {
        let Ok(line) = line else { continue };
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
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
/// ASS 只能声明一个字体。macOS 用苹方；Windows 必须换成雅黑，否则 libass
/// 找不到 PingFang SC 会退到无中文字形的字体。Linux 用发行版常见的 Noto。
fn danmaku_font() -> &'static str {
    if cfg!(target_os = "macos") {
        "PingFang SC"
    } else if cfg!(target_os = "windows") {
        "Microsoft YaHei"
    } else {
        "Noto Sans CJK SC"
    }
}

/// Count readable chat records even when a previous export already exists.
pub fn recovered_chat_count(jsonl: &Path) -> std::io::Result<i64> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(std::fs::File::open(jsonl)?);
    let mut line = Vec::new();
    let mut count = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(count);
        }
        if serde_json::from_slice::<serde_json::Value>(&line)
            .ok()
            .is_some_and(|v| v["c"].as_str().is_some_and(|c| !c.trim().is_empty()))
        {
            count += 1;
        }
    }
}

pub fn danmaku_to_ass(jsonl: &Path) -> Result<PostResult, AppError> {
    write_danmaku_ass(jsonl, false)
}

fn render_danmaku_ass(jsonl: &Path) -> Result<String, AppError> {
    use std::io::{BufRead, BufReader};

    ensure_file(jsonl)?;
    // 仍需按时间排序，但读取逐行进行，避免 raw String 与解析结果双份占内存。
    let reader = BufReader::new(std::fs::File::open(jsonl)?);
    // 第四位表示「固定不滚动」：chat 之外（gift / super_chat）都按公告处理。
    let mut lines: Vec<(i64, String, u32, bool)> = Vec::new();
    for line in reader.split(b'\n') {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line?) else {
            continue;
        };
        let text = value["c"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let at = value["t"].as_i64().unwrap_or(0);
        let color = value["col"].as_u64().unwrap_or(0) as u32;
        let kind = value["k"].as_str().unwrap_or("chat");
        // 礼物/上舰在实时浮层里是「发送者：内容」，字幕也保持一致；
        // 聊天保持纯文本，避免每条滚动弹幕都顶一个名字。
        let text = if kind != "chat" {
            match value["u"].as_str().map(str::trim).filter(|u| !u.is_empty()) {
                Some(user) => format!("{user}：{text}"),
                None => text,
            }
        } else {
            text
        };
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
        "Style: Danmaku,{},{FONT_SIZE},&H00FFFFFF,&H00FFFFFF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,1,7,0,0,0,1\n\n",
        danmaku_font()
    ));
    ass.push_str("[Events]\n");
    ass.push_str(
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );

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
        // 下一条要等上一条的尾巴完全进入画面（再留 24px 间隙）才能复用轨道。
        // 固定 4 秒会让长弹幕还在屏幕右侧时就被下一条压住。
        let lane_ms = ((width as f64 + 24.0) / SCROLL_SPEED * 1000.0) as i64;
        lane_free[lane] = at + lane_ms.max(1);
        ass.push_str(&format!(
            "Dialogue: 0,{},{},Danmaku,,0,0,0,,{{\\move({PLAY_RES_X},{y},-{width},{y})\\c{colour}}}{body}\n",
            ass_time(at),
            ass_time(at + duration)
        ));
    }

    Ok(ass)
}

// New exports carry an ASS comment so routine startup can distinguish them from
// files written by the old direct-copy fallback without scanning their contents.
const ASS_INFO: &[u8] = b"[Script Info]\n";
const LEGACY_HEADER: &[u8] = b"[Script Info]\nScriptType: v4.00+\n";
const ATOMIC_EXPORT_COMMENT: &[u8] = b"; HIVECAST subtitle export\n";

pub fn write_danmaku_ass(jsonl: &Path, exclusive: bool) -> Result<PostResult, AppError> {
    use std::io::Write;
    let ass = render_danmaku_ass(jsonl)?;
    let out = sibling(jsonl, "", "ass");
    let mut file = tempfile::NamedTempFile::new_in(jsonl.parent().unwrap_or(Path::new(".")))?;
    file.write_all(ASS_INFO)?;
    file.write_all(ATOMIC_EXPORT_COMMENT)?;
    file.write_all(&ass.as_bytes()[ASS_INFO.len()..])?;
    file.as_file().sync_all()?;
    if exclusive {
        match file.persist_noclobber(&out) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(e.error.into());
            }
            Err(e) => {
                // 跨卷时在目标卷重新暂存并排他发布。
                crate::post::copy_no_clobber(e.file.path(), &out)?;
            }
        }
    } else {
        file.persist(&out).map_err(|e| e.error)?;
    }
    tracing::info!(
        target: "hivecast_lib::postprocess",
        task = "ass",
        src = %jsonl.display(),
        out = %out.display(),
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

/// Missing or interrupted legacy export. A user-authored ASS must remain intact.
/// Compare suspected truncations against the actual JSONL, including partial UTF-8.
pub(crate) fn ass_needs_recovery(track: &Path, count: i64) -> Result<bool, AppError> {
    incomplete_ass_at(track, &track.with_extension("ass"), count)
}

fn incomplete_ass_at(track: &Path, path: &Path, count: i64) -> Result<bool, AppError> {
    use std::io::{BufRead, BufReader, Read, Seek};
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(err.into()),
    };
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    (&mut reader)
        .take(LEGACY_HEADER.len() as u64)
        .read_to_end(&mut bytes)?;
    if !LEGACY_HEADER.starts_with(&bytes) {
        return Ok(false);
    }
    if bytes.is_empty() {
        return Ok(true);
    }
    // Old complete exports need only a line count, without loading either track into memory.
    let mut dialogues = 0;
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        if bytes.starts_with(b"Dialogue: ") && bytes.ends_with(b"\n") {
            dialogues += 1;
            if dialogues >= count && count > 0 {
                return Ok(false);
            }
        }
    }
    let expected = render_danmaku_ass(track)?;
    if reader.get_ref().metadata()?.len() >= expected.len() as u64 {
        return Ok(false);
    }
    reader.rewind()?;
    reader.read_to_end(&mut bytes)?;
    if !expected.as_bytes().starts_with(&bytes) {
        return Ok(false);
    }
    // An edited subtitle can legitimately end after any complete event. Older
    // exports have no ownership marker: preserve that ambiguous case in place.
    let last = bytes
        .strip_suffix(b"\n")
        .unwrap_or(&bytes)
        .rsplit(|b| *b == b'\n')
        .next()
        .unwrap_or_default();
    let complete_event = last.starts_with(b"Dialogue: ")
        && last.splitn(10, |b| *b == b',').nth(9).is_some_and(|text| {
            text.iter()
                .position(|b| *b == b'}')
                .is_some_and(|end| end + 1 < text.len())
                && std::str::from_utf8(text).is_ok()
        });
    Ok(!complete_event)
}

pub(crate) fn recover_danmaku_ass(track: &Path, count: i64) -> Result<(), AppError> {
    let path = track.with_extension("ass");
    if path.try_exists()? {
        if !incomplete_ass_at(track, &path, count)? {
            return Ok(());
        }
        // Pre-fix exports carried no ownership receipt. Preserve the original
        // bytes even if an edited subtitle happens to be a prefix of our export.
        let backup = tempfile::Builder::new()
            .prefix(".hivecast-ass-backup-")
            .tempdir_in(path.parent().unwrap_or(Path::new(".")))?;
        let saved = backup.path().join("original.ass");
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err(AppError::Path(format!(
                "字幕路径不是普通文件：{}",
                path.display()
            )));
        }
        std::fs::rename(&path, &saved)?;
        let backup_dir = backup.keep();
        // A concurrent writer may have installed a different file before rename.
        // Restore it exclusively, and keep the backup if the name is taken again.
        if !incomplete_ass_at(track, &saved, count)? {
            crate::post::copy_no_clobber(&saved, &path)?;
            let _ = std::fs::remove_dir(backup_dir);
            return Ok(());
        }
        tracing::warn!(path = %saved.display(), "旧版不完整字幕已留存");
    }
    match write_danmaku_ass(track, true) {
        Ok(_) => Ok(()),
        Err(AppError::Io(err))
            if err.kind() == std::io::ErrorKind::AlreadyExists
                && !ass_needs_recovery(track, count)? =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
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
    fn merge_reservation_preserves_existing_sidecars() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        for ext in ["concat.txt", "jsonl", "ass"] {
            let previous = dir.join("合并-test.mp4").with_extension(ext);
            std::fs::write(&previous, b"sentinel").unwrap();
            let out = reserve_merge_output(&dir.join("clip.mp4"), "test").unwrap();
            assert_eq!(out, dir.join("合并-test-1.mp4"));
            let list = out.with_extension("concat.txt");
            std::fs::File::create_new(&list).unwrap();
            drop(MergeArtifacts {
                out,
                list,
                keep: false,
            });
            assert_eq!(std::fs::read(&previous).unwrap(), b"sentinel");
            std::fs::remove_file(previous).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_merge_cleanup_removes_output_and_list() {
        let dir =
            std::env::temp_dir().join(format!("hivecast-merge-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("merge.mp4");
        let list = dir.join("merge.concat.txt");
        std::fs::write(&out, b"").unwrap();
        std::fs::write(
            &list,
            b"file 'a.ts'
",
        )
        .unwrap();
        {
            let _guard = MergeArtifacts {
                out: out.clone(),
                list: list.clone(),
                keep: false,
            };
        }
        assert!(!out.exists(), "失败合并不能留下 0 字节成品");
        assert!(!list.exists(), "失败合并不能留下清单");

        std::fs::write(&out, b"ok").unwrap();
        {
            let mut guard = MergeArtifacts {
                out: out.clone(),
                list: list.clone(),
                keep: false,
            };
            guard.keep = true;
        }
        assert!(out.exists(), "成功的成品必须保留");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    fn merges_danmaku_with_cumulative_offsets() {
        let dir = std::env::temp_dir().join(format!("hivecast-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("a.jsonl");
        let third = dir.join("c.jsonl");
        std::fs::write(&first, "{\"t\":100,\"k\":\"chat\",\"c\":\"a\"}\n").unwrap();
        std::fs::write(&third, "{\"t\":50,\"k\":\"chat\",\"c\":\"c\"}\n").unwrap();
        let sources = vec![
            DanmakuSource {
                jsonl: Some(first),
                duration_ms: 1_000,
            },
            DanmakuSource {
                jsonl: None,
                duration_ms: 1_000,
            },
            DanmakuSource {
                jsonl: Some(third),
                duration_ms: 2_000,
            },
        ];
        let out = dir.join("merged.mp4");
        let jsonl = merge_danmaku(&sources, &out).unwrap().unwrap();
        let text = std::fs::read_to_string(&jsonl).unwrap();
        assert!(text.contains("\"t\":100"));
        assert!(text.contains("\"t\":2050"));
        let empty = dir.join("empty.mp4");
        assert!(merge_danmaku(&[], &empty).unwrap().is_none());
        assert!(!empty.with_extension("jsonl").exists());
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

        // Reserve the unnumbered names across the next minute so both FFmpeg
        // runs collide with a pre-existing manifest even across a second boundary.
        let now = crate::clock::now_secs();
        let mut sentinels = Vec::new();
        for second in now..now + 60 {
            let stamp = crate::clock::format_local_opt(second, "%Y%m%d-%H%M%S").unwrap();
            let list = dir.join(format!("合并-{stamp}.concat.txt"));
            std::fs::write(&list, b"previous manifest").unwrap();
            sentinels.push(list);
        }
        let merged = merge(&[first.clone(), second.clone()], None, None)
            .await
            .unwrap();
        let source_total = size_of(&first) + size_of(&second);
        assert_eq!(merged.kind, "merge");
        assert!(
            merged.size as f64 > source_total as f64 * 0.6,
            "合并结果偏小：{} vs {}",
            merged.size,
            source_total
        );
        assert!(Path::new(&merged.path).is_file());

        assert!(Path::new(&merged.path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .ends_with("-1"));
        let bad = dir.join("broken.flv");
        std::fs::write(&bad, b"invalid media").unwrap();
        assert!(merge(&[first.clone(), bad, second.clone()], None, None)
            .await
            .is_err());
        // A playable output still fails validation if it falls short of the input timeline.
        assert!(merge(
            &[first, second],
            Some(MergeDurationExpectation {
                total_ms: 10_000,
                min_clip_ms: Some(3_000),
            }),
            None,
        )
        .await
        .is_err());
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "mp4"))
                .count(),
            1
        );
        for list in sentinels {
            assert_eq!(std::fs::read(list).unwrap(), b"previous manifest");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_outputs_never_share_a_name_in_the_same_second() {
        let dir = std::env::temp_dir().join(format!("hivecast-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("a.flv");
        let one = reserve_merge_output(&first, "20261125-010203").unwrap();
        let two = reserve_merge_output(&first, "20261125-010203").unwrap();
        assert_ne!(one, two);
        assert_eq!(one.parent(), Some(dir.as_path()));
        assert_eq!(two.parent(), Some(dir.as_path()));
        assert!(one.is_file() && two.is_file(), "占位文件必须原子创建");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_duration_tolerance_scales_without_hiding_dropped_clips() {
        // 2 段 1 秒录像保持 250 ms 容差：正常舍入放行，漏掉 1 段拦住。
        let two_clips = MergeDurationExpectation {
            total_ms: 2_000,
            min_clip_ms: Some(1_000),
        };
        assert_eq!(merge_duration_tolerance_ms(2, Some(1_000)), 250);
        assert!(!merge_duration_is_short(1_800, 2, two_clips));
        assert!(merge_duration_is_short(1_000, 2, two_clips));

        // 30 段 10 分钟真实直播合并：各段累计舍入 450 ms 放行，漏掉 1 段拦住。
        let thirty_clips = MergeDurationExpectation {
            total_ms: 18_000_000,
            min_clip_ms: Some(600_000),
        };
        assert_eq!(merge_duration_tolerance_ms(30, Some(600_000)), 1_370);
        assert!(!merge_duration_is_short(17_999_550, 30, thirty_clips));
        assert!(merge_duration_is_short(17_400_000, 30, thirty_clips));

        // 61 段长录像容差封顶 2500 ms。
        assert_eq!(merge_duration_tolerance_ms(61, Some(600_000)), 2_500);

        // 61 段 1 秒短录像：容差受最短段一半（500 ms）约束，漏掉 1 秒片段仍会拦住。
        let short_many = MergeDurationExpectation {
            total_ms: 61_000,
            min_clip_ms: Some(1_000),
        };
        assert_eq!(merge_duration_tolerance_ms(61, Some(1_000)), 500);
        assert!(!merge_duration_is_short(60_600, 61, short_many));
        assert!(merge_duration_is_short(60_000, 61, short_many));

        // 极短片段不会把容差压到低于原本的 250 ms。
        assert_eq!(merge_duration_tolerance_ms(30, Some(200)), 250);
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
        assert!(
            text.contains(&format!("Style: Danmaku,{},", danmaku_font())),
            "ASS 必须声明当前平台可用的中文字体"
        );
        assert!(text.contains("&H000000FF&"));
        assert!(text.contains("｛"));
        assert!(!text.contains("{\\move(0,0)}"));
        // 礼物走固定公告：居中、an8 顶部对齐，不带 move。
        assert!(text.contains("\\an8\\pos(640,"));
        assert!(text.contains("小刚：赠送 辣条 ×3"));
        assert!(text.contains("[30 元] 加油"), "SC 不能丢");
        assert!(text.contains("开通了 舰长 × 1"), "舰长不能丢");
        std::fs::remove_dir_all(&dir).ok();
    }
}
