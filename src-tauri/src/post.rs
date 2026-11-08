//! 录制后处理 v1：源封装母带 + 三种互斥模式（source / convert / custom）。
//!
//! 设计约束（docs/77）：
//! - 录制阶段只产出「源封装母带」，后处理产物是派生文件；
//! - 派生文件先写临时文件，ffprobe 校验通过后才改名；
//! - 只有 convert 模式允许把源移进系统回收站，且必须在数据库提交之后；
//! - custom 只执行 ffmpeg，不经过 shell，模板必须含 {input}/{output}，源永不自动删。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hivecast_ipc::Settings;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::ffmpeg;
use crate::platform::{Codec, StreamFormat};
use crate::settings;

pub const STATE_RECORDING: &str = "recording";
pub const STATE_DONE: &str = "done";
pub const STATE_FAILED: &str = "failed";

const DEFAULT_TIMEOUT_SEC: u32 = 600;
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(120);

/// 一场录制在开录时快照下来的后处理配置；中途改设置只影响新会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostConfig {
    pub mode: String,
    pub convert_format: String,
    pub delete_source: bool,
    pub custom_command: String,
    pub custom_output_ext: String,
    pub custom_timeout_sec: u32,
}

impl Default for PostConfig {
    fn default() -> Self {
        Self {
            mode: settings::POST_MODE_SOURCE.to_string(),
            convert_format: "mp4".into(),
            delete_source: false,
            custom_command: "-i {input} -c copy {output}".into(),
            custom_output_ext: "mp4".into(),
            custom_timeout_sec: DEFAULT_TIMEOUT_SEC,
        }
    }
}

impl PostConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            mode: settings.post_mode.clone(),
            convert_format: settings.post_convert_format.clone(),
            delete_source: settings.post_delete_source,
            custom_command: settings.post_custom_command.clone(),
            custom_output_ext: sanitize_output_ext(&settings.post_custom_output_ext),
            custom_timeout_sec: settings.post_custom_timeout_sec.clamp(
                settings::POST_CUSTOM_TIMEOUT_MIN,
                settings::POST_CUSTOM_TIMEOUT_MAX,
            ),
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(
            self.custom_timeout_sec
                .max(settings::POST_CUSTOM_TIMEOUT_MIN),
        ))
    }
}

/// 后处理结果：无论成功失败，source_path 都指向可播放的源封装母带。
#[derive(Debug, Clone)]
pub struct PostOutcome {
    pub path: PathBuf,
    pub size: i64,
    pub source_path: PathBuf,
    pub state: String,
    pub error: Option<String>,
    /// 调用方在数据库提交之后才执行删除；失败时始终为 None。
    pub delete_source: Option<PathBuf>,
}

impl PostOutcome {
    fn source_only(src: &Path, state: &str, error: Option<String>) -> Self {
        let size = file_size(src);
        Self {
            path: src.to_path_buf(),
            size,
            source_path: src.to_path_buf(),
            state: state.to_string(),
            error,
            delete_source: None,
        }
    }
}

/// 源封装扩展名：FLV + HEVC/AV1 不能装进 FLV，统一用 MKV。
pub fn source_extension(format: StreamFormat, codec: Option<Codec>) -> &'static str {
    match format {
        // 音频流只有 AAC 能直接装进 m4a；其它编码用 Matroska 音频容器。
        StreamFormat::AudioOnly => match codec {
            Some(Codec::Aac) => "m4a",
            _ => "mka",
        },
        StreamFormat::Flv => match codec {
            Some(Codec::H265) | Some(Codec::Av1) => "mkv",
            _ => "flv",
        },
        // DASH 本身就是 fMP4 分片，直接落 mp4 最稳；HLS/HTTP-TS 保留 ts。
        StreamFormat::Dash => "mp4",
        StreamFormat::Hls | StreamFormat::HttpTs => "ts",
    }
}

