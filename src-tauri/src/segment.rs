//! 录制分片：ffmpeg 的 segment muxer 每 5 分钟切一个 mpegts 文件，
//! 进程被杀最多丢最后一片。收尾时用 concat demuxer 无转码合成单个 mp4。
//!
//! 为什么是 mpegts 而不是 fmp4：见 docs/26 §4.2 —— ffmpeg 9 从网络流转 fmp4
//! 会写出缺 tfhd 的 moof，hls.js 直接报 mediaSourceRequiresReset；mpegts 既能
//! 单独播放，也能按字节拼接，是「崩溃可救」的容器。
//!
//! 命名约定：最终文件 [时间][主播][标题].mp4，分片 [时间][主播][标题].part0000.ts，
//! 合并在同一目录下完成，不额外建临时目录（用户能在 Finder 里一眼看出残留）。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::ffmpeg;

/// 单片时长：1080p 约 250 MB，崩溃最多丢 5 分钟。
pub const SEGMENT_SECONDS: u32 = 300;

/// ffmpeg 的 segment 模板：把 %04d 交给 ffmpeg 展开。
pub fn parts_pattern(final_path: &Path) -> String {
    let stem = stem_of(final_path).replace('%', "%%");
    format!("{stem}.part%04d.ts")
}

/// 某一片的名字（测试用：构造分片再验证排序与拼接）。
#[cfg(test)]
pub fn part_name(final_path: &Path, index: u32) -> String {
    format!("{}.part{index:04}.ts", stem_of(final_path))
}

fn stem_of(final_path: &Path) -> String {
    final_path.with_extension("").display().to_string()
}

/// 收集本次录制的全部分片，按编号升序（字典序等于编号序，因为补了零）。
pub fn collect_parts(final_path: &Path) -> Vec<PathBuf> {
    collect_parts_range(final_path, 0, None)
}

/// 只收集编号在 `[start, end)` 里的分片：`end=None` 表示到末尾。
///
/// 分割录制时同一个 ffmpeg 持续写 `partNNNN.ts`，收尾按范围把不同的
/// 片段合并成各自的成品，因此不能再无脑合并全部。
pub fn collect_parts_range(final_path: &Path, start: u32, end: Option<u32>) -> Vec<PathBuf> {
    let mut parts = collect_all_parts(final_path);
    parts.retain(|part| {
        let index = part_index(part).unwrap_or(u32::MAX);
        index >= start && end.is_none_or(|end| index < end)
    });
    parts
}

/// 分片文件名的编号；不是本场分片时返回 None。
pub fn part_index(part: &Path) -> Option<u32> {
    let name = part.file_name()?.to_str()?;
    let (_, digits) = name.rsplit_once(".part")?;
    parse_part_digits(digits.strip_suffix(".ts")?)
}

/// ffmpeg 的 `%04d` 只会写出 0000…9999，以及 10000 起的五位数字；
/// `part1.ts`、`part00001.ts` 都不是本场产物，超过 u32 的编号也不能收。
fn parse_part_digits(digits: &str) -> Option<u32> {
    if digits.len() < 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 4 && digits.starts_with('0') {
        return None;
    }
    digits.parse().ok()
}

/// 本场分片里最大的编号 + 1：续录时从这里接着编号，
/// 即使前面的分片已经被分割流程删掉也不会覆盖旧片。
pub fn next_part_index(final_path: &Path) -> u32 {
    collect_all_parts(final_path)
        .iter()
        .filter_map(|part| part_index(part))
        .max()
        .map_or(0, |index| index.saturating_add(1))
}

fn collect_all_parts(final_path: &Path) -> Vec<PathBuf> {
    let Some(parent) = final_path.parent() else {
        return Vec::new();
    };
    let Some(stem) = final_path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{stem}.part");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut parts: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            // 必须是「本场 stem.part + 纯数字 + .ts」。只匹配前缀会把
            // `room.part2.part0001.ts`（另一场/另一段）也收进来，收尾时误删。
            let digits = name.strip_prefix(&prefix)?.strip_suffix(".ts")?;
            parse_part_digits(digits)?;
            Some(entry.path())
        })
        .collect();
    // 编号补零，字典序在超过 9999 后会错（part10000 < part9999）。
    parts.sort_by_key(|part| part_index(part).unwrap_or(u32::MAX));
    parts
}

