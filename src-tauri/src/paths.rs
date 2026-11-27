//! 运行期路径解析。
//!
//! - Windows 正式版（release）采用「安装目录优先，不可写或网络盘时回落」：
//!   可写时将 `data/`、`cache/`、`logs/`、`webview/`、`Recording/` 统一放在程序安装目录下；
//!   不可写（如 `Program Files`）或位于 UNC 网络路径时自动回落到系统 `%APPDATA%` / `%LOCALAPPDATA%` / `%USERPROFILE%\Videos\HIVECAST`。
//! - macOS 保持系统标准目录（`~/Library/...`），默认录像目录固定为 `~/Movies/HIVECAST`（不再落入 `/Applications/Recording`）。
//! - `HIVECAST_DATA_DIR`、`HIVECAST_CACHE_DIR`、`HIVECAST_LOG_DIR`、`HIVECAST_MEDIA_DIR`、`HIVECAST_WEBVIEW_DIR`
//!   环境变量始终优先，便于自动化测试与高级用户自定义。

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::sync::OnceLock;

use tauri::{AppHandle, Manager};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    #[allow(dead_code)]
    pub log_dir: PathBuf,
    /// 仅 Windows 安装目录优先模式启用，用于将 WebView2 数据目录放到 `<install_dir>\webview`。
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub webview_data_dir: Option<PathBuf>,
    /// 本次是否命中 Windows 安装目录优先模式。
    pub portable_install_dir: bool,
}

/// 进程内只探测一次：仅 Windows 非 debug 构建启用「安装目录优先」。
pub fn portable_root() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if cfg!(debug_assertions) {
            return None;
        }
        static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
        ROOT.get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = strip_windows_verbatim(exe.parent()?);
            if is_remote_volume(&dir) {
                return None;
            }
            if ensure_writable(&dir) {
                Some(dir)
            } else {
                None
            }
        })
        .clone()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 根据当前平台解析运行期路径。
pub fn resolve(app: &AppHandle) -> Result<AppPaths, String> {
    let system_data = app.path().app_data_dir().map_err(|err| err.to_string())?;
    let system_cache = app.path().app_cache_dir().map_err(|err| err.to_string())?;

    let env_data = env_path("HIVECAST_DATA_DIR");
    let env_cache = env_path("HIVECAST_CACHE_DIR");
    let env_webview = env_path("HIVECAST_WEBVIEW_DIR");
    let log_dir = crate::logging::default_dir();

    if let Some(root) = portable_root() {
        return Ok(AppPaths {
            data_dir: match env_data {
                Some(dir) => dir,
                None => select_profile(&root.join("data"), &system_data)?,
            },
            cache_dir: env_cache.unwrap_or_else(|| root.join("cache")),
            log_dir,
            webview_data_dir: Some(env_webview.unwrap_or_else(|| root.join("webview"))),
            portable_install_dir: true,
        });
    }

    Ok(AppPaths {
        data_dir: env_data.unwrap_or(system_data),
        cache_dir: env_cache.unwrap_or(system_cache),
        log_dir,
        webview_data_dir: env_webview,
        portable_install_dir: false,
    })
}

// An existing portable profile travels with the executable. Otherwise an upgrade
// continues using the system profile and its matching encryption key.
fn profile_has_user_data(file: &Path) -> Option<bool> {
    use rusqlite::{Connection, OpenFlags};
    let db = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.query_row(
        "SELECT EXISTS (SELECT 1 FROM room UNION ALL SELECT 1 FROM credential
        UNION ALL SELECT 1 FROM record_session UNION ALL SELECT 1 FROM task_history
        UNION ALL SELECT 1 FROM douyin_follow UNION ALL SELECT 1 FROM app_setting
        WHERE key NOT IN ('ui.window_state','db.task_event_fts_v1'))",
        [],
        |r| r.get(0),
    )
    .ok()
}

fn select_profile(local: &Path, system: &Path) -> Result<PathBuf, String> {
    for dir in [local, system] {
        if dir.is_file() {
            continue;
        }
        let db = dir.join("hivecast.db");
        if db
            .try_exists()
            .map_err(|e| format!("{}: {e}", db.display()))?
        {
            if dir == local
                && system.join("hivecast.db").is_file()
                && profile_has_user_data(&db) == Some(false)
                && profile_has_user_data(&system.join("hivecast.db")) == Some(true)
            {
                // An older release may already have bootstrapped an unused empty
                // portable profile over a populated legacy one. Keep both files.
                return Ok(system.to_owned());
            }
            return Ok(dir.to_owned());
        }
    }
    Ok(if ensure_writable(local) {
        local
    } else {
        system
    }
    .to_owned())
}

#[cfg(windows)]
fn is_remote_volume(dir: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    if is_unc_path(dir) {
        return true;
    }
    let Some(root) = dir.ancestors().last() else {
        return true;
    };
    let wide: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    // DRIVE_UNKNOWN, DRIVE_NO_ROOT_DIR and DRIVE_REMOTE cannot host a portable WAL.
    matches!(unsafe { GetDriveTypeW(wide.as_ptr()) }, 0 | 1 | 4)
}

