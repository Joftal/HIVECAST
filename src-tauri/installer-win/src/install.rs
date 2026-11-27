//! Cross-platform installer core logic: CLI parsing, path validation, FFmpeg detection.

use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    GuiInstall,
    GuiUninstall,
    SilentInstall {
        dir: Option<PathBuf>,
        desktop_shortcut: bool,
    },
    SilentUninstall {
        dir: Option<PathBuf>,
        purge_data: bool,
    },
    SelfTest,
    Pack {
        stub: PathBuf,
        app_exe: PathBuf,
        version: String,
        out_setup: PathBuf,
    },
}

pub fn is_uninstaller_exe(exe_path: &Path) -> bool {
    exe_path
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("uninstall.exe"))
}

#[cfg(test)]
pub fn parse_cli(args: &[String]) -> Result<CliAction, String> {
    parse_cli_for_exe(None, true, args)
}

pub fn parse_cli_for_exe(
    exe_path: Option<&Path>,
    has_payload: bool,
    args: &[String],
) -> Result<CliAction, String> {
    if args.first().is_some_and(|arg| arg == "--pack") {
        if args.len() != 5 {
            return Err("用法: --pack <stub.exe> <HIVECAST.exe> <version> <out-setup.exe>".into());
        }
        return Ok(CliAction::Pack {
            stub: PathBuf::from(&args[1]),
            app_exe: PathBuf::from(&args[2]),
            version: args[3].clone(),
            out_setup: PathBuf::from(&args[4]),
        });
    }
    if args.iter().any(|a| a == "--self-test") {
        return Ok(CliAction::SelfTest);
    }

    let uninstaller = !has_payload && exe_path.is_some_and(is_uninstaller_exe);
    if !has_payload && !uninstaller {
        return Err("安装包缺少有效载荷或已损坏，请重新下载安装包。".into());
    }
    let mut uninstall = uninstaller;
    let mut silent = false;
    let mut desktop_shortcut = true;
    let mut purge_data = false;
    let mut dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--uninstall" => uninstall = true,
            "--silent" | "/S" => silent = true,
            "--no-shortcut" => desktop_shortcut = false,
            "--purge-data" => purge_data = true,
            "--dir" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "--dir 需要传入目标目录".to_string())?;
                dir = Some(PathBuf::from(value));
                i += 1;
            }
            other if other.starts_with("/D=") => {
                dir = Some(PathBuf::from(&other[3..]));
            }
            _ => {}
        }
        i += 1;
    }

    match (uninstall, silent) {
        (true, true) => Ok(CliAction::SilentUninstall { dir, purge_data }),
        (true, false) => Ok(CliAction::GuiUninstall),
        (false, true) => Ok(CliAction::SilentInstall {
            dir,
            desktop_shortcut,
        }),
        (false, false) => Ok(CliAction::GuiInstall),
    }
}

/// Strip surrounding whitespace and Explorer's BOM/bidi marks. Only the REG_SZ
/// reader may interpret a NUL as a terminator; keep it here for validation.
pub fn sanitize_dir_input(raw: &str) -> String {
    raw.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '\u{feff}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}'
            )
    })
    .to_string()
}

pub fn default_install_dir() -> PathBuf {
    if let Ok(local) = env::var("LOCALAPPDATA") {
        let clean = sanitize_dir_input(&local);
        if !clean.is_empty() {
            return PathBuf::from(clean).join("HIVECAST");
        }
    }
    if let Ok(home) = env::var("USERPROFILE").or_else(|_| env::var("HOME")) {
        let clean = sanitize_dir_input(&home);
        if !clean.is_empty() {
            return PathBuf::from(clean).join("HIVECAST");
        }
    }
    PathBuf::from("HIVECAST")
}

/// Validate that a chosen install path is safe (absolute, non-root, non-system).
pub fn validate_install_dir(raw: &str) -> Result<PathBuf, String> {
    let trimmed = sanitize_dir_input(raw);
    if trimmed.is_empty() {
        return Err("安装路径不能为空".into());
    }
    if let Some(bad) = trimmed.chars().find(|c| c.is_control()) {
        return Err(format!("安装路径包含非法控制字符（U+{:04X}）", bad as u32));
    }
    let path = PathBuf::from(&trimmed);
    if !path.is_absolute() && !looks_like_windows_absolute(&trimmed) {
        return Err("请选择完整的绝对路径".into());
    }
    let normalized = trimmed
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    if normalized.is_empty() || normalized.len() <= 2 && normalized.ends_with(':') {
        return Err("不能直接安装到磁盘根目录，请选择子文件夹".into());
    }
    for forbidden in [
        "c:\\windows",
        "c:\\windows\\system32",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\users",
    ] {
        if normalized == forbidden {
            return Err("不能直接安装到系统目录，请在子目录中安装（例如 ...\\HIVECAST）".into());
        }
    }
    Ok(path)
}

