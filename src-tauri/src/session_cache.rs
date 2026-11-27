//! Shared cleanup for the app and Windows uninstaller. Only remove files that the
//! viewing and local playback pipelines generate in their per-session directories.
use std::{fs, io, path::Path};

fn ordinary(path: &Path, directory: bool) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        }
    })
}

pub fn session_id(id: &str) -> bool {
    (id.len() == 32 || id.len() == 36)
        && id.bytes().enumerate().all(|(i, b)| {
            if id.len() == 36 && [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub fn clean(root: &Path) -> io::Result<()> {
    if !ordinary(root, true) {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let dir = entry.path();
        let id = if root.ends_with("play") {
            let Some((id, stamp)) = name.rsplit_once('-') else {
                continue;
            };
            if stamp.parse::<u64>().is_err() {
                continue;
            }
            id
        } else {
            &name
        };
        // Session IDs are generated with UUID::simple() or the UUID hyphenated representation.
        if !session_id(id) || !ordinary(&dir, true) {
            continue;
        }
        for file in fs::read_dir(&dir)? {
            let file = file?;
            let name = file.file_name();
            let name = name.to_string_lossy();
            let segment = name
                .strip_prefix("seg_")
                .and_then(|s| s.split_once('.'))
                .is_some_and(|(number, ext)| {
                    number.len() >= 5
                        && number.bytes().all(|b| b.is_ascii_digit())
                        && matches!(ext, "ts" | "m4s")
                });
            if (segment || matches!(name.as_ref(), "index.m3u8" | "init.mp4" | "ffmpeg.log"))
                && ordinary(&file.path(), false)
            {
                fs::remove_file(file.path())?;
            }
        }
        if fs::read_dir(&dir)?.next().is_none() {
            fs::remove_dir(dir)?;
        }
    }
    Ok(())
}
