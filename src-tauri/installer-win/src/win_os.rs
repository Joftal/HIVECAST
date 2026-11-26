//! Windows-specific OS integration (DWM styling, WebView2 check, shortcuts, Uninstall registry).

use std::env;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD, REG_SZ,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_YESNO, SW_SHOWNORMAL,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const UNINSTALL_REG_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\HIVECAST";
const WEBVIEW2_DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Apply Windows 11 rounded corners, Mica backdrop hint, and light/dark frame preference.
/// Safely ignored on Windows 10 where newer DWM attributes return an error HRESULT.
pub fn apply_window_material(hwnd: isize, dark: bool) {
    let hwnd = hwnd as HWND;
    let round: i32 = DWMWCP_ROUND;
    let dark_val: i32 = i32::from(dark);
    let backdrop: i32 = DWMSBT_MAINWINDOW;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&raw const round).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&raw const dark_val).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&raw const backdrop).cast(),
            std::mem::size_of::<i32>() as u32,
        );
    }
}

pub fn is_system_zh() -> bool {
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    (lang_id & 0x03ff) == 0x04
}

pub fn is_system_dark() -> bool {
    read_reg_dword(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .is_some_and(|v| v == 0)
}

pub fn is_webview2_installed() -> bool {
    const KEYS: [&str; 2] = [
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
    ];
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for subkey in KEYS {
            if let Some(pv) = read_reg_string(root, subkey, "pv") {
                if !pv.trim().is_empty() && pv != "0.0.0.0" {
                    return true;
                }
            }
        }
    }
    false
}

pub fn prompt_webview2_download(zh: bool) {
    let (title, body) = if zh {
        (
            "需要 Microsoft Edge WebView2",
            "HIVECAST 与安装程序需要 Microsoft Edge WebView2 Runtime。\n\n是否现在打开官方下载页面安装？安装完成后请重新运行本安装器。",
        )
    } else {
        (
            "Microsoft Edge WebView2 Required",
            "HIVECAST and its installer require Microsoft Edge WebView2 Runtime.\n\nOpen Microsoft's official download page now? Re-run this installer after installing WebView2.",
        )
    };
    let title_w = to_wide(title);
    let body_w = to_wide(body);
    let choice = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body_w.as_ptr(),
            title_w.as_ptr(),
            MB_YESNO | MB_ICONINFORMATION,
        )
    };
    if choice == IDYES {
        let open_w = to_wide("open");
        let url_w = to_wide(WEBVIEW2_DOWNLOAD_URL);
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                open_w.as_ptr(),
                url_w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

pub fn is_app_running() -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq HIVECAST.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .to_ascii_lowercase()
            .contains("hivecast.exe"),
        Err(_) => false,
    }
}

/// Show native folder picker dialog via STA PowerShell (initial directory passed via env var).
pub fn pick_folder(initial: &Path) -> Option<PathBuf> {
    let script = r#"
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
Add-Type -AssemblyName System.Windows.Forms
$dlg = New-Object System.Windows.Forms.FolderBrowserDialog
$dlg.Description = '选择 HIVECAST 安装目录'
$dlg.ShowNewFolderButton = $true
if ($env:HIVECAST_INIT_DIR -and (Test-Path -LiteralPath $env:HIVECAST_INIT_DIR)) {
  $dlg.SelectedPath = $env:HIVECAST_INIT_DIR
}
if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
  [Console]::Out.Write($dlg.SelectedPath)
}
"#;
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-STA",
            "-Command",
            script,
        ])
        .env("HIVECAST_INIT_DIR", initial)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let chosen = crate::install::sanitize_dir_input(&String::from_utf8_lossy(&output.stdout));
    if chosen.is_empty() {
        None
    } else {
        Some(PathBuf::from(chosen))
    }
}

