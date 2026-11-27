#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod install;
mod payload;

#[cfg(windows)]
mod gui;
#[cfg(windows)]
mod win_os;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::install::CliAction;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let action = match install::parse_cli(&args) {
        Ok(action) => action,
        Err(err) => {
            eprintln!("{err}");
            process::exit(2);
        }
    };

    let result = match action {
        CliAction::Pack {
            stub,
            app_exe,
            version,
            out_setup,
        } => run_pack(&stub, &app_exe, &version, &out_setup),
        CliAction::SelfTest => run_self_test(),
        CliAction::SilentInstall {
            dir,
            desktop_shortcut,
        } => {
            let raw_dir = dir
                .unwrap_or_else(preferred_install_dir)
                .display()
                .to_string();
            perform_install(&raw_dir, desktop_shortcut, |_, _| {})
                .map(|_| ())
                .map_err(InstallError::into_message)
        }
        CliAction::SilentUninstall { dir, purge_data } => {
            resolve_uninstall_dir(dir).and_then(|target| perform_uninstall(&target, purge_data))
        }
        CliAction::GuiInstall => launch_gui(false),
        CliAction::GuiUninstall => launch_gui(true),
    };

    if let Err(err) = result {
        eprintln!("Error: {err}");
        process::exit(1);
    }
}

fn run_pack(stub: &Path, app_exe: &Path, version: &str, out_setup: &Path) -> Result<(), String> {
    let meta = payload::pack_installer(stub, app_exe, version, "HIVECAST.exe", out_setup)
        .map_err(|e| format!("打包安装器失败：{e}"))?;
    println!(
        "packed {} (v{}, raw={} bytes, sha256={})",
        out_setup.display(),
        meta.version,
        meta.raw_size,
        meta.sha256
    );
    Ok(())
}

fn run_self_test() -> Result<(), String> {
    let self_exe = env::current_exe().map_err(|e| format!("无法获取自身路径：{e}"))?;
    let meta =
        payload::verify_embedded_payload(&self_exe).map_err(|e| format!("安装包自检失败：{e}"))?;
    println!(
        "self-test ok: v{} {} ({} bytes, sha256={})",
        meta.version, meta.exe_name, meta.raw_size, meta.sha256
    );
    Ok(())
}

fn preferred_install_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(path) = win_os::read_installed_dir() {
        return path;
    }
    install::default_install_dir()
}

fn resolve_uninstall_dir(cli_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    if exe
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("uninstall.exe"))
    {
        let parent = exe.parent().ok_or("无法定位卸载程序目录")?;
        if cli_dir
            .as_ref()
            .is_some_and(|dir| !same_location(dir, parent))
        {
            return Err("卸载目标与卸载程序所在目录不一致".into());
        }
        return Ok(parent.to_owned());
    }
    Ok(cli_dir.unwrap_or_else(preferred_install_dir))
}

fn same_location(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a)
        .ok()
        .zip(fs::canonicalize(b).ok())
        .is_some_and(|(a, b)| a == b)
}

// Keep original files on their own volume until registry and shortcut changes have succeeded.
struct Replacement {
    dest: PathBuf,
    previous: Option<tempfile::TempDir>,
}

fn replace_file(source: &Path, dest: &Path, changes: &mut Vec<Replacement>) -> Result<(), String> {
    let parent = dest.parent().ok_or("目标路径缺少上级目录")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let next = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    fs::copy(source, next.path()).map_err(|e| e.to_string())?;
    let previous = if dest.try_exists().map_err(|e| e.to_string())? {
        if !dest.is_file() {
            return Err(format!("目标不是文件：{}", dest.display()));
        }
        let backup = tempfile::tempdir_in(parent).map_err(|e| e.to_string())?;
        fs::rename(dest, backup.path().join("original"))
            .map_err(|e| format!("备份 {} 失败：{e}", dest.display()))?;
        Some(backup)
    } else {
        None
    };
    let result = next.persist_noclobber(dest);
    if previous.is_some() || result.is_ok() {
        changes.push(Replacement {
            dest: dest.to_owned(),
            previous,
        });
    }
    result.map_err(|e| format!("写入 {} 失败：{e}", dest.display()))?;
    Ok(())
}