/// 合并分片为 mp4：-c copy 不转码，几分钟的录像一两秒完成。
///
/// 原始分片由调用方在历史登记后清理。
#[cfg(test)]
pub async fn merge(final_path: &Path) -> Result<u64, String> {
    merge_range(final_path, final_path, 0, None).await
}

/// 合并指定范围的分片到 `output`。
///
/// `final_path` 是分片基名（决定 `partNNNN.ts` 的查找位置），`output`
/// 是这一段的成品路径。成功后保留输入分片，等待调用方登记成品。
pub async fn merge_range(
    final_path: &Path,
    output: &Path,
    start: u32,
    end: Option<u32>,
) -> Result<u64, String> {
    let parts = collect_parts_range(final_path, start, end);
    if parts.is_empty() {
        return Err("没有分片".into());
    }
    // 已经非空的成品说明上一次合并成功但还没来得及删分片/写清单。
    // 直接报错让调用方复用，别把完整文件覆盖成一次新的（可能残缺的）合并。
    if let Ok(meta) = tokio::fs::metadata(output).await {
        if meta.len() > 0 {
            return Err("合并目标已存在且非空".into());
        }
    }
    let started = std::time::Instant::now();
    let total: u64 = parts
        .iter()
        .filter_map(|part| std::fs::metadata(part).ok())
        .map(|meta| meta.len())
        .sum();
    tracing::debug!(
        target: "hivecast_lib::segment",
        path = %output.display(),
        parts = parts.len(),
        bytes = total,
        "开始合并分片"
    );
    let scratch = tempfile::Builder::new()
        .prefix(".hivecast-merge-")
        .tempdir_in(output.parent().unwrap_or(Path::new(".")))
        .map_err(|err| format!("创建合成临时目录失败：{err}"))?;
    let list = scratch.path().join("inputs.txt");
    let mut body = String::new();
    for part in &parts {
        // concat 清单按清单所在目录解析相对路径；媒体目录若曾是相对路径，
        // 会把目录拼两次。这里统一写绝对路径，再转义单引号。
        let absolute = std::path::absolute(part).unwrap_or_else(|_| part.clone());
        let text = absolute.display().to_string().replace('\u{27}', "'\\''");
        body.push_str(&format!("file '{text}'\n"));
    }
    tokio::fs::write(&list, body)
        .await
        .map_err(|err| format!("写合并清单失败：{err}"))?;

    // 先写临时文件，成功后再改名到成品。否则「ffmpeg 已经写完、分片删了
    // 一半」时崩溃，下次恢复会把剩余分片当成完整范围重新合并，覆盖掉本来
    // 完好的成品。临时文件保留原扩展名，让 ffmpeg 选对封装器。
    let staging = scratch
        .path()
        .join("output")
        .with_extension(output.extension().unwrap_or_default());
    let mut command = tokio::process::Command::new(ffmpeg::binary());
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(&list)
        .arg("-c")
        .arg("copy");
    // faststart 只对 MP4 系容器有意义；给 FLV/TS/MKV 传会被 ffmpeg 拒绝。
    if matches!(
        staging.extension().and_then(|ext| ext.to_str()),
        Some("mp4" | "mov" | "m4v")
    ) {
        command.arg("-movflags").arg("+faststart");
    }
    let run = command
        .arg(&staging)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await;
    let run = run.map_err(|err| format!("启动 ffmpeg 失败：{err}"))?;
    if !run.status.success() {
        return Err(format!(
            "合并失败：{}",
            tail(&String::from_utf8_lossy(&run.stderr), 240)
        ));
    }
    let size = tokio::fs::metadata(&staging)
        .await
        .map_err(|e| e.to_string())?
        .len();
    if size == 0 {
        return Err("合并产物为空".into());
    }
    tokio::fs::rename(&staging, output)
        .await
        .map_err(|err| format!("提交合成成品失败：{err}"))?;
    tracing::info!(
        target: "hivecast_lib::segment",
        path = %output.display(),
        parts = parts.len(),
        bytes = size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "分片合并完成"
    );
    Ok(size)
}