/// Create Start Menu shortcut and optionally Desktop shortcut via env-var-safe PowerShell COM.
pub fn create_shortcuts(install_dir: &Path, exe_path: &Path, desktop: bool) -> Result<(), String> {
    let script = r#"
$ErrorActionPreference = 'Stop'
$shell = New-Object -ComObject WScript.Shell
$startDir = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
New-Item -ItemType Directory -Force -Path $startDir | Out-Null
$startLnk = $shell.CreateShortcut((Join-Path $startDir 'HIVECAST.lnk'))
$startLnk.TargetPath = $env:HIVECAST_EXE
$startLnk.WorkingDirectory = $env:HIVECAST_DIR
$startLnk.IconLocation = "$env:HIVECAST_EXE,0"
$startLnk.Description = 'HIVECAST Live Recording Studio'
$startLnk.Save()

if ($env:HIVECAST_DESKTOP -eq '1') {
  $deskDir = [Environment]::GetFolderPath('Desktop')
  if ($deskDir) {
    $deskLnk = $shell.CreateShortcut((Join-Path $deskDir 'HIVECAST.lnk'))
    $deskLnk.TargetPath = $env:HIVECAST_EXE
    $deskLnk.WorkingDirectory = $env:HIVECAST_DIR
    $deskLnk.IconLocation = "$env:HIVECAST_EXE,0"
    $deskLnk.Description = 'HIVECAST Live Recording Studio'
    $deskLnk.Save()
  }
}
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("HIVECAST_EXE", exe_path)
        .env("HIVECAST_DIR", install_dir)
        .env("HIVECAST_DESKTOP", if desktop { "1" } else { "0" })
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("无法创建快捷方式：{e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("创建快捷方式失败：{stderr}"));
    }
    Ok(())
}

pub fn remove_shortcuts() {
    if let Ok(appdata) = env::var("APPDATA") {
        let start_lnk = PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("HIVECAST.lnk");
        let _ = fs::remove_file(start_lnk);
    }
    for var in ["USERPROFILE", "OneDrive", "OneDriveConsumer"] {
        if let Ok(base) = env::var(var) {
            let desk_lnk = PathBuf::from(base).join("Desktop").join("HIVECAST.lnk");
            let _ = fs::remove_file(desk_lnk);
        }
    }
}

pub fn write_uninstall_registry(
    install_dir: &Path,
    exe_path: &Path,
    uninstall_exe: &Path,
    version: &str,
    estimated_kb: u32,
) -> Result<(), String> {
    let subkey_w = to_wide(UNINSTALL_REG_SUBKEY);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey_w.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null(),
            &raw mut hkey,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(format!("写入卸载注册表失败（错误码 {rc}）"));
    }

    let uninstall_cmd = format!("\"{}\" --uninstall", uninstall_exe.display());
    let quiet_cmd = format!("\"{}\" --uninstall --silent", uninstall_exe.display());
    let icon_val = format!("{},0", exe_path.display());

    let res = (|| -> Result<(), String> {
        set_reg_sz(hkey, "DisplayName", "HIVECAST")?;
        set_reg_sz(hkey, "DisplayVersion", version)?;
        set_reg_sz(hkey, "Publisher", "HIVECAST")?;
        set_reg_sz(hkey, "InstallLocation", &install_dir.display().to_string())?;
        set_reg_sz(hkey, "DisplayIcon", &icon_val)?;
        set_reg_sz(hkey, "UninstallString", &uninstall_cmd)?;
        set_reg_sz(hkey, "QuietUninstallString", &quiet_cmd)?;
        set_reg_dword(hkey, "NoModify", 1)?;
        set_reg_dword(hkey, "NoRepair", 1)?;
        set_reg_dword(hkey, "EstimatedSize", estimated_kb)?;
        Ok(())
    })();

    unsafe {
        RegCloseKey(hkey);
    }
    res
}

pub fn read_installed_dir() -> Option<PathBuf> {
    let raw = read_reg_string(HKEY_CURRENT_USER, UNINSTALL_REG_SUBKEY, "InstallLocation")?;
    crate::install::validate_install_dir(&raw).ok()
}

