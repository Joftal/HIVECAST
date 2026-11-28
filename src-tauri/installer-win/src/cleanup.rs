//! Only clean a recognized profile and product runtime paths. A missing or unreadable
//! profile is never evidence that a different profile should be deleted.
use rusqlite::{Connection, OpenFlags};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

const WEBVIEW_MARKER: &str = ".hivecast-webview";
const WEBVIEW_MARKER_CONTENT: &[u8] = b"HIVECAST WebView2 v1";

fn regular(path: &Path, directory: bool) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
        if directory {
            m.is_dir()
        } else {
            m.is_file()
        }
    })
}

fn owned_webview(dir: &Path) -> bool {
    let marker = dir.join(WEBVIEW_MARKER);
    regular(&marker, false) && fs::read(marker).is_ok_and(|value| value == WEBVIEW_MARKER_CONTENT)
}

fn remove_webview_profile(path: &Path) -> io::Result<()> {
    // WebView2 children can briefly keep profile files open after the app exits.
    // Retry within the uninstall request, but still fail loudly if the profile stays locked.
    let mut last = None;
    for attempt in 0..5 {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                last = Some(err);
                if attempt < 4 {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("删除 WebView2 缓存失败")))
}

pub fn product_db(dir: &Path) -> Option<Connection> {
    let file = dir.join("hivecast.db");
    if !regular(dir, true) || !regular(&file, false) {
        return None;
    }
    let db = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.prepare("SELECT r.room_key,s.file_path,c.platform,a.value FROM room r,record_session s,credential c,app_setting a LIMIT 0").ok()?;
    Some(db)
}

fn protected_paths(db: &Connection) -> rusqlite::Result<Vec<PathBuf>> {
    let mut query = "SELECT value FROM app_setting WHERE key='media.root'".to_owned();
    // Older databases can be uninstalled before the upgraded app has run its migration.
    for (table, column) in [
        ("record_session", "file_path"),
        ("record_session", "source_path"),
        ("record_session", "danmaku_path"),
        ("record_session", "split_manifest"),
        ("media_file", "path"),
        ("media_root_history", "path"),
    ] {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            [table, column],
            |r| r.get(0),
        )?;
        if exists {
            query.push_str(&format!(" UNION SELECT {column} FROM {table}"));
        }
    }
    let mut stmt = db.prepare(&query)?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    let mut paths = Vec::new();
    for row in rows {
        if let Some(value) = row?.filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(rusqlite::Error::InvalidPath(path));
            }
            paths.push(path.canonicalize().unwrap_or(path));
        }
    }
    Ok(paths)
}

fn remove_empty(dir: &Path) -> io::Result<()> {
    if regular(dir, true) && fs::read_dir(dir)?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}

fn clean_runtime(root: &Path, protected: Option<&[PathBuf]>) -> io::Result<()> {
    for (parent, children) in [
        ("cache", &["play", "thumbs", "view"][..]),
        ("webview", &["EBWebView"][..]),
        ("logs", &[][..]),
    ] {
        let dir = root.join(parent);
        if !regular(&dir, true) {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let Some(protected) = protected else {
                continue;
            };
            let actual = path.canonicalize()?;
            if protected
                .iter()
                .any(|p| actual.starts_with(p) || p.starts_with(&actual))
            {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if children.contains(&name.as_ref()) && regular(&path, true) {
                match name.as_ref() {
                    "view" | "play" => crate::session_cache::clean(&path)?,
                    "thumbs" => {
                        for file in fs::read_dir(&path)? {
                            let file = file?.path();
                            let owned = file.extension().is_some_and(|ext| ext == "jpg")
                                && file
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .is_some_and(crate::session_cache::session_id);
                            if owned && regular(&file, false) {
                                fs::remove_file(file)?;
                            }
                        }
                    }
                    "EBWebView" => {
                        // The marker is written before WebView2 creates the profile. Once
                        // present, EBWebView is this app's disposable runtime cache; WebView2
                        // adds opaque files across runtime versions, so content sniffing is
                        // not a stable ownership test. Registered media was skipped above.
                        if owned_webview(&dir) {
                            remove_webview_profile(&path)?;
                        }
                    }
                    _ => {}
                }
                remove_empty(&path)?;
            } else if parent == "logs" && regular(&path, false) && is_app_log(&name) {
                fs::remove_file(&path)?;
            }
        }
        if parent == "webview" && !dir.join("EBWebView").exists() && owned_webview(&dir) {
            fs::remove_file(dir.join(WEBVIEW_MARKER))?;
        }
        remove_empty(&dir)?;
    }
    Ok(())
}

fn is_app_log(name: &str) -> bool {
    let Some(stamp) = name
        .strip_prefix("hivecast-")
        .and_then(|s| s.strip_suffix(".log"))
    else {
        return false;
    };
    let (date, part) = stamp.split_once('.').unwrap_or((stamp, "0"));
    date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
        && !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit())
}

