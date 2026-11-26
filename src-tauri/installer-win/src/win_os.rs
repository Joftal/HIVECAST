//! Windows-specific OS integration (DWM styling, WebView2 check, shortcuts, Uninstall registry).

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyTransactedW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD,
    REG_SZ,
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

pub fn operation_lock() -> Result<OwnedHandle, String> {
    let name = to_wide(r"Local\HIVECAST-Install-Uninstall");
    unsafe {
        let raw =
            windows_sys::Win32::System::Threading::CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        let error = GetLastError();
        if raw.is_null() {
            return Err(format!("无法锁定安装状态：{error}"));
        }
        let handle = OwnedHandle::from_raw_handle(raw);
        if error == ERROR_ALREADY_EXISTS {
            return Err("另一个安装或卸载操作正在进行".into());
        }
        Ok(handle)
    }
}

// Resolve shortcut locations identically during installation and removal, including redirected desktops.
fn shortcut_command(
    exe: &Path,
    desktop: bool,
    staged: &Path,
    script: &str,
) -> Result<String, String> {
    let common = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$shell = New-Object -ComObject WScript.Shell
$folders = @(Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs')
if ($env:HIVECAST_DESKTOP -eq '1') {
    $desk = [Environment]::GetFolderPath('DesktopDirectory')
    if ($desk) { $folders += $desk }
}
$paths = @($folders | ForEach-Object { Join-Path $_ 'HIVECAST.lnk' })
"#;
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("{common}\n{script}"),
        ])
        .env("HIVECAST_STAGED_LINK", staged)
        .env("HIVECAST_EXE", exe)
        .env("HIVECAST_DESKTOP", if desktop { "1" } else { "0" })
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "快捷方式操作失败：{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn stage_shortcut(exe: &Path, desktop: bool, staging: &Path) -> Result<Vec<PathBuf>, String> {
    // WScript requires a .lnk extension. The stage directory is private to this operation.
    let staged = staging.join("HIVECAST.lnk");
    let script = r#"
$link = $shell.CreateShortcut($env:HIVECAST_STAGED_LINK)
$link.TargetPath = $env:HIVECAST_EXE
$link.WorkingDirectory = [IO.Path]::GetDirectoryName($env:HIVECAST_EXE)
$link.IconLocation = "$env:HIVECAST_EXE,0"
$link.Description = 'HIVECAST Live Recording Studio'
$link.Save()
ConvertTo-Json -Compress -InputObject $paths
"#;
    let json = shortcut_command(exe, desktop, &staged, script)?;
    serde_json::from_str(&json).map_err(|e| format!("读取快捷方式目录失败：{e}"))
}

pub fn remove_shortcuts(exe: &Path) -> Result<(), String> {
    shortcut_command(
        exe,
        true,
        Path::new(""),
        r#"
foreach ($path in $paths) {
    if ((Test-Path -LiteralPath $path -PathType Leaf) -and
        ($shell.CreateShortcut($path).TargetPath -eq $env:HIVECAST_EXE)) {
        Remove-Item -LiteralPath $path
    }
}
"#,
    )
    .map(|_| ())
}

pub fn write_uninstall_registry(
    install_dir: &Path,
    exe_path: &Path,
    uninstall_exe: &Path,
    version: &str,
    estimated_kb: u32,
) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::{CommitTransaction, CreateTransaction};
    let raw = unsafe {
        CreateTransaction(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            std::ptr::null(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // Closing an uncommitted transaction rolls back all of its registry changes.
    let transaction = unsafe { OwnedHandle::from_raw_handle(raw) };
    let subkey_w = to_wide(UNINSTALL_REG_SUBKEY);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyTransactedW(
            HKEY_CURRENT_USER,
            subkey_w.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null(),
            &raw mut hkey,
            std::ptr::null_mut(),
            transaction.as_raw_handle(),
            std::ptr::null(),
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
    res?;
    if unsafe { CommitTransaction(transaction.as_raw_handle()) } == 0 {
        return Err(format!(
            "提交卸载登记失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub fn read_installed_dir() -> Option<PathBuf> {
    read_reg_string(HKEY_CURRENT_USER, UNINSTALL_REG_SUBKEY, "InstallLocation").and_then(|s| {
        let clean = crate::install::sanitize_dir_input(&s);
        let trimmed = clean.as_str();
        let path = trimmed
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(trimmed);
        crate::install::validate_install_dir(path).ok()
    })
}

pub fn delete_uninstall_registry() -> Result<(), String> {
    let subkey_w = to_wide(UNINSTALL_REG_SUBKEY);
    let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, subkey_w.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("清除卸载登记失败：{rc}"))
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
pub fn schedule_self_cleanup(self_exe: &Path, install_dir: &Path) -> Result<(), String> {
    // Wait for the actual uninstaller (GUI users can keep its completion screen open).
    // An unregistered nonempty directory cannot be reused by the installer until cleanup finishes.
    let script = r#"
$parent = Get-Process -Id $env:HIVECAST_PARENT -ErrorAction SilentlyContinue
if ($parent) { $parent.WaitForExit() }
for ($i = 0; $i -lt 100; $i++) {
    try { [IO.File]::Delete($env:HIVECAST_SELF); break } catch { Start-Sleep -Milliseconds 100 }
}
try { [IO.Directory]::Delete($env:HIVECAST_CLEAN_DIR, $false) } catch [IO.IOException] {}
"#;
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("HIVECAST_PARENT", std::process::id().to_string())
        .env("HIVECAST_SELF", self_exe)
        .env("HIVECAST_CLEAN_DIR", install_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法启动卸载清理：{e}"))
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