/// 兜底：把分片按字节拼成单个 .ts。
///
/// mpegts 是自同步的流式容器，字节级拼接仍然可播——比「合并失败只剩一堆分片」强。
#[cfg(test)]
pub async fn fallback_concat(final_path: &Path) -> Result<(PathBuf, u64), String> {
    fallback_concat_range(final_path, final_path, 0, None).await
}

/// 把指定范围的分片按字节拼成单个 .ts，目标名由 `output` 决定。
pub async fn fallback_concat_range(
    final_path: &Path,
    output: &Path,
    start: u32,
    end: Option<u32>,
) -> Result<(PathBuf, u64), String> {
    let parts = collect_parts_range(final_path, start, end);
    if parts.is_empty() {
        return Err("没有分片".into());
    }
    let target = output.with_extension("ts");
    tracing::warn!(
        target: "hivecast_lib::segment",
        path = %output.display(),
        parts = parts.len(),
        "改用字节拼接分片"
    );
    // Only release input parts once the caller has recorded the actual fallback path.
    let staging = tempfile::NamedTempFile::new_in(output.parent().unwrap_or(Path::new(".")))
        .map_err(|e| e.to_string())?;
    let mut out = tokio::fs::File::from_std(staging.reopen().map_err(|e| e.to_string())?);
    let mut size = 0;
    for part in &parts {
        let mut src = tokio::fs::File::open(part)
            .await
            .map_err(|e| e.to_string())?;
        size += tokio::io::copy(&mut src, &mut out)
            .await
            .map_err(|e| e.to_string())?;
    }
    out.sync_all().await.map_err(|e| e.to_string())?;
    drop(out);
    if size == 0 {
        return Err("拼接产物为空".into());
    }
    let mut staging = staging;
    let mut candidate = target.clone();
    for index in 2.. {
        match staging.persist_noclobber(&candidate) {
            Ok(_) => break,
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                staging = e.file;
                candidate = target.with_file_name(format!(
                    "{} ({index}).ts",
                    target.file_stem().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(e) => return Err(e.error.to_string()),
        }
    }
    let target = candidate;
    tracing::info!(
        target: "hivecast_lib::segment",
        path = %target.display(),
        bytes = size,
        "字节拼接完成"
    );
    Ok((target, size))
}

/// 一条会话最后一次「有数据」的时间（I1）。
///
/// 崩溃恢复不能把结束时间记成重启时刻——那会让 32 KB 的残留分片显示成
/// 录了五个小时。取成品、弹幕、分片里最新的 mtime，都找不到时返回 None。
pub fn last_data_at(final_path: &str) -> Option<i64> {
    use std::time::UNIX_EPOCH;
    let path = Path::new(final_path);
    let mut newest: Option<i64> = None;
    let mut consider = |candidate: &Path| {
        let Ok(meta) = std::fs::metadata(candidate) else {
            return;
        };
        let Ok(modified) = meta.modified() else {
            return;
        };
        let Ok(age) = modified.duration_since(UNIX_EPOCH) else {
            return;
        };
        let secs = age.as_secs() as i64;
        newest = Some(newest.map_or(secs, |old: i64| old.max(secs)));
    };
    consider(path);
    consider(&path.with_extension("jsonl"));
    for part in collect_parts(path) {
        consider(&part);
    }
    newest
}

/// 分片总大小（合并彻底失败时用来填历史记录）。
pub fn parts_total(final_path: &Path) -> u64 {
    collect_parts(final_path)
        .iter()
        .filter_map(|part| std::fs::metadata(part).ok())
        .map(|meta| meta.len())
        .sum()
}

fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= max {
        return trimmed.to_string();
    }
    chars[chars.len() - max..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_and_names_line_up() {
        let path = Path::new("/tmp/lib/哔哩哔哩/主播/[2026-11-05 10-00-00][主播][标题].mp4");
        assert_eq!(
            parts_pattern(path),
            "/tmp/lib/哔哩哔哩/主播/[2026-11-05 10-00-00][主播][标题].part%04d.ts"
        );
        assert_eq!(
            part_name(path, 3),
            "/tmp/lib/哔哩哔哩/主播/[2026-11-05 10-00-00][主播][标题].part0003.ts"
        );
    }

    #[test]
    fn pattern_escapes_percent_in_the_recording_name() {
        // 目录和文件名都可能有百分号：模板是完整路径，两处都要转义。
        let path = Path::new("/tmp/100%媒体/[100%][a][b].mp4");
        assert_eq!(
            parts_pattern(path),
            "/tmp/100%%媒体/[100%%][a][b].part%04d.ts",
            "百分号必须转义，否则 ffmpeg 会把它当模板"
        );
    }

    #[test]
    fn collect_parts_rejects_nested_partial_names_and_sorts_by_number() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        std::fs::write(part_name(&final_path, 9999), b"a").unwrap();
        std::fs::write(part_name(&final_path, 10_000), b"b").unwrap();
        // 前缀相同但编号不是纯数字：上一场的分片，不能并进这一场。
        std::fs::write(dir.join("[t][a][b].part2.part0001.ts"), b"x").unwrap();
        std::fs::write(dir.join("[t][a][b].partial.ts"), b"y").unwrap();
        // 非 %04d 产物、前导零多于四位、u32 溢出：都不能当成本场分片。
        std::fs::write(dir.join("[t][a][b].part1.ts"), b"p").unwrap();
        std::fs::write(dir.join("[t][a][b].part00001.ts"), b"q").unwrap();
        std::fs::write(dir.join("[t][a][b].part99999999999.ts"), b"r").unwrap();

        let parts = collect_parts(&final_path);
        let names: Vec<String> = parts
            .iter()
            .map(|part| part.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["[t][a][b].part9999.ts", "[t][a][b].part10000.ts"],
            "必须按数字排序，且只收本场分片"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collect_parts_sorts_and_ignores_other_files() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        std::fs::write(part_name(&final_path, 10), b"c").unwrap();
        std::fs::write(part_name(&final_path, 2), b"b").unwrap();
        std::fs::write(part_name(&final_path, 0), b"a").unwrap();
        std::fs::write(dir.join("[t][a][b].jsonl"), b"{}").unwrap();

        let parts = collect_parts(&final_path);
        let names: Vec<String> = parts
            .iter()
            .map(|part| part.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "[t][a][b].part0000.ts",
                "[t][a][b].part0002.ts",
                "[t][a][b].part0010.ts"
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parts_total_sums_every_remaining_part() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        std::fs::write(part_name(&final_path, 0), vec![0u8; 128]).unwrap();
        std::fs::write(part_name(&final_path, 1), vec![0u8; 256]).unwrap();
        assert_eq!(parts_total(&final_path), 384);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn next_part_index_continues_after_the_highest_remaining_part() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        assert_eq!(next_part_index(&final_path), 0, "没有分片时从 0 开始");
        std::fs::write(part_name(&final_path, 0), b"a").unwrap();
        std::fs::write(part_name(&final_path, 3), b"b").unwrap();
        std::fs::write(part_name(&final_path, 1), b"c").unwrap();
        assert_eq!(next_part_index(&final_path), 4, "不能覆盖已有最大编号");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn merge_refuses_to_overwrite_a_complete_output() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        let output = dir.join("clip.mp4");
        std::fs::write(part_name(&final_path, 0), b"AAA").unwrap();
        std::fs::write(part_name(&final_path, 1), b"BBB").unwrap();
        std::fs::write(&output, b"COMPLETE").unwrap();

        let err = merge_range(&final_path, &output, 0, None)
            .await
            .expect_err("不能覆盖完整成品");
        assert!(err.contains("已存在"));
        assert_eq!(std::fs::read(&output).unwrap(), b"COMPLETE");
        assert_eq!(collect_parts_range(&final_path, 0, None).len(), 2);
        assert!(!dir.join(".clip.merging.mp4").exists(), "临时文件应清理");
        assert!(!output.with_extension("concat.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fallback_concat_joins_bytes_in_order() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        std::fs::write(part_name(&final_path, 0), b"AAA").unwrap();
        std::fs::write(part_name(&final_path, 1), b"BBB").unwrap();

        let (target, size) = fallback_concat(&final_path).await.expect("拼接");
        assert_eq!(target.extension().unwrap(), "ts");
        assert_eq!(size, 6);
        assert_eq!(std::fs::read(&target).unwrap(), b"AAABBB");
        assert_eq!(
            collect_parts(&final_path).len(),
            2,
            "登记兜底路径之前保留分片"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fallback_concat_keeps_an_existing_target() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        // 上一场兜底留下的同名 .ts：不能被这一场截断覆盖（H5）。
        let existing = final_path.with_extension("ts");
        std::fs::write(&existing, b"OLD").unwrap();
        std::fs::write(part_name(&final_path, 0), b"AAA").unwrap();

        let (target, size) = fallback_concat(&final_path).await.expect("拼接");
        assert_ne!(target, existing);
        assert_eq!(size, 3);
        assert_eq!(std::fs::read(&target).unwrap(), b"AAA");
        assert_eq!(std::fs::read(&existing).unwrap(), b"OLD");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn last_data_at_uses_the_newest_artifact() {
        use std::time::{Duration, UNIX_EPOCH};
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
        let old = part_name(&final_path, 0);
        std::fs::write(&old, b"a").unwrap();
        std::fs::write(&final_path, b"old-mp4").unwrap();
        std::fs::write(final_path.with_extension("jsonl"), b"{}").unwrap();

        // 把三份产物设成不同时间：取最新的那片。
        let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let jsonl = final_path.with_extension("jsonl");
        let artifacts = vec![
            (std::path::PathBuf::from(old), 10u64),
            (final_path.clone(), 30),
            (jsonl, 20),
        ];
        for (path, offset) in artifacts {
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(base + Duration::from_secs(offset))
                .unwrap();
        }
        assert_eq!(
            last_data_at(&final_path.display().to_string()),
            Some(1_700_000_030)
        );

        // 全都不存在时返回 None，让调用方退回 now。
        assert_eq!(last_data_at("/tmp/definitely-missing-hivecast.mp4"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 真跑 ffmpeg：两片 ts 合成 mp4 且能被再次探测。
    #[tokio::test]
    #[ignore = "需要本机 ffmpeg"]
    async fn merge_makes_playable_mp4() {
        // 目录与文件名都带 %：验证 ffmpeg 模板转义在完整路径上生效。
        let dir = std::env::temp_dir().join(format!("hivecast-100%seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][100%][b].mp4");
        let status = tokio::process::Command::new(ffmpeg::binary())
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
                "4",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                // segment 只在关键帧处切分：GOP 不压到 2 秒，4 秒素材切不出两片
                "-g",
                "20",
                "-c:a",
                "aac",
                "-f",
                "segment",
                "-segment_time",
                "2",
                "-reset_timestamps",
                "1",
                "-segment_format",
                "mpegts",
                &parts_pattern(&final_path),
            ])
            .status()
            .await
            .expect("生成分片");
        assert!(status.success());
        assert!(collect_parts(&final_path).len() >= 2, "至少要切出两片");

        let list = final_path.with_extension("concat.txt");
        let staging = dir.join(".[t][100%][b].merging.mp4");
        std::fs::write(&list, b"existing list").unwrap();
        std::fs::write(&staging, b"existing staging").unwrap();
        let size = merge(&final_path).await.expect("合并");
        assert!(size > 1000, "mp4 太小：{size}");
        assert!(collect_parts(&final_path).len() >= 2);
        assert_eq!(std::fs::read(&list).unwrap(), b"existing list");
        assert_eq!(std::fs::read(&staging).unwrap(), b"existing staging");
        let invalid = dir.join("invalid.mp4");
        let bad_list = invalid.with_extension("concat.txt");
        let bad_stage = dir.join(".invalid.merging.mp4");
        std::fs::write(&bad_list, b"existing list").unwrap();
        std::fs::write(&bad_stage, b"existing staging").unwrap();
        std::fs::write(part_name(&invalid, 0), b"invalid media").unwrap();
        assert!(merge(&invalid).await.is_err());
        assert_eq!(std::fs::read(&bad_list).unwrap(), b"existing list");
        assert_eq!(std::fs::read(&bad_stage).unwrap(), b"existing staging");
        assert_eq!(
            std::fs::read(part_name(&invalid, 0)).unwrap(),
            b"invalid media"
        );
        assert!(final_path.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }
}
