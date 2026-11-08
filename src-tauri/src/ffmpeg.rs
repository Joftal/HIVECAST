use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, RwLock};

use hivecast_ipc::FfmpegStatus;

/// 用户在设置里显式指定的 ffmpeg。启动时先于后处理恢复写入，
/// 保存设置时也会立刻更新，所有既有调用点无需感知。
static CONFIGURED: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn configured_slot() -> &'static RwLock<Option<PathBuf>> {
    CONFIGURED.get_or_init(|| RwLock::new(None))
}

/// 设置页保存的手动路径；传 None 恢复自动检测。
///
/// 这里不因为配置失效就清空它：设置页仍要显示用户填了什么，
/// 运行时由 `resolve` 回退到 PATH / 常见安装位置。
pub fn set_configured_path(path: Option<PathBuf>) {
    let normalized = path.map(|path| selection_candidate(&path));
    if let Some(candidate) = normalized.as_ref() {
        if is_runnable(candidate) {
            tracing::info!(path = %candidate.display(), "使用手动指定的 ffmpeg");
        } else {
            tracing::warn!(
                path = %candidate.display(),
                "手动指定的 ffmpeg 不可用，将回退自动检测"
            );
        }
    }
    let mut slot = configured_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = normalized;
}

/// 手动配置的路径（已归一化为可执行文件，不是目录）。
pub fn configured_path() -> Option<PathBuf> {
    configured_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// 在「手动配置 > PATH > 常见安装位置」中寻找可执行的 ffmpeg。
///
/// macOS 上从 Finder 启动的 GUI 进程，PATH 通常只有
/// `/usr/bin:/bin:/usr/sbin:/sbin`，Homebrew 的 `/opt/homebrew/bin`
/// 并不在其中，所以这里显式补几个候选目录，避免打包后「找不到 ffmpeg」。
pub fn resolve() -> Option<PathBuf> {
    resolve_with_source().map(|(path, _)| path)
}

/// 返回实际路径与来源：manual | path | candidate。
pub fn resolve_with_source() -> Option<(PathBuf, &'static str)> {
    if let Some(path) = configured_path() {
        if is_runnable(&path) {
            return Some((path, "manual"));
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe_name());
            if is_runnable(&candidate) {
                return Some((candidate, "path"));
            }
        }
    }

    for dir in extra_dirs() {
        let candidate = dir.join(exe_name());
        if is_runnable(&candidate) {
            return Some((candidate, "candidate"));
        }
    }
    None
}

/// ffprobe 通常与 ffmpeg 同目录；找不到时后处理会退回 ffmpeg 校验。
pub fn probe_binary() -> Option<String> {
    let ffmpeg = resolve()?;
    let name = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    let probe = ffmpeg.with_file_name(name);
    is_runnable(&probe).then(|| probe.display().to_string())
}

/// 供前端展示的绝对路径。
pub fn path_string() -> Option<String> {
    resolve().map(|path| path.display().to_string())
}

/// 设置页的状态行：当前实际路径、来源、版本，以及用户配置的原始路径。
pub fn status() -> FfmpegStatus {
    let configured = configured_path()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    match resolve_with_source() {
        Some((path, source)) => FfmpegStatus {
            configured,
            version: version_of(&path),
            path: Some(path.display().to_string()),
            source: source.into(),
        },
        None => FfmpegStatus {
            configured,
            path: None,
            source: "missing".into(),
            version: None,
        },
    }
}

/// 真正启动 ffmpeg 时用这个：解析成功就用绝对路径，否则退回裸名由 PATH 决定。
pub fn binary() -> String {
    match resolve() {
        Some(path) => {
            tracing::debug!(path = %path.display(), "使用 ffmpeg 可执行文件");
            path.display().to_string()
        }
        None => {
            tracing::warn!("未找到 ffmpeg 可执行文件，将直接按 PATH 调用");
            exe_name().to_string()
        }
    }
}

/// 校验用户在设置里填的路径：允许选目录（自动补 ffmpeg 文件名），
/// 但必须是绝对路径、真实存在的文件，而且真的能跑 `-version`。
pub fn validate_selection(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute() {
        return Err("请填写 ffmpeg 的绝对路径".into());
    }
    let candidate = selection_candidate(&path);
    if !candidate.exists() {
        return Err(format!("{} 不存在", candidate.display()));
    }
    if !candidate.is_file() {
        return Err(format!("{} 不是文件", candidate.display()));
    }
    let output = Command::new(&candidate)
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("无法启动 {}：{err}", candidate.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.lines().next().unwrap_or("").trim();
        return Err(if detail.is_empty() {
            format!("{} 无法运行 -version", candidate.display())
        } else {
            format!("{} 不是可用的 ffmpeg：{detail}", candidate.display())
        });
    }
    // 不 canonicalize：Homebrew 的 /opt/homebrew/bin/ffmpeg 是符号链接，
    // 解析到 Cellar/版本号后，brew upgrade 会让配置直接失效。保留用户选的稳定入口。
    Ok(candidate)
}

/// 用户可能选到目录，也可能直接选可执行文件；统一归一化成文件路径。
fn selection_candidate(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(exe_name())
    } else {
        path.to_path_buf()
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn extra_dirs() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ]
    } else if cfg!(windows) {
        vec![
            PathBuf::from(r"C:\ffmpeg\bin"),
            PathBuf::from(r"C:\Program Files\ffmpeg\bin"),
        ]
    } else {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")]
    }
}

fn version_of(path: &Path) -> Option<String> {
    let output = Command::new(path)
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(160).collect())
    }
}

fn is_runnable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    Command::new(path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_keeps_executable_and_expands_directory() {
        let dir = std::env::temp_dir();
        assert_eq!(selection_candidate(&dir), dir.join(exe_name()));
        let file = dir.join(exe_name());
        assert_eq!(selection_candidate(&file), file);
    }

    #[test]
    fn relative_paths_are_rejected() {
        assert!(validate_selection("ffmpeg").is_err());
        assert!(validate_selection("").is_err());
    }
}