fn rollback(changes: Vec<Replacement>, cause: String) -> String {
    let mut errors = vec![cause];
    for change in changes.into_iter().rev() {
        let result = match change.previous {
            Some(backup) => fs::rename(backup.path().join("original"), &change.dest).map_err(|e| {
                format!(
                    "恢复 {} 失败：{e}，原文件备份在 {}",
                    change.dest.display(),
                    backup.keep().display()
                )
            }),
            None => fs::remove_file(&change.dest)
                .or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| e.to_string()),
        };
        if let Err(e) = result {
            errors.push(e);
        }
    }
    errors.join("；")
}

#[derive(serde::Serialize)]
#[serde(tag = "type", content = "message")]
pub(crate) enum InstallError {
    #[serde(rename = "installDirError")]
    Directory(String),
    #[serde(rename = "error")]
    Other(String),
}

impl InstallError {
    fn into_message(self) -> String {
        match self {
            Self::Directory(message) | Self::Other(message) => message,
        }
    }
}

impl From<String> for InstallError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

pub(crate) fn perform_install<F>(
    raw_dir: &str,
    desktop_shortcut: bool,
    mut on_progress: F,
) -> Result<(PathBuf, PathBuf), InstallError>
where
    F: FnMut(u8, u8),
{
    let target = install::validate_install_dir(raw_dir).map_err(InstallError::Directory)?;
    let install_dir = target.as_path();
    #[cfg(windows)]
    if win_os::is_app_running() {
        return Err(InstallError::Other(
            "检测到 HIVECAST 正在运行，请先关闭应用后再安装或升级。".into(),
        ));
    }

    #[cfg(windows)]
    let _lock = win_os::operation_lock()?;
    #[cfg(windows)]
    {
        let registered = win_os::read_installed_dir();
        let dir_exists = install_dir
            .try_exists()
            .map_err(|e| InstallError::Directory(e.to_string()))?;
        let owned = dir_exists
            && registered.as_ref().is_some_and(|dir| {
                same_location(dir, install_dir) && install::has_existing_install(dir)
            });
        if !owned {
            if registered
                .as_ref()
                .is_some_and(|dir| install::has_existing_install(dir))
            {
                return Err(InstallError::Directory(
                    "请使用已登记的安装目录升级，或先卸载原版本".into(),
                ));
            }
            if dir_exists {
                if !install_dir.is_dir() {
                    return Err(InstallError::Directory("所选路径不是有效文件夹".into()));
                }
                if fs::read_dir(install_dir)
                    .map_err(|e| InstallError::Directory(e.to_string()))?
                    .next()
                    .is_some()
                    && !install::is_reusable_preserved_dir(install_dir)
                {
                    return Err(InstallError::Directory(
                        "目录非空，请选择新的 HIVECAST 专用子目录".into(),
                    ));
                }
            }
        }
    }
    fs::create_dir_all(install_dir).map_err(|e| InstallError::Directory(e.to_string()))?;
    let staging =
        tempfile::tempdir_in(install_dir).map_err(|e| InstallError::Directory(e.to_string()))?;
    let self_exe = env::current_exe().map_err(|e| format!("无法获取安装程序路径：{e}"))?;
    let located = payload::locate_payload(&self_exe)
        .map_err(|e| format!("读取安装包结构失败：{e}"))?
        .ok_or_else(|| "当前程序不包含内嵌载荷，请下载正式安装包。".to_string())?;

    on_progress(5, 0);
    let dest_exe = install_dir.join("HIVECAST.exe");
    let staged_app = staging.path().join("HIVECAST.exe");
    payload::extract_verified(&self_exe, &located, &staged_app, |pct| {
        // Map extraction 0..=100 into overall 5..=80%.
        let scaled = 5u8.saturating_add(((u16::from(pct) * 75) / 100) as u8);
        on_progress(scaled, 0);
    })
    .map_err(|e| format!("释放主程序失败：{e}"))?;

    on_progress(84, 1);
    let uninstall_exe = install_dir.join("uninstall.exe");
    let staged_uninstaller = staging.path().join("uninstall.exe");
    payload::copy_clean_uninstaller(&self_exe, &located, &staged_uninstaller)
        .map_err(|e| format!("写入卸载程序失败：{e}"))?;

    let mut changes = Vec::new();
    let result = (|| {
        #[cfg(windows)]
        let links = win_os::stage_shortcut(&dest_exe, desktop_shortcut, staging.path())?;
        replace_file(&staged_app, &dest_exe, &mut changes)?;
        replace_file(&staged_uninstaller, &uninstall_exe, &mut changes)?;
        #[cfg(windows)]
        {
            for link in links {
                replace_file(&staging.path().join("HIVECAST.lnk"), &link, &mut changes)?;
            }
            on_progress(92, 1);
            let estimated_kb = (located.meta.raw_size / 1024).min(u64::from(u32::MAX)) as u32;
            win_os::write_uninstall_registry(
                install_dir,
                &dest_exe,
                &uninstall_exe,
                &located.meta.version,
                estimated_kb,
            )?;
        }
        #[cfg(not(windows))]
        let _ = desktop_shortcut;
        Ok(())
    })();
    if let Err(err) = result {
        return Err(rollback(changes, err).into());
    }

    on_progress(100, 2);
    Ok((target, dest_exe))
}