/// 执行后处理。失败不抛错：返回源文件 + failed 状态，调用方照常写历史。
pub async fn run(src: &Path, cfg: &PostConfig) -> PostOutcome {
    if !src.is_file() {
        return PostOutcome::source_only(src, STATE_FAILED, Some("源文件不存在".into()));
    }
    match cfg.mode.as_str() {
        settings::POST_MODE_CONVERT => run_convert(src, cfg).await,
        settings::POST_MODE_CUSTOM => run_custom(src, cfg).await,
        _ => {
            // 源封装就是母带，仍然要确认容器可读：合并命令退出码为 0 不等于文件完整。
            match validate_media(src).await {
                Ok(()) => PostOutcome::source_only(src, STATE_DONE, None),
                Err(err) => PostOutcome::source_only(
                    src,
                    STATE_FAILED,
                    Some(format!("源封装校验失败：{err}")),
                ),
            }
        }
    }
}

async fn run_convert(src: &Path, cfg: &PostConfig) -> PostOutcome {
    let format = if settings::POST_CONVERT_FORMATS.contains(&cfg.convert_format.as_str()) {
        cfg.convert_format.as_str()
    } else {
        "mp4"
    };
    if src.extension().and_then(|ext| ext.to_str()) == Some(format) {
        return match validate_media(src).await {
            Ok(()) => PostOutcome::source_only(src, STATE_DONE, None),
            Err(err) => PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("源文件已经是 {format}，但校验失败：{err}")),
            ),
        };
    }
    let target = target_for(src, format);
    if let Some(outcome) = reuse_existing(src, &target, cfg).await {
        return outcome;
    }
    let temp = temp_path(src, format);
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-nostdin"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        src.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from("0"),
        OsString::from("-c"),
        OsString::from("copy"),
        OsString::from("-avoid_negative_ts"),
        OsString::from("make_zero"),
    ];
    if matches!(format, "mp4" | "mov" | "m4v") {
        args.push(OsString::from("-movflags"));
        args.push(OsString::from("+faststart"));
    }
    args.push(temp.as_os_str().to_os_string());
    finish_command(src, cfg, &target, &temp, args, "转封装").await
}

async fn run_custom(src: &Path, cfg: &PostConfig) -> PostOutcome {
    let ext = sanitize_output_ext(&cfg.custom_output_ext);
    let target = target_for(src, &ext);
    if let Some(outcome) = reuse_existing(src, &target, cfg).await {
        return outcome;
    }
    let temp = temp_path(src, &ext);
    let tokens = match parse_command(&cfg.custom_command) {
        Ok(tokens) => tokens,
        Err(err) => return PostOutcome::source_only(src, STATE_FAILED, Some(err)),
    };
    let mut args = Vec::new();
    for token in tokens {
        if args.is_empty() && is_ffmpeg_token(&token) {
            continue;
        }
        // 先替换 output 再替换 input：即使源文件名里恰好含有 "{output}"，
        // 也不会在第二步被再次替换。
        let replaced = token
            .replace("{output}", &temp.display().to_string())
            .replace("{input}", &src.display().to_string());
        args.push(OsString::from(replaced));
    }
    if args.is_empty() {
        return PostOutcome::source_only(src, STATE_FAILED, Some("自定义命令没有有效参数".into()));
    }
    finish_command(src, cfg, &target, &temp, args, "自定义后处理").await
}

