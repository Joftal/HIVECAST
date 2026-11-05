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
    format!("{}.part%04d.ts", stem_of(final_path))
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
            if name.starts_with(&prefix) && name.ends_with(".ts") {
                Some(entry.path())
            } else {
                None
            }
        })
        .collect();
    parts.sort();
    parts
}

/// 合并分片为 mp4：-c copy 不转码，几分钟的录像一两秒完成。
///
/// 成功后删除分片与清单；失败保留现场，交给调用方决定是否降级成 .ts。
pub async fn merge(final_path: &Path) -> Result<u64, String> {
    let parts = collect_parts(final_path);
    if parts.is_empty() {
        return Err("没有分片".into());
    }
    let list = final_path.with_extension("concat.txt");
    let mut body = String::new();
    for part in &parts {
        // concat 清单里用单引号包路径，路径内的单引号要转义
        let text = part.display().to_string().replace('\u{27}', "'\\''");
        body.push_str(&format!("file '{text}'\n"));
    }
    tokio::fs::write(&list, body)
        .await
        .map_err(|err| format!("写合并清单失败：{err}"))?;

    let output = tokio::process::Command::new(ffmpeg::binary())
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
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(final_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| format!("启动 ffmpeg 失败：{err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("合并失败：{}", tail(&stderr, 240)));
    }
    let size = tokio::fs::metadata(final_path)
        .await
        .map(|meta| meta.len())
        .unwrap_or(0);
    if size == 0 {
        return Err("合并产物为空".into());
    }
    for part in &parts {
        let _ = tokio::fs::remove_file(part).await;
    }
    let _ = tokio::fs::remove_file(&list).await;
    Ok(size)
}

/// 兜底：把分片按字节拼成单个 .ts。
///
/// mpegts 是自同步的流式容器，字节级拼接仍然可播——比「合并失败只剩一堆分片」强。
pub async fn fallback_concat(final_path: &Path) -> Result<(PathBuf, u64), String> {
    let parts = collect_parts(final_path);
    if parts.is_empty() {
        return Err("没有分片".into());
    }
    let target = final_path.with_extension("ts");
    let mut out = tokio::fs::File::create(&target)
        .await
        .map_err(|err| format!("创建 {} 失败：{err}", target.display()))?;
    for part in &parts {
        let mut src = tokio::fs::File::open(part)
            .await
            .map_err(|err| format!("读取 {} 失败：{err}", part.display()))?;
        tokio::io::copy(&mut src, &mut out)
            .await
            .map_err(|err| format!("拼接 {} 失败：{err}", part.display()))?;
    }
    drop(out);
    let size = tokio::fs::metadata(&target)
        .await
        .map(|meta| meta.len())
        .unwrap_or(0);
    for part in &parts {
        let _ = tokio::fs::remove_file(part).await;
    }
    Ok((target, size))
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
        assert!(collect_parts(&final_path).is_empty(), "拼接后分片应清掉");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 真跑 ffmpeg：两片 ts 合成 mp4 且能被再次探测。
    #[tokio::test]
    #[ignore = "需要本机 ffmpeg"]
    async fn merge_makes_playable_mp4() {
        let dir = std::env::temp_dir().join(format!("hivecast-seg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("[t][a][b].mp4");
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

        let size = merge(&final_path).await.expect("合并");
        assert!(size > 1000, "mp4 太小：{size}");
        assert!(collect_parts(&final_path).is_empty(), "分片应被清理");
        assert!(final_path.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }
}