fn purge_profile(dir: &Path, protected: &[PathBuf]) -> io::Result<()> {
    // Stage the matching database, journals and key on the same volume. A locked
    // file must leave the readable database and its original key together.
    let mut files = Vec::new();
    for name in [
        "instance.lock",
        "secret.bin",
        "secret.bin.bak",
        "hivecast.db-wal",
        "hivecast.db-shm",
        "hivecast.db-journal",
        "hivecast.db",
    ] {
        let src = dir.join(name);
        if !regular(&src, false) {
            continue;
        }
        let actual = src.canonicalize()?;
        if protected.contains(&actual) {
            return Ok(());
        }
        files.push(name);
    }
    let backup = tempfile::tempdir_in(dir)?;
    let mut staged = Vec::new();
    for name in files {
        let src = dir.join(name);
        if let Err(err) = fs::rename(&src, backup.path().join(name)) {
            let backup = backup.keep();
            for previous in staged {
                fs::rename(backup.join(previous), dir.join(previous)).map_err(|restore| {
                    io::Error::other(format!("{err}; {restore}; {}", backup.display()))
                })?;
            }
            fs::remove_dir(backup)?;
            return Err(err);
        }
        staged.push(name);
    }
    backup.close()?;
    remove_empty(dir)
}

pub fn clean(root: &Path, purge: bool) -> Result<(), String> {
    let data = root.join("data");
    let override_dir = std::env::var_os("HIVECAST_DATA_DIR").filter(|v| !v.is_empty());
    let class = crate::install_receipt::read(root).and_then(|(_, class)| class);
    let local =
        if override_dir.is_none() && !matches!(class.as_deref(), Some("system" | "external")) {
            product_db(&data)
        } else {
            None
        };
    let fallback = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("com.hivecast.desktop"));
    let system = fallback
        .as_deref()
        .filter(|_| {
            override_dir.is_none()
                && local.is_none()
                && !matches!(class.as_deref(), Some("external" | "local"))
        })
        .and_then(product_db)
        .filter(|db| {
            db.query_row(
                "SELECT value FROM app_setting WHERE key='install.system_profile_owner'",
                [],
                |r| r.get::<_, String>(0),
            )
            .is_ok_and(|p| crate::install::same_location(root, Path::new(&p)))
        });
    let (profile, db) = if local.is_some() {
        (Some(data.as_path()), local)
    } else {
        (fallback.as_deref(), system)
    };
    let mut protected = db
        .as_ref()
        .map(protected_paths)
        .transpose()
        .map_err(|e| format!("无法核对录像位置：{e}"))?;
    if let (Some(list), Some(media)) = (&mut protected, std::env::var_os("HIVECAST_MEDIA_DIR")) {
        let path = PathBuf::from(media);
        list.push(path.canonicalize().unwrap_or(path));
    }
    drop(db);
    clean_runtime(root, protected.as_deref()).map_err(|e| format!("清理运行文件失败：{e}"))?;
    if purge {
        if let (Some(dir), Some(paths)) = (profile, protected.as_deref()) {
            purge_profile(dir, paths).map_err(|e| format!("清理账号设置失败：{e}"))?;
        }
    }
    Ok(())
}