fn looks_like_windows_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Detect whether FFmpeg is already available on PATH or common Windows install locations.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn detect_ffmpeg() -> Option<String> {
    let exe_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Some(candidate.display().to_string());
            }
        }
    }
    if cfg!(windows) {
        let mut extras = vec![PathBuf::from(r"C:\ffmpeg\bin\ffmpeg.exe")];
        if let Ok(local) = env::var("LOCALAPPDATA") {
            extras.push(
                PathBuf::from(&local)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links")
                    .join("ffmpeg.exe"),
            );
        }
        if let Ok(profile) = env::var("USERPROFILE") {
            extras.push(
                PathBuf::from(&profile)
                    .join("scoop")
                    .join("shims")
                    .join("ffmpeg.exe"),
            );
        }
        for candidate in extras {
            if candidate.is_file() {
                return Some(candidate.display().to_string());
            }
        }
    }
    None
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn has_existing_install(dir: &Path) -> bool {
    let exe = dir.join("HIVECAST.exe");
    if !exe.is_file() {
        return false;
    }
    #[cfg(windows)]
    if crate::win_os::has_description(&exe, "HIVECAST") {
        return true;
    }
    install_hash(dir).is_some_and(|hash| {
        crate::payload::sha256_file(&exe).is_ok_and(|(_, digest)| digest == hash)
    })
}

pub use crate::install_receipt::MARKER;

pub fn install_hash(dir: &Path) -> Option<String> {
    crate::install_receipt::read(dir).map(|(hash, _)| hash)
}

pub fn same_location(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a)
        .ok()
        .zip(std::fs::canonicalize(b).ok())
        .is_some_and(|(a, b)| a == b)
}

/// 只在空目录、可验证的产品安装或留有归属凭据的目录安装。
/// 已登记的有效安装须原位升级，绿色版与其他程序混放时拒绝覆盖。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn check_target_dir_policy(
    install_dir: &Path,
    registered_dir: Option<&Path>,
) -> Result<(), String> {
    let dir_exists = install_dir.try_exists().map_err(|e| e.to_string())?;
    if dir_exists && !install_dir.is_dir() {
        return Err("所选路径不是有效文件夹".into());
    }
    if dir_exists {
        let exe = install_dir.join("HIVECAST.exe");
        if exe.try_exists().map_err(|e| e.to_string())? && !has_existing_install(install_dir) {
            return Err("所选目录含有无法确认为 HIVECAST 的同名程序".into());
        }
        if install_dir.join(MARKER).exists() && install_hash(install_dir).is_none() {
            return Err("安装标记冲突，请选择新的专用目录".into());
        }
        if registered_dir.is_some_and(|dir| same_location(dir, install_dir))
            && (has_existing_install(install_dir) || install_hash(install_dir).is_some())
        {
            return Ok(());
        }
        #[cfg(windows)]
        if install_dir.join("uninstall.exe").exists()
            && !crate::win_os::has_description(&install_dir.join("uninstall.exe"), "HIVECAST Setup")
        {
            return Err("所选目录含有其他卸载程序".into());
        }
    }
    // A portable executable in Downloads does not make the entire folder an installation.
    if dir_exists && install_hash(install_dir).is_none() && has_existing_install(install_dir) {
        for entry in std::fs::read_dir(install_dir).map_err(|e| e.to_string())? {
            let name = entry
                .map_err(|e| e.to_string())?
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "hivecast.exe"
                    | "uninstall.exe"
                    | "cache"
                    | "webview"
                    | "logs"
                    | "data"
                    | "recording"
            ) {
                return Err("绿色版与其他文件混放，请先将程序和数据移入专用目录".into());
            }
        }
    }
    if registered_dir.is_some_and(has_existing_install) {
        return Err("请使用已登记的安装目录升级，或先卸载原版本".into());
    }
    if dir_exists
        && std::fs::read_dir(install_dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        && !is_reusable_preserved_dir(install_dir)
        && !has_existing_install(install_dir)
    {
        return Err("目录非空，请选择新的 HIVECAST 专用子目录".into());
    }
    Ok(())
}