async fn finish_command(
    src: &Path,
    cfg: &PostConfig,
    target: &Path,
    temp: &Path,
    args: Vec<OsString>,
    label: &str,
) -> PostOutcome {
    let _ = tokio::fs::remove_file(temp).await;
    let mut command = Command::new(ffmpeg::binary());
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = match tokio::time::timeout(cfg.timeout(), command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            let _ = tokio::fs::remove_file(temp).await;
            return PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("启动 ffmpeg 失败：{err}")),
            );
        }
        Err(_) => {
            let _ = tokio::fs::remove_file(temp).await;
            return PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("{label}超时（{} 秒）", cfg.timeout().as_secs())),
            );
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        let _ = tokio::fs::remove_file(temp).await;
        let reason = if tail.is_empty() {
            format!("{label}失败（ffmpeg 无输出）")
        } else {
            format!("{label}失败：{tail}")
        };
        return PostOutcome::source_only(src, STATE_FAILED, Some(reason));
    }
    if let Err(err) = validate_media(temp).await {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("产物校验失败：{err}")));
    }
    let sync = match tokio::fs::File::open(temp).await {
        Ok(file) => file.sync_all().await,
        Err(err) => Err(err),
    };
    if let Err(err) = sync {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("产物落盘失败：{err}")));
    }
    // 目标可能来自上一次失败尝试：先移除再改名，Windows 的 rename 不覆盖。
    if target.exists() {
        if let Err(err) = tokio::fs::remove_file(target).await {
            let _ = tokio::fs::remove_file(temp).await;
            return PostOutcome::source_only(
                src,
                STATE_FAILED,
                Some(format!("替换旧产物失败：{err}")),
            );
        }
    }
    if let Err(err) = tokio::fs::rename(temp, target).await {
        let _ = tokio::fs::remove_file(temp).await;
        return PostOutcome::source_only(src, STATE_FAILED, Some(format!("产物改名失败：{err}")));
    }
    let delete_source =
        (cfg.mode == settings::POST_MODE_CONVERT && cfg.delete_source && target != src)
            .then(|| src.to_path_buf());
    PostOutcome {
        path: target.to_path_buf(),
        size: file_size(target),
        source_path: src.to_path_buf(),
        state: STATE_DONE.to_string(),
        error: None,
        delete_source,
    }
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or(0)
}

/// 派生文件的确定目标：同名同扩展名且不早于源文件时才复用。
///
/// 崩溃恢复会重跑后处理；每次都换 `.1` 会让数据库指向新文件、旧产物永远留下。
/// 但同名文件也可能来自上一场录制（源已删、只剩派生），所以必须比较 mtime：
/// 早于当前源文件的派生文件是别人的，跳过，不能覆盖也不能直接复用。
fn target_for(src: &Path, ext: &str) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let mut candidate = parent.join(format!("{stem}.{ext}"));
    let mut index = 1;
    // 自定义后处理的输出扩展名可能与源相同；此时绝不能把源本身当目标，
    // 否则 finish_command 会先删掉源再改名。显式跳过源文件。
    while candidate == src || candidate.exists() {
        if candidate != src && is_current_derived(&candidate, src) {
            return candidate;
        }
        candidate = parent.join(format!("{stem}.{index}.{ext}"));
        index += 1;
    }
    candidate
}

/// 派生文件是否由当前源文件产生：mtime 不早于源文件。
///
/// 崩溃恢复重跑时，源文件刚合并完，派生文件是上一次尝试留下的，mtime 更晚；
/// 上一场录制留下的同名派生文件则早于本场源文件，必须避开。
fn is_current_derived(target: &Path, src: &Path) -> bool {
    let target_time = std::fs::metadata(target).and_then(|meta| meta.modified());
    let source_time = std::fs::metadata(src).and_then(|meta| meta.modified());
    match (target_time, source_time) {
        (Ok(target_time), Ok(source_time)) => target_time >= source_time,
        _ => false,
    }
}

/// 目标已经存在且能通过校验时直接复用，避免恢复重跑生成重复文件。
async fn reuse_existing(src: &Path, target: &Path, cfg: &PostConfig) -> Option<PostOutcome> {
    if target == src || !target.is_file() || file_size(target) <= 0 {
        return None;
    }
    if !is_current_derived(target, src) {
        return None;
    }
    if validate_media(target).await.is_err() {
        return None;
    }
    let delete_source =
        (cfg.mode == settings::POST_MODE_CONVERT && cfg.delete_source && target != src)
            .then(|| src.to_path_buf());
    Some(PostOutcome {
        path: target.to_path_buf(),
        size: file_size(target),
        source_path: src.to_path_buf(),
        state: STATE_DONE.to_string(),
        error: None,
        delete_source,
    })
}

fn temp_path(src: &Path, ext: &str) -> PathBuf {
    let name = src
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".into());
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    src.with_file_name(format!(".{name}.post-{}.{}", &nonce[..8], ext))
}

