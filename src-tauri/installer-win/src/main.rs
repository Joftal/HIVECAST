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
                .unwrap_or_else(install::default_install_dir)
                .display()
                .to_string();
            install::validate_install_dir(&raw_dir).and_then(|target| {
                perform_install(&target, desktop_shortcut, |_, _| {}).map(|_| ())
            })
        }
        CliAction::SilentUninstall { dir } => {
            let target = resolve_uninstall_dir(dir);
            perform_uninstall(&target, true)
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

fn resolve_uninstall_dir(cli_dir: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = cli_dir {
        return dir;
    }
    #[cfg(windows)]
    if let Some(reg_dir) = win_os::read_installed_dir() {
        return reg_dir;
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.to_path_buf();
        }
    }
    install::default_install_dir()
}

pub(crate) fn perform_install<F>(
    install_dir: &Path,
    desktop_shortcut: bool,
    mut on_progress: F,
) -> Result<PathBuf, String>
where
    F: FnMut(u8, u8),
{
    #[cfg(windows)]
    if win_os::is_app_running() {
        return Err("检测到 HIVECAST 正在运行，请先关闭应用后再安装或升级。".into());
    }

    let self_exe = env::current_exe().map_err(|e| format!("无法获取安装程序路径：{e}"))?;
    let located = payload::locate_payload(&self_exe)
        .map_err(|e| format!("读取安装包结构失败：{e}"))?
        .ok_or_else(|| "当前程序不包含内嵌载荷，请下载正式安装包。".to_string())?;

    on_progress(5, 0);
    let dest_exe = install_dir.join(&located.meta.exe_name);
    payload::extract_verified(&self_exe, &located, &dest_exe, |pct| {
        // Map extraction 0..=100 into overall 5..=80%.
        let scaled = 5u8.saturating_add(((u16::from(pct) * 75) / 100) as u8);
        on_progress(scaled, 0);
    })
    .map_err(|e| format!("释放主程序失败：{e}"))?;

    on_progress(84, 1);
    let uninstall_exe = install_dir.join("uninstall.exe");
    payload::copy_clean_uninstaller(&self_exe, &located, &uninstall_exe)
        .map_err(|e| format!("写入卸载程序失败：{e}"))?;

    #[cfg(windows)]
    {
        win_os::create_shortcuts(install_dir, &dest_exe, desktop_shortcut)?;
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
    {
        let _ = desktop_shortcut;
    }

    on_progress(100, 2);
    Ok(dest_exe)
}

pub(crate) fn perform_uninstall(
    install_dir: &Path,
    immediate_self_cleanup: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    if win_os::is_app_running() {
        return Err("检测到 HIVECAST 正在运行，请先关闭应用后再卸载。".into());
    }

    let app_exe = install_dir.join("HIVECAST.exe");
    if app_exe.exists() {
        fs::remove_file(&app_exe).map_err(|e| format!("删除主程序失败：{e}"))?;
    }

    #[cfg(windows)]
    {
        win_os::remove_shortcuts();
        win_os::delete_uninstall_registry();
        if let Ok(self_exe) = env::current_exe() {
            if self_exe.starts_with(install_dir) {
                if immediate_self_cleanup {
                    win_os::schedule_self_cleanup(&self_exe, install_dir);
                }
            } else {
                let uninst = install_dir.join("uninstall.exe");
                if uninst.exists() {
                    let _ = fs::remove_file(&uninst);
                }
                let _ = fs::remove_dir(install_dir);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = immediate_self_cleanup;
        let uninst = install_dir.join("uninstall.exe");
        if uninst.exists() {
            let _ = fs::remove_file(&uninst);
        }
        let _ = fs::remove_dir(install_dir);
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
