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

/// Read the localized VERSIONINFO resource without running the selected executable.
pub fn has_description(path: &Path, expected: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let len = GetFileVersionInfoSizeW(name.as_ptr(), std::ptr::null_mut());
        if len == 0 {
            return false;
        }
        let mut data = vec![0u32; (len as usize).div_ceil(4)];
        let ptr = data.as_mut_ptr().cast();
        if GetFileVersionInfoW(name.as_ptr(), 0, len, ptr) == 0 {
            return false;
        }
        let mut value = std::ptr::null_mut();
        let mut size = 0;
        if VerQueryValueW(
            ptr,
            to_wide(r"\VarFileInfo\Translation").as_ptr(),
            &mut value,
            &mut size,
        ) == 0
            || size < 4
        {
            return false;
        }
        let lang = value.cast::<u16>().read_unaligned();
        let code = value.cast::<u16>().add(1).read_unaligned();
        ["ProductName", "FileDescription"]
            .into_iter()
            .zip(["HIVECAST", expected])
            .all(|(key, wanted)| {
                let key = to_wide(&format!(r"\StringFileInfo\{lang:04x}{code:04x}\{key}"));
                VerQueryValueW(ptr, key.as_ptr(), &mut value, &mut size) != 0
                    && size > 0
                    && String::from_utf16_lossy(std::slice::from_raw_parts(
                        value.cast(),
                        size as usize,
                    ))
                    .trim_end_matches('\0')
                        == wanted
            })
    }
}

pub fn show_package_error(message: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            to_wide(message).as_ptr(),
            to_wide("HIVECAST Setup").as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
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
$init = $env:HIVECAST_INIT_DIR
while ($init -and -not (Test-Path -LiteralPath $init)) {
  $parent = Split-Path -Path $init -Parent
  if ($parent -eq $init) { break }
  $init = $parent
}
if ($init -and (Test-Path -LiteralPath $init)) {
  $dlg.SelectedPath = $init
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
// Uses native Unicode COM IShellLinkW + IPersistFile instead of WScript.Shell, which rejects non-ACP
// Unicode paths (e.g., CJK install paths on English Windows) with E_INVALIDARG.
fn shortcut_command(
    exe: &Path,
    desktop: bool,
    staged: &Path,
    script: &str,
) -> Result<String, String> {
    let common = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

public static class HivecastShellLink {
    [ComImport]
    [Guid("00021401-0000-0000-C000-000000000046")]
    private class ShellLink {}

    [ComImport]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    [Guid("000214F9-0000-0000-C000-000000000046")]
    private interface IShellLinkW {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszFile, int cchMaxPath, IntPtr pfd, uint fFlags);
        void GetIDList(out IntPtr ppidl);
        void SetIDList(IntPtr pidl);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszName, int cchMaxName);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszDir, int cchMaxPath);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string pszDir);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszArgs, int cchMaxPath);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string pszArgs);
        void GetHotkey(out ushort pwHotkey);
        void SetHotkey(ushort wHotkey);
        void GetShowCmd(out int piShowCmd);
        void SetShowCmd(int iShowCmd);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszIconPath, int cchIconPath, out int piIcon);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string pszIconPath, int iIcon);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string pszPathRel, uint dwReserved);
        void Resolve(IntPtr hwnd, uint fFlags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string pszFile);
    }

    public static void Create(string lnkPath, string targetPath, string workingDir, string description) {
        var link = (IShellLinkW)new ShellLink();
        try {
            link.SetPath(targetPath);
            link.SetWorkingDirectory(workingDir);
            link.SetIconLocation(targetPath, 0);
            link.SetDescription(description);
            ((IPersistFile)link).Save(lnkPath, true);
        } finally {
            Marshal.ReleaseComObject(link);
        }
    }

    public static string GetTarget(string lnkPath) {
        var link = (IShellLinkW)new ShellLink();
        try {
            ((IPersistFile)link).Load(lnkPath, 0);
            var sb = new StringBuilder(4096);
            link.GetPath(sb, sb.Capacity, IntPtr.Zero, 0);
            if (sb.Length == 0) {
                link.GetPath(sb, sb.Capacity, IntPtr.Zero, 4);
            }
            return sb.ToString();
        } finally {
            Marshal.ReleaseComObject(link);
        }
    }
}
'@
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
            "-STA",
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
    // IShellLinkW + IPersistFile saves a full-Unicode .lnk without WScript.Shell's ANSI codepage limits.
    let staged = staging.join("HIVECAST.lnk");
    let script = r#"
[HivecastShellLink]::Create(
    $env:HIVECAST_STAGED_LINK,
    $env:HIVECAST_EXE,
    [IO.Path]::GetDirectoryName($env:HIVECAST_EXE),
    'HIVECAST Live Recording Studio'
)
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
$expected = [IO.Path]::GetFullPath($env:HIVECAST_EXE)
foreach ($path in $paths) {
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        $target = [HivecastShellLink]::GetTarget($path)
        if ($target -and [string]::Equals([IO.Path]::GetFullPath($target), $expected, [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $path
        }
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
for ($i = 0; $i -lt 20; $i++) {
    try {
        [IO.Directory]::Delete($env:HIVECAST_CLEAN_DIR, $false)
        break
    } catch [IO.DirectoryNotFoundException] {
        break
    } catch {
        Start-Sleep -Milliseconds 100
    }
}
"#;
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .current_dir(std::env::temp_dir())
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
