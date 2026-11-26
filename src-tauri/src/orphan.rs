//! Inspect the recorder's final output argument before touching a surviving process.
use std::path::Path;

fn same_executable(name: &str) -> bool {
    let Some(actual) = Path::new(name).file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let binary = crate::ffmpeg::binary();
    let expected = Path::new(&binary).file_name().and_then(|n| n.to_str());
    let matches = |value: &str| {
        if cfg!(windows) {
            actual.eq_ignore_ascii_case(value)
        } else {
            actual == value
        }
    };
    expected.is_some_and(matches)
        || matches(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        })
}

fn recording_args(args: &[String], path: &Path) -> bool {
    let Some(output) = args.last() else {
        return false;
    };
    let expected = crate::segment::parts_pattern(path);
    path.is_absolute()
        && args.windows(2).any(|p| p[0] == "-f" && p[1] == "segment")
        && Path::new(output) == Path::new(&expected)
}

#[cfg(windows)]
fn split_windows(command: &str) -> Option<Vec<String>> {
    use windows_sys::Win32::{Foundation::LocalFree, UI::Shell::CommandLineToArgvW};
    let wide: Vec<_> = command.encode_utf16().chain(Some(0)).collect();
    let mut count = 0;
    // The API returns a single LocalAlloc block of NUL-terminated strings.
    unsafe {
        let argv = CommandLineToArgvW(wide.as_ptr(), &mut count);
        if argv.is_null() {
            return None;
        }
        let args = std::slice::from_raw_parts(argv, count as usize)
            .iter()
            .map(|&p| {
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                String::from_utf16(std::slice::from_raw_parts(p, len)).ok()
            })
            .collect();
        LocalFree(argv.cast());
        args
    }
}

/// True when it is safe to finalize the session. An inaccessible process stays pending.
#[cfg(windows)]
pub fn stop_if_owned(pid: u32, path: &Path) -> bool {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
        System::Threading::*,
    };
    // Keep the handle through inspection, termination and wait. A reused numeric
    // PID can never redirect TerminateProcess to a different process.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error()
            == Some(ERROR_INVALID_PARAMETER as i32);
    }
    let _owner = unsafe { OwnedHandle::from_raw_handle(handle) };
    let exited = |timeout| unsafe { WaitForSingleObject(handle, timeout) == WAIT_OBJECT_0 };
    if exited(0) {
        return true;
    }
    let script = format!(
        r#"[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); Get-CimInstance Win32_Process -Filter 'ProcessId={pid}' -ErrorAction Stop | Select-Object Name,CommandLine | ConvertTo-Json -Compress"#
    );
    let info = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok());
    let Some(info) = info else {
        return exited(0);
    };
    let Some(name) = info["Name"].as_str() else {
        return exited(0);
    };
    if !same_executable(name) {
        return true;
    }
    let Some(args) = info["CommandLine"]
        .as_str()
        .filter(|c| !c.is_empty())
        .and_then(split_windows)
    else {
        return exited(0);
    };
    if !recording_args(&args, path) {
        return true;
    }
    unsafe {
        TerminateProcess(handle, 1);
    }
    exited(3000)
}