#[cfg(any(windows, test))]
pub(crate) fn relocated(path: &Path, old_root: &Path, root: &Path) -> Option<PathBuf> {
    let path = strip_windows_verbatim(path);
    let mut parts = path.components();
    for expected in strip_windows_verbatim(old_root).components() {
        let actual = parts.next()?;
        if !actual
            .as_os_str()
            .eq_ignore_ascii_case(expected.as_os_str())
        {
            return None;
        }
    }
    let tail = parts.as_path();
    tail.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
        .then(|| root.join(tail))
}

pub(crate) fn path_key(path: &Path) -> PathBuf {
    let path = strip_windows_verbatim(path);
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path
    }
}

pub(crate) fn overlap(a: &Path, b: &Path) -> bool {
    let key = |path: &Path| path_key(&path.canonicalize().unwrap_or_else(|_| path.to_owned()));
    let (a, b) = (key(a), key(b));
    a.starts_with(&b) || b.starts_with(&a)
}

fn env_path(name: &str) -> Option<PathBuf> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(std::path::absolute(trimmed).unwrap_or_else(|_| PathBuf::from(trimmed)))
}

/// 判断是否是 UNC 网络共享路径（SQLite WAL 在 SMB/UNC 上容易损坏）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn is_unc_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    let upper = raw.to_ascii_uppercase();
    if upper.starts_with(r"\\?\UNC\") || upper.starts_with(r"\\.\UNC\") {
        return true;
    }
    if raw.starts_with(r"\\?\") || raw.starts_with(r"\\.\") {
        return false;
    }
    raw.starts_with(r"\\") || raw.starts_with("//")
}

/// 去掉 Windows `\\?\C:\...` 逐字前缀，避免 UI 显示和 WebView2 COM 异常。
pub(crate) fn strip_windows_verbatim(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\") {
        if rest.len() >= 2 && rest.as_bytes()[0].is_ascii_alphabetic() && rest.as_bytes()[1] == b':'
        {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// 目录能建出来、也能真的写进去一个文件。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn ensure_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    // 唯一文件名 + create_new：不能截断用户目录里恰好同名的文件，
    // 也不能跟随已存在的符号链接。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe = dir.join(format!(
        ".hivecast-write-test-{}-{stamp}",
        std::process::id()
    ));
    let Ok(file) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    else {
        return false;
    };
    drop(file);
    let _ = fs::remove_file(&probe);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_probe_never_touches_an_existing_fixed_name() {
        let dir = std::env::temp_dir().join(format!("hivecast-probe-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join(".hivecast-write-test");
        fs::write(&legacy, b"do not touch").unwrap();

        assert!(ensure_writable(&dir));
        assert_eq!(fs::read(&legacy).unwrap(), b"do not touch");
        let leaked: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy() != ".hivecast-write-test")
            .collect();
        assert!(leaked.is_empty(), "探测文件必须删除");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unc_path_detection_handles_shares_and_local_verbatim_paths() {
        assert!(is_unc_path(Path::new(r"\\nas\share\HIVECAST")));
        assert!(is_unc_path(Path::new(r"\\?\UNC\nas\share\HIVECAST")));
        assert!(is_unc_path(Path::new("//nas/share/HIVECAST")));
        assert!(!is_unc_path(Path::new(r"D:\Apps\HIVECAST")));
        assert!(!is_unc_path(Path::new(r"\\?\D:\Apps\HIVECAST")));
        assert_eq!(
            strip_windows_verbatim(Path::new(r"\\?\D:\Apps\HIVECAST")),
            PathBuf::from(r"D:\Apps\HIVECAST")
        );
    }
    #[test]
    fn existing_profiles_and_unusable_portable_directory() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("data");
        let system = dir.path().join("roaming");
        std::fs::write(&local, b"not a directory").unwrap();
        assert_eq!(select_profile(&local, &system).unwrap(), system);
        std::fs::remove_file(&local).unwrap();
        std::fs::create_dir(&system).unwrap();
        std::fs::write(system.join("hivecast.db"), b"existing").unwrap();
        assert_eq!(select_profile(&local, &system).unwrap(), system);
        assert!(!local.exists());
        std::fs::create_dir(&local).unwrap();
        std::fs::write(local.join("hivecast.db"), b"portable").unwrap();
        assert_eq!(select_profile(&local, &system).unwrap(), local);
    }
    #[test]
    fn upgrade_recovers_an_already_bootstrapped_empty_profile() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("data");
        let system = root.path().join("system");
        for dir in [&local, &system] {
            std::fs::create_dir(dir).unwrap();
            drop(crate::db::Db::open(&dir.join("hivecast.db")).unwrap());
        }
        let db = crate::db::Db::open(&system.join("hivecast.db")).unwrap();
        db.set_setting("ui.language", "en-US").unwrap();
        assert_eq!(select_profile(&local, &system).unwrap(), system);
        crate::db::Db::open(&local.join("hivecast.db"))
            .unwrap()
            .set_setting("ui.theme", "dark")
            .unwrap();
        assert_eq!(select_profile(&local, &system).unwrap(), local);
    }
}