fn remove_real_dir_if_exists(dir: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => {
            let mut last_err = None;
            for attempt in 0..20 {
                match fs::remove_dir_all(dir) {
                    Ok(()) => return Ok(true),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                    Err(err) => {
                        last_err = Some(err);
                        if attempt < 19 {
                            std::thread::sleep(std::time::Duration::from_millis(150));
                        }
                    }
                }
            }
            Err(format!(
                "清理目录 {} 失败：{}",
                dir.display(),
                last_err.map_or_else(|| "未知错误".to_string(), |e| e.to_string())
            ))
        }
        Ok(_) => Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("检查目录 {} 失败：{err}", dir.display())),
    }
}

pub(crate) fn perform_uninstall(install_dir: &Path, purge_data: bool) -> Result<(), String> {
    let app_exe = install_dir.join("HIVECAST.exe");
    let uninstaller = install_dir.join("uninstall.exe");
    #[cfg(windows)]
    let _lock = win_os::operation_lock()?;
    #[cfg(windows)]
    {
        if !win_os::read_installed_dir()
            .is_some_and(|registered| same_location(&registered, install_dir))
        {
            return Err("卸载目录与当前安装登记不符".into());
        }
        if win_os::is_app_running() {
            return Err("检测到 HIVECAST 正在运行，请先关闭应用后再卸载。".into());
        }
        win_os::remove_shortcuts(&app_exe)?;
    }
    if app_exe.exists() {
        fs::remove_file(&app_exe).map_err(|e| format!("删除主程序失败：{e}"))?;
    }
    for ephemeral in ["cache", "webview", "logs"] {
        let _ = remove_real_dir_if_exists(&install_dir.join(ephemeral))?;
    }
    if purge_data {
        let removed_local_data = remove_real_dir_if_exists(&install_dir.join("data"))?;
        #[cfg(windows)]
        if !removed_local_data {
            if let Ok(appdata) = env::var("APPDATA") {
                let fallback_data = PathBuf::from(appdata).join("com.hivecast.desktop");
                let _ = remove_real_dir_if_exists(&fallback_data)?;
            }
        }
        #[cfg(not(windows))]
        let _ = removed_local_data;
    }
    #[cfg(windows)]
    {
        let exe = env::current_exe().map_err(|e| e.to_string())?;
        if same_location(&exe, &uninstaller) {
            win_os::delete_uninstall_registry()?;
            return win_os::schedule_self_cleanup(&exe, install_dir);
        }
    }
    if uninstaller.exists() {
        fs::remove_file(uninstaller).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    win_os::delete_uninstall_registry()?;
    if fs::read_dir(install_dir)
        .map_err(|e| e.to_string())?
        .next()
        .is_none()
    {
        fs::remove_dir(install_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn launch_gui(uninstall: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let self_exe = env::current_exe().map_err(|e| format!("无法获取自身路径：{e}"))?;
        let version = payload::locate_payload(&self_exe)
            .ok()
            .flatten()
            .map(|l| l.meta.version)
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let mode = if uninstall {
            gui::GuiMode::Uninstall
        } else {
            gui::GuiMode::Install
        };
        gui::run(mode, version)
    }
    #[cfg(not(windows))]
    {
        let _ = uninstall;
        Err("GUI 安装界面仅支持 Windows 运行；非 Windows 环境请使用 --pack / --self-test / --silent。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_directory_reports_a_field_error_before_installation_starts() {
        for raw in ["relative", r"C:\", r"C:\Windows", ""] {
            let error = perform_install(raw, false, |_, _| panic!("unexpected install progress"))
                .unwrap_err();
            let event = serde_json::json!(error);
            assert_eq!(event["type"], "installDirError");
            assert_eq!(
                event["message"],
                install::validate_install_dir(raw).unwrap_err()
            );
        }
    }
}