pub fn finish(root: &Path) -> Result<(), String> {
    let data = root.join("data");
    // Keep an ownership receipt when user data or recordings survive uninstall.
    if !data.exists()
        && ["Recording", "cache", "webview", "logs"]
            .iter()
            .all(|name| !root.join(name).exists())
        && crate::install::install_hash(root).is_some()
    {
        fs::remove_file(root.join(crate::install::MARKER)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_preserves_foreign_files_and_registered_media_under_runtime_paths() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        fs::create_dir(&dir).unwrap();
        let historic = root.path().join("webview/EBWebView/old/clip.mp4");
        let library = root.path().join("cache/view/library");
        fs::create_dir_all(&library).unwrap();
        fs::create_dir_all(historic.parent().unwrap()).unwrap();
        fs::write(&historic, b"old video").unwrap();
        fs::write(library.join("new.mp4"), b"new video").unwrap();
        let db = Connection::open(dir.join("hivecast.db")).unwrap();
        db.execute_batch("CREATE TABLE app_setting(key TEXT,value TEXT);CREATE TABLE room(room_key TEXT);
            CREATE TABLE record_session(file_path TEXT,source_path TEXT,danmaku_path TEXT,split_manifest TEXT);
            CREATE TABLE credential(platform TEXT); CREATE TABLE media_file(path TEXT);").unwrap();
        db.execute(
            "INSERT INTO app_setting VALUES('media.root',?1)",
            [library.to_string_lossy()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO media_file VALUES(?1)",
            [historic.to_string_lossy()],
        )
        .unwrap();
        drop(db);
        let paths = protected_paths(&product_db(&dir).unwrap()).unwrap();
        fs::write(
            root.path().join("webview").join(WEBVIEW_MARKER),
            WEBVIEW_MARKER_CONTENT,
        )
        .unwrap();
        fs::write(dir.join("secret.bin"), b"key").unwrap();
        fs::write(dir.join("project.txt"), b"user").unwrap();
        clean_runtime(root.path(), Some(&paths)).unwrap();
        purge_profile(&dir, &paths).unwrap();
        assert_eq!(fs::read(&historic).unwrap(), b"old video");
        assert_eq!(fs::read(library.join("new.mp4")).unwrap(), b"new video");
        assert_eq!(fs::read(dir.join("project.txt")).unwrap(), b"user");
        assert!(!dir.join("secret.bin").exists());
        assert!(!dir.join("hivecast.db").exists());
    }

    #[test]
    fn clean_runtime_removes_owned_profile_with_arbitrary_files() {
        let root = tempfile::tempdir().unwrap();
        let webview = root.path().join("webview");
        let profile = webview.join("EBWebView");
        fs::create_dir_all(profile.join("Default/SomeFutureComponent")).unwrap();
        fs::write(profile.join("Local State"), b"{}").unwrap();
        fs::write(profile.join("Default/SomeFutureComponent/blob"), b"browser").unwrap();
        fs::write(
            profile.join("Default/notes.txt"),
            b"not a whitelisted browser file",
        )
        .unwrap();
        fs::write(webview.join(WEBVIEW_MARKER), WEBVIEW_MARKER_CONTENT).unwrap();
        clean_runtime(root.path(), Some(&[])).unwrap();
        assert!(!webview.exists());
    }

    #[test]
    fn clean_runtime_keeps_profile_without_creation_marker() {
        let root = tempfile::tempdir().unwrap();
        let webview = root.path().join("webview");
        let profile = webview.join("EBWebView");
        fs::create_dir_all(profile.join("Default")).unwrap();
        fs::write(profile.join("Default/notes.txt"), b"unowned").unwrap();
        clean_runtime(root.path(), Some(&[])).unwrap();
        assert!(profile.join("Default/notes.txt").exists());
        assert!(!webview.join(WEBVIEW_MARKER).exists());
    }

    #[test]
    fn clean_runtime_keeps_profile_with_foreign_marker() {
        let root = tempfile::tempdir().unwrap();
        let webview = root.path().join("webview");
        let profile = webview.join("EBWebView");
        fs::create_dir_all(profile.join("Default")).unwrap();
        fs::write(profile.join("Default/notes.txt"), b"unowned").unwrap();
        fs::write(webview.join(WEBVIEW_MARKER), b"another app").unwrap();
        clean_runtime(root.path(), Some(&[])).unwrap();
        assert!(profile.join("Default/notes.txt").exists());
    }
}
