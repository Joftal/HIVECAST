use std::path::{Path, PathBuf};
use std::process::Stdio;

/// 在 PATH 与常见安装位置中寻找可执行的 ffmpeg。
///
/// macOS 上从 Finder 启动的 GUI 进程，PATH 通常只有
/// `/usr/bin:/bin:/usr/sbin:/sbin`，Homebrew 的 `/opt/homebrew/bin`
/// 并不在其中，所以这里显式补几个候选目录，避免打包后「找不到 ffmpeg」。
pub fn resolve() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(path_var) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path_var).map(|dir| dir.join(exe_name())));
    }
    candidates.extend(extra_dirs().into_iter().map(|dir| dir.join(exe_name())));

    candidates.into_iter().find(|candidate| is_runnable(candidate))
}

/// 供前端展示的绝对路径。
pub fn path_string() -> Option<String> {
    resolve().map(|path| path.display().to_string())
}

/// 真正启动 ffmpeg 时用这个：解析成功就用绝对路径，否则退回裸名由 PATH 决定。
pub fn binary() -> String {
    path_string().unwrap_or_else(|| exe_name().to_string())
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

fn is_runnable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    std::process::Command::new(path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