/// 自定义命令模板只做分词，不经过 shell。支持单/双引号与反斜杠转义。
pub fn parse_command(template: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;
    for ch in template.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        match quote {
            Some(q) if ch == q => {
                quote = None;
                started = true;
            }
            Some(_) if ch == '\\' => escaped = true,
            Some(_) => {
                current.push(ch);
                started = true;
            }
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch == '\\' => {
                escaped = true;
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("引号没有闭合".into());
    }
    if escaped {
        return Err("结尾的反斜杠没有转义对象".into());
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

pub fn validate_custom_command(template: &str) -> Result<(), String> {
    let template = template.trim();
    if template.is_empty() {
        return Err("命令不能为空".into());
    }
    if !template.contains("{input}") || !template.contains("{output}") {
        return Err("命令必须同时包含 {input} 与 {output}".into());
    }
    let tokens = parse_command(template)?;
    if tokens.is_empty() {
        return Err("命令不能为空".into());
    }
    for token in &tokens {
        if matches!(token.as_str(), "|" | "||" | "&&" | ";" | ">" | ">>" | "<") {
            return Err("不支持 shell 管道/重定向，请只写 ffmpeg 参数".into());
        }
        if token.contains('$') && !token.contains("{input}") && !token.contains("{output}") {
            return Err("不支持 shell 变量".into());
        }
    }
    Ok(())
}

fn is_ffmpeg_token(token: &str) -> bool {
    let name = Path::new(token)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(token);
    name.eq_ignore_ascii_case("ffmpeg") || name.eq_ignore_ascii_case("ffmpeg.exe")
}

pub fn sanitize_output_ext(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "mp4".into()
    } else {
        cleaned
    }
}

/// ffprobe 校验：至少有一个音/视频流，且容器能读出时长或流信息。
pub async fn validate_media(path: &Path) -> Result<(), String> {
    let probe = ffmpeg::probe_binary();
    if probe.is_some() {
        return validate_with_ffprobe(path, probe.as_deref().unwrap()).await;
    }
    // 没有 ffprobe 时用 ffmpeg 只读前 1 秒，至少能确认容器头和第一条流可用。
    let mut command = Command::new(ffmpeg::binary());
    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-t")
        .arg("1")
        .arg("-map")
        .arg("0")
        .arg("-c")
        .arg("copy")
        .arg("-f")
        .arg("null")
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(VALIDATE_TIMEOUT, command.output())
        .await
        .map_err(|_| "校验超时".to_string())?
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap_or("容器无法读取")
            .to_string())
    }
}

async fn validate_with_ffprobe(path: &Path, probe: &str) -> Result<(), String> {
    let mut command = Command::new(probe);
    command
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration:stream=index")
        .arg("-of")
        .arg("default=noprint_wrappers=1")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(VALIDATE_TIMEOUT, command.output())
        .await
        .map_err(|_| "校验超时".to_string())?
        .map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && text.lines().any(|line| line.starts_with("index=")) {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .last()
        .unwrap_or("没有可用的音视频流")
        .to_string())
}

/// 把文件移入系统回收站。失败时返回错误，调用方保留源文件。
pub fn trash(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or_else(|| "找不到 HOME".to_string())?;
        move_into(&Path::new(&home).join(".Trash"), path)
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile('{}','OnlyErrorDialogs','SendToRecycleBin')",
            path.display().to_string().replace('\'', "''")
        );
        let output = std::process::Command::new("powershell")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(script)
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
            .ok_or_else(|| "找不到回收站目录".to_string())?;
        move_into(data.join("Trash/files"), path)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Err("当前系统不支持回收站".into())
    }
}

