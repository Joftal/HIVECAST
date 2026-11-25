//! Sparse byte ranges for the current playback track. Min/max covers unordered legacy JSONL too.
use hivecast_ipc::DanmakuMessage;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

struct Block {
    start: u64,
    end: u64,
    min: i64,
    max: i64,
}

struct Index {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
    blocks: Vec<Block>,
}

static CURRENT: Mutex<Option<Arc<Index>>> = Mutex::new(None);
const BLOCK_BYTES: u64 = 64 * 1024;

#[derive(serde::Deserialize)]
struct Timestamp {
    #[serde(default)]
    t: serde_json::Value,
}

fn offset(value: &serde_json::Value) -> i64 {
    value.get("t").and_then(|v| v.as_i64()).unwrap_or_default()
}

fn index(file: &File, path: &Path) -> io::Result<Arc<Index>> {
    let meta = file.metadata()?;
    let modified = meta.modified().ok();
    let mut cached = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = cached.as_ref() {
        if modified.is_some()
            && old.path == path
            && old.size == meta.len()
            && old.modified == modified
        {
            return Ok(old.clone());
        }
    }
    let mut reader = BufReader::new(file.take(meta.len()));
    let mut blocks = Vec::<Block>::new();
    let mut line = Vec::new();
    let mut pos = 0;
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if blocks.last().is_none_or(|b| pos - b.start >= BLOCK_BYTES) {
            blocks.push(Block {
                start: pos,
                end: pos,
                min: i64::MAX,
                max: i64::MIN,
            });
        }
        pos += n as u64;
        let b = blocks.last_mut().unwrap();
        b.end = pos;
        if let Ok(row) = serde_json::from_slice::<Timestamp>(&line) {
            let t = row.t.as_i64().unwrap_or_default();
            b.min = b.min.min(t);
            b.max = b.max.max(t);
        }
    }
    let new = Arc::new(Index {
        path: path.to_owned(),
        size: meta.len(),
        modified,
        blocks,
    });
    *cached = Some(new.clone());
    Ok(new)
}

pub fn read(path: &Path, from: Option<i64>, to: Option<i64>) -> io::Result<Vec<DanmakuMessage>> {
    let mut file = File::open(path)?;
    let idx = index(&file, path)?;
    let mut messages = Vec::new();
    let mut line = Vec::new();
    for b in &idx.blocks {
        if from.is_some_and(|v| v > b.max) || to.is_some_and(|v| v < b.min) {
            continue;
        }
        file.seek(SeekFrom::Start(b.start))?;
        let mut reader = BufReader::new((&mut file).take(b.end - b.start));
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let Ok(row) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            let t = offset(&row);
            if from.is_some_and(|v| t < v) || to.is_some_and(|v| t > v) {
                continue;
            }
            let Some(content) = row["c"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            messages.push(DanmakuMessage {
                offset_ms: t,
                content: content.into(),
                kind: row["k"].as_str().unwrap_or("chat").into(),
                user: row["u"].as_str().map(str::to_owned),
                color: row["col"].as_u64().unwrap_or(0xffffff) as u32,
            });
        }
    }
    messages.sort_by_key(|m| m.offset_ms);
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn late_out_of_order_and_appended_rows_are_visible() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let mut file = File::create(&path).unwrap();
        for _ in 0..4000 {
            writeln!(file, r#"{{"t":9000,"c":"later"}}"#).unwrap();
        }
        writeln!(file, r#"{{"t":2,"c":"early"}}"#).unwrap();
        assert_eq!(read(&path, Some(0), Some(3)).unwrap().len(), 1);
        writeln!(file, r#"{{"t":1,"c":"appended"}}"#).unwrap();
        let rows = read(&path, None, Some(3)).unwrap();
        assert_eq!(rows.iter().map(|m| m.offset_ms).collect::<Vec<_>>(), [1, 2]);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    #[ignore = "百万行弹幕性能验收"]
    fn million_message_seek() {
        use std::time::Instant;
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let mut out = std::io::BufWriter::new(File::create(&path).unwrap());
        for n in 0..1_000_000 {
            writeln!(out, r#"{{"t":{},"c":"abc","k":"chat"}}"#, n * 50).unwrap();
        }
        out.flush().unwrap();
        drop(out);
        for start in [0, 120_000, 240_000, 48_000_000, 0] {
            let timer = Instant::now();
            let rows = read(&path, Some(start), Some(start + 120_000)).unwrap();
            assert_eq!(rows.len(), 2401);
            assert_eq!(rows[0].offset_ms, start);
            println!(
                "window={start} count={} elapsed_ms={} bytes={}",
                rows.len(),
                timer.elapsed().as_millis(),
                path.metadata().unwrap().len()
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}