pub fn delete_uninstall_registry() {
    let subkey_w = to_wide(UNINSTALL_REG_SUBKEY);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, subkey_w.as_ptr());
    }
}

pub fn launch_app(exe_path: &Path) -> Result<(), String> {
    let mut cmd = Command::new(exe_path);
    if let Some(parent) = exe_path.parent() {
        cmd.current_dir(parent);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("无法启动 HIVECAST：{e}"))
}

/// Remove the running uninstaller executable and remove `install_dir` ONLY if empty.
pub fn schedule_self_cleanup(self_exe: &Path, install_dir: &Path) {
    let _ = Command::new("cmd.exe")
        .args([
            "/C",
            "ping 127.0.0.1 -n 3 >NUL & del /F /Q \"%HIVECAST_SELF%\" >NUL 2>&1 & rmdir \"%HIVECAST_CLEAN_DIR%\" >NUL 2>&1",
        ])
        .env("HIVECAST_SELF", self_exe)
        .env("HIVECAST_CLEAN_DIR", install_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn set_reg_sz(hkey: HKEY, name: &str, value: &str) -> Result<(), String> {
    let name_w = to_wide(name);
    let val_w = to_wide(value);
    let byte_len = (val_w.len() * std::mem::size_of::<u16>()) as u32;
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            name_w.as_ptr(),
            0,
            REG_SZ,
            val_w.as_ptr().cast(),
            byte_len,
        )
    };
    if rc != 0 {
        return Err(format!("写入注册表项 {name} 失败（错误码 {rc}）"));
    }
    Ok(())
}

fn set_reg_dword(hkey: HKEY, name: &str, value: u32) -> Result<(), String> {
    let name_w = to_wide(name);
    let bytes = value.to_le_bytes();
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            name_w.as_ptr(),
            0,
            REG_DWORD,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    if rc != 0 {
        return Err(format!("写入注册表项 {name} 失败（错误码 {rc}）"));
    }
    Ok(())
}

fn read_reg_dword(root: HKEY, subkey: &str, name: &str) -> Option<u32> {
    let subkey_w = to_wide(subkey);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(root, subkey_w.as_ptr(), 0, KEY_READ, &raw mut hkey) };
    if rc != 0 {
        return None;
    }
    let name_w = to_wide(name);
    let mut value_type = 0u32;
    let mut data = [0u8; 4];
    let mut data_len = data.len() as u32;
    let qrc = unsafe {
        RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null(),
            &raw mut value_type,
            data.as_mut_ptr(),
            &raw mut data_len,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if qrc == 0 && value_type == REG_DWORD && data_len == 4 {
        Some(u32::from_le_bytes(data))
    } else {
        None
    }
}

fn read_reg_string(root: HKEY, subkey: &str, name: &str) -> Option<String> {
    let subkey_w = to_wide(subkey);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(root, subkey_w.as_ptr(), 0, KEY_READ, &raw mut hkey) };
    if rc != 0 {
        return None;
    }
    let name_w = to_wide(name);
    let mut value_type = 0u32;
    let mut byte_len = 0u32;
    let q1 = unsafe {
        RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null(),
            &raw mut value_type,
            std::ptr::null_mut(),
            &raw mut byte_len,
        )
    };
    if q1 != 0 || value_type != REG_SZ || byte_len < 2 {
        unsafe {
            RegCloseKey(hkey);
        }
        return None;
    }
    let mut buf = vec![0u16; (byte_len as usize).div_ceil(2)];
    let q2 = unsafe {
        RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null(),
            &raw mut value_type,
            buf.as_mut_ptr().cast(),
            &raw mut byte_len,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if q2 != 0 {
        return None;
    }
    let valid_u16s = (byte_len as usize) / 2;
    if valid_u16s < buf.len() {
        buf.truncate(valid_u16s);
    }
    if let Some(nul_pos) = buf.iter().position(|&c| c == 0) {
        buf.truncate(nul_pos);
    }
    Some(String::from_utf16_lossy(&buf))
}