#[cfg(unix)]
fn move_into(dir: &Path, path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("创建回收站失败：{err}"))?;
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "recording".into());
    let mut target = dir.join(&name);
    let mut index = 1;
    while target.exists() {
        target = dir.join(format!("{name}.{index}"));
        index += 1;
    }
    match std::fs::rename(path, &target) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            // 跨卷时 rename 会失败：复制后删除，仍保留「回收站」语义。
            std::fs::copy(path, &target).map_err(|err| format!("复制到回收站失败：{err}"))?;
            std::fs::remove_file(path)
                .map_err(|err| format!("移入回收站后删除源失败：{err}（{rename_err}）"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_mapping_covers_flv_hevc_and_audio() {
        assert_eq!(
            source_extension(StreamFormat::Flv, Some(Codec::H264)),
            "flv"
        );
        assert_eq!(
            source_extension(StreamFormat::Flv, Some(Codec::H265)),
            "mkv"
        );
        assert_eq!(source_extension(StreamFormat::Flv, Some(Codec::Av1)), "mkv");
        assert_eq!(source_extension(StreamFormat::Hls, Some(Codec::H264)), "ts");
        assert_eq!(
            source_extension(StreamFormat::HttpTs, Some(Codec::H265)),
            "ts"
        );
        assert_eq!(
            source_extension(StreamFormat::Dash, Some(Codec::H264)),
            "mp4"
        );
        assert_eq!(
            source_extension(StreamFormat::AudioOnly, Some(Codec::Aac)),
            "m4a"
        );
        assert_eq!(source_extension(StreamFormat::AudioOnly, None), "mka");
    }

    #[test]
    fn custom_template_requires_both_placeholders() {
        assert!(validate_custom_command("-i {input} -c copy {output}").is_ok());
        assert!(validate_custom_command("-i {input} -c copy out.mp4").is_err());
        assert!(validate_custom_command("-i {input} | cat > {output}").is_err());
    }

    #[test]
    fn shell_words_respect_quotes() {
        let tokens = parse_command(r#"-i "{input}" -metadata title="a b" {output}"#).unwrap();
        assert_eq!(tokens[1], "{input}");
        assert_eq!(tokens[3], "title=a b");
        assert_eq!(tokens[4], "{output}");
    }

    #[test]
    fn target_never_collides_with_source_when_ext_matches() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        std::fs::write(&src, b"recording").unwrap();

        let target = target_for(&src, "flv");
        assert_eq!(target, dir.join("live.1.flv"));
        assert!(!target.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_derived_file_from_previous_recording_is_skipped() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        let stale = dir.join("live.mp4");
        std::fs::write(&src, b"new recording").unwrap();
        std::fs::write(&stale, b"previous recording").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let target = target_for(&src, "mp4");
        assert_eq!(target, dir.join("live.1.mp4"));
        assert!(!target.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_ext_is_sanitized() {
        assert_eq!(sanitize_output_ext(".MP4"), "mp4");
        assert_eq!(sanitize_output_ext("m kv"), "mkv");
        assert_eq!(sanitize_output_ext(""), "mp4");
    }

    async fn make_test_flv(path: &Path) {
        let status = Command::new(ffmpeg::binary())
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
                "1",
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
            .arg(path)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "生成测试片失败");
    }

    /// 真跑 ffmpeg 的后处理冒烟：source 校验、convert 转封装、custom 模板都要产出可读文件。
    #[tokio::test]
    #[ignore = "需要本机 ffmpeg"]
    async fn real_ffmpeg_source_convert_and_custom_are_playable() {
        let dir = std::env::temp_dir().join(format!("hivecast-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("live.flv");
        make_test_flv(&src).await;

        let source = run(&src, &PostConfig::default()).await;
        assert_eq!(source.state, STATE_DONE, "{:?}", source.error);
        assert_eq!(source.path, src);

        let convert = run(
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CONVERT.into(),
                convert_format: "mp4".into(),
                ..PostConfig::default()
            },
        )
        .await;
        assert_eq!(convert.state, STATE_DONE, "{:?}", convert.error);
        assert_eq!(convert.path, dir.join("live.mp4"));
        assert!(convert.size > 0);

        let custom = run(
            &src,
            &PostConfig {
                mode: settings::POST_MODE_CUSTOM.into(),
                custom_command: "-i {input} -map 0 -c copy {output}".into(),
                custom_output_ext: "mkv".into(),
                ..PostConfig::default()
            },
        )
        .await;
        assert_eq!(custom.state, STATE_DONE, "{:?}", custom.error);
        assert_eq!(custom.path, dir.join("live.mkv"));
        assert!(custom.size > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