/// 通过文件夹选择框选中父目录（如 `D:\` 或 `D:\Apps`）时自动补齐 `\HIVECAST` 子目录，
/// 避免用户每次都要手动新建文件夹；若选中的目录本身已名为 `HIVECAST`、或已是既有安装/保留数据目录则保持原样。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn normalize_browsed_dir(chosen: &Path) -> PathBuf {
    if has_existing_install(chosen) || is_reusable_preserved_dir(chosen) {
        return chosen.to_path_buf();
    }
    let raw = chosen.to_string_lossy();
    let trimmed = raw.trim_end_matches(['\\', '/']);
    let last_segment = trimmed.rsplit(['\\', '/']).next().unwrap_or("");
    if last_segment.eq_ignore_ascii_case("hivecast") {
        return chosen.to_path_buf();
    }
    chosen.join("HIVECAST")
}

/// 复用安装回执或旧产品数据库确认的残留目录。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn is_reusable_preserved_dir(dir: &Path) -> bool {
    install_hash(dir).is_some() || crate::cleanup::product_db(&dir.join("data")).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_pack_self_test_silent_and_uninstall() {
        assert_eq!(parse_cli(&[]).unwrap(), CliAction::GuiInstall);
        assert_eq!(
            parse_cli(&["--self-test".into()]).unwrap(),
            CliAction::SelfTest
        );
        assert_eq!(
            parse_cli(&["--uninstall".into()]).unwrap(),
            CliAction::GuiUninstall
        );
        assert_eq!(
            parse_cli(&[
                "--silent".into(),
                "--dir".into(),
                r"D:\Apps\HIVECAST".into(),
                "--no-shortcut".into()
            ])
            .unwrap(),
            CliAction::SilentInstall {
                dir: Some(PathBuf::from(r"D:\Apps\HIVECAST")),
                desktop_shortcut: false,
            }
        );
        assert_eq!(
            parse_cli(&[
                "--uninstall".into(),
                "--silent".into(),
                "--purge-data".into()
            ])
            .unwrap(),
            CliAction::SilentUninstall {
                dir: None,
                purge_data: true,
            }
        );
        assert_eq!(
            parse_cli(&[
                "--pack".into(),
                "a".into(),
                "b".into(),
                "0.1.0".into(),
                "out".into()
            ])
            .unwrap(),
            CliAction::Pack {
                stub: PathBuf::from("a"),
                app_exe: PathBuf::from("b"),
                version: "0.1.0".into(),
                out_setup: PathBuf::from("out"),
            }
        );
    }

    #[test]
    fn install_dir_validation_blocks_roots_and_system_dirs() {
        assert!(validate_install_dir(r"C:\Users\alice\AppData\Local\HIVECAST").is_ok());
        assert!(validate_install_dir(r"D:\工具\HIVECAST").is_ok());
        assert_eq!(
            validate_install_dir(" \u{feff} \u{202a}C:\\Users\\alice\\HIVECAST\u{202c} ").unwrap(),
            PathBuf::from(r"C:\Users\alice\HIVECAST")
        );
        for bad in [
            "",
            "   ",
            "relative\\dir",
            r"C:\",
            r"C:\Windows",
            r"C:\Program Files",
            "C:\\Bad\nPath",
        ] {
            assert!(
                validate_install_dir(bad).is_err(),
                "expected invalid install dir: {bad:?}"
            );
        }
    }

    #[test]
    fn entered_path_rejects_nul_instead_of_installing_to_a_prefix() {
        for raw in ["\0C:\\Apps", "C:\\Apps\0", "C:\\Apps\0\\HIVECAST"] {
            assert!(validate_install_dir(raw).unwrap_err().contains("U+0000"));
        }
    }

    #[test]
    fn preserved_directory_requires_evidence_of_product_ownership() {
        let root = tempfile::tempdir().unwrap();
        assert!(!is_reusable_preserved_dir(root.path()));

        let only_recording = root.path().join("only-recording");
        std::fs::create_dir_all(only_recording.join("Recording")).unwrap();
        assert!(!is_reusable_preserved_dir(&only_recording));

        let both = root.path().join("both");
        std::fs::create_dir_all(both.join("data")).unwrap();
        std::fs::create_dir_all(both.join("Recording")).unwrap();
        assert!(!is_reusable_preserved_dir(&both));
        std::fs::write(both.join(MARKER), "a".repeat(64)).unwrap();
        assert!(is_reusable_preserved_dir(&both));

        let foreign_file = root.path().join("foreign-file");
        std::fs::create_dir_all(foreign_file.join("data")).unwrap();
        std::fs::write(foreign_file.join("other.txt"), b"x").unwrap();
        assert!(!is_reusable_preserved_dir(&foreign_file));

        let foreign_dir = root.path().join("foreign-dir");
        std::fs::create_dir_all(foreign_dir.join("OtherApp")).unwrap();
        assert!(!is_reusable_preserved_dir(&foreign_dir));
    }

    #[test]
    fn browsed_folder_appends_hivecast_subdir_unless_already_named_or_existing() {
        let root = tempfile::tempdir().unwrap();

        let parent = root.path().join("Apps");
        std::fs::create_dir_all(&parent).unwrap();
        assert_eq!(normalize_browsed_dir(&parent), parent.join("HIVECAST"));

        let already_named = root.path().join("HIVECAST");
        std::fs::create_dir_all(&already_named).unwrap();
        assert_eq!(normalize_browsed_dir(&already_named), already_named);

        let lower_named = root.path().join("hivecast");
        std::fs::create_dir_all(&lower_named).unwrap();
        assert_eq!(normalize_browsed_dir(&lower_named), lower_named);

        let existing_custom = root.path().join("MyStudio");
        std::fs::create_dir_all(&existing_custom).unwrap();
        std::fs::write(existing_custom.join("HIVECAST.exe"), b"exe").unwrap();
        assert_eq!(
            normalize_browsed_dir(&existing_custom),
            existing_custom.join("HIVECAST")
        );

        let preserved_custom = root.path().join("KeptData");
        std::fs::create_dir_all(preserved_custom.join("data")).unwrap();
        assert_eq!(
            normalize_browsed_dir(&preserved_custom),
            preserved_custom.join("HIVECAST")
        );

        // Windows drive root path appends HIVECAST instead of blocking the user.
        assert_eq!(
            normalize_browsed_dir(Path::new(r"D:\")),
            Path::new(r"D:\").join("HIVECAST")
        );
    }

    #[test]
    fn uninstaller_exe_defaults_to_uninstall_without_flag() {
        assert_eq!(
            parse_cli_for_exe(Some(Path::new("uninstall.exe")), false, &[]).unwrap(),
            CliAction::GuiUninstall
        );
        for name in ["remove-hivecast.exe", "damaged-setup.exe"] {
            assert!(parse_cli_for_exe(Some(Path::new(name)), false, &["--silent".into()]).is_err());
        }
        assert_eq!(
            parse_cli_for_exe(
                Some(Path::new("/tmp/UNINSTALL.EXE")),
                false,
                &["--silent".into(), "--purge-data".into()]
            )
            .unwrap(),
            CliAction::SilentUninstall {
                dir: None,
                purge_data: true,
            }
        );
        assert_eq!(
            parse_cli_for_exe(
                Some(Path::new("HIVECAST-v1.0.0-Windows-x64-setup.exe")),
                true,
                &[]
            )
            .unwrap(),
            CliAction::GuiInstall
        );
    }

    #[test]
    fn target_dir_policy_allows_moved_or_registry_lost_hivecast_but_blocks_foreign_dirs() {
        let root = tempfile::tempdir().unwrap();

        // 1. Registry-lost or portable HIVECAST directory is allowed to upgrade/repair in place.
        let lost_reg = root.path().join("HIVECAST-moved");
        std::fs::create_dir_all(lost_reg.join("data")).unwrap();
        std::fs::write(lost_reg.join("HIVECAST.exe"), b"old-exe").unwrap();
        std::fs::write(lost_reg.join("data").join("hivecast.db"), b"db").unwrap();
        assert!(check_target_dir_policy(&lost_reg, None).is_err());
        let (_, hash) = crate::payload::sha256_file(&lost_reg.join("HIVECAST.exe")).unwrap();
        std::fs::write(lost_reg.join(MARKER), hash).unwrap();
        assert!(check_target_dir_policy(&lost_reg, None).is_ok());

        // 2. Old registered path no longer exists (user moved folder) -> upgrading new location succeeds.
        let stale_reg = root.path().join("HIVECAST-old-gone");
        assert!(check_target_dir_policy(&lost_reg, Some(&stale_reg)).is_ok());

        // 3. Another active registered installation still exists -> refuse duplicate install elsewhere.
        let other_dir = root.path().join("HIVECAST-second");
        assert!(check_target_dir_policy(&other_dir, Some(&lost_reg)).is_err());

        // 4. Registered installation itself -> allowed.
        assert!(check_target_dir_policy(&lost_reg, Some(&lost_reg)).is_ok());

        // 5. Foreign non-empty directory (even with uninstall.exe, but no HIVECAST.exe) -> blocked.
        let foreign = root.path().join("OtherApp");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("uninstall.exe"), b"other").unwrap();
        assert!(check_target_dir_policy(&foreign, None).is_err());
    }
}