#[cfg(target_os = "macos")]
fn process_args(pid: i32) -> Option<Vec<String>> {
    // KERN_PROCARGS2 provides argc, executable path, padding, then real argv.
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0;
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        let mut buf = vec![0u8; size];
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        buf.truncate(size);
        let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
        let tail = buf.get(4..)?;
        let end = tail.iter().position(|b| *b == 0)?;
        let arguments = &tail[end..];
        let start = arguments.iter().position(|b| *b != 0)?;
        let mut args: Vec<String> = arguments[start..]
            .split(|b| *b == 0)
            .take(argc)
            .map(|arg| String::from_utf8(arg.to_vec()).ok())
            .collect::<Option<_>>()?;
        if args.len() != argc || args.is_empty() {
            return None;
        }
        args[0] = std::str::from_utf8(&tail[..end]).ok()?.to_owned();
        Some(args)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn process_args(pid: i32) -> Option<Vec<String>> {
    let data = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let mut args: Vec<String> = data
        .strip_suffix(&[0])?
        .split(|b| *b == 0)
        .map(|arg| String::from_utf8(arg.to_vec()).ok())
        .collect::<Option<_>>()?;
    *args.first_mut()? = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()?
        .to_str()?
        .to_owned();
    Some(args)
}

#[cfg(unix)]
pub fn stop_if_owned(pid: u32, path: &Path) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let gone = || unsafe {
        libc::kill(pid, 0) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    };
    let Some(args) = process_args(pid) else {
        return gone();
    };
    if !args.first().is_some_and(|s| same_executable(s)) || !recording_args(&args, path) {
        return true;
    }
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return gone();
    }
    for _ in 0..30 {
        if gone() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    gone()
}

/// A missing sidecar alone cannot distinguish a clean stop from a surviving writer.
pub fn stop_unmarked(path: &Path) -> bool {
    recorder_pids().is_some_and(|pids| pids.into_iter().all(|pid| stop_if_owned(pid, path)))
}

#[cfg(windows)]
fn recorder_pids() -> Option<Vec<u32>> {
    let output = std::process::Command::new("powershell").args([
        "-NoProfile", "-NonInteractive", "-Command",
        "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); ConvertTo-Json -Compress -InputObject @(Get-CimInstance Win32_Process | Select-Object Name,ProcessId)"
    ]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Process {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "ProcessId")]
        pid: u32,
    }
    let rows: Vec<Process> = serde_json::from_slice(&output.stdout).ok()?;
    if rows.is_empty() {
        return None;
    }
    Some(
        rows.into_iter()
            .filter(|p| same_executable(&p.name))
            .map(|p| p.pid)
            .collect(),
    )
}

#[cfg(unix)]
fn recorder_pids() -> Option<Vec<u32>> {
    let output = std::process::Command::new("ps")
        .args(["-ww", "-axo", "pid=,comm="])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    let mut pids = Vec::new();
    for line in std::str::from_utf8(&output.stdout).ok()?.lines() {
        let (pid, name) = line.trim().split_once(char::is_whitespace)?;
        if same_executable(name.trim()) {
            pids.push(pid.parse().ok()?);
        }
    }
    Some(pids)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_segment_output_matches() {
        let root = std::env::temp_dir();
        let path = root.join("中文 50%").join("a.flv");
        let template = crate::segment::parts_pattern(&path);
        let mut args = vec![
            "ffmpeg".into(),
            "-i".into(),
            path.display().to_string(),
            "-f".into(),
            "segment".into(),
            template.clone(),
        ];
        assert!(recording_args(&args, &path));
        for other in [
            template.replace("a.part", "a.part2.part"),
            root.join("other/a.flv").display().to_string(),
        ] {
            *args.last_mut().unwrap() = other;
            assert!(!recording_args(&args, &path));
        }
        #[cfg(windows)]
        assert!(recording_args(
            &split_windows(&format!("ffmpeg.exe -i x -f segment \"{template}\"")).unwrap(),
            &path
        ));
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "需要本机 FFmpeg 与 Windows CIM"]
    fn verify_live_owned_segment_process() {
        let dir = std::env::temp_dir().join(format!("中文 50% {}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("a.mp4");
        let mut child = std::process::Command::new(crate::ffmpeg::binary())
            .args([
                "-nostdin",
                "-loglevel",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "color=size=128x96",
                "-t",
                "12",
                "-c:v",
                "mpeg2video",
                "-f",
                "segment",
                "-segment_time",
                "1",
            ])
            .arg(crate::segment::parts_pattern(&path))
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let unrelated = stop_if_owned(child.id(), &dir.join("a.part2.mp4"));
        let still_running = child.try_wait().unwrap().is_none();
        let stopped = stop_if_owned(child.id(), &path);
        let exit = child.wait().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(unrelated && still_running && stopped && !exit.success());
    }
}
