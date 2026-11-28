//! Only clean a recognized profile and product runtime paths. A missing or unreadable
//! profile is never evidence that a different profile should be deleted.
use rusqlite::{Connection, OpenFlags};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

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
                        // Treat a browser profile as opaque. Only the directory allocated
                        // by this app is disposable. Imported/legacy trees stay in place.
                        if regular(&dir.join(".hivecast-webview"), false)
                            && fs::read(dir.join(".hivecast-webview"))? == b"HIVECAST WebView2 v1"
                            && !contains_user_content(&path)?
                        {
                            fs::remove_dir_all(&path)?;
                        }
                    }
                    _ => {}
                }
                remove_empty(&path)?;
            } else if parent == "logs" && regular(&path, false) && is_app_log(&name) {
                fs::remove_file(&path)?;
            }
        }
        if parent == "webview"
            && !dir.join("EBWebView").exists()
            && regular(&dir.join(".hivecast-webview"), false)
            && fs::read(dir.join(".hivecast-webview"))? == b"HIVECAST WebView2 v1"
        {
            fs::remove_file(dir.join(".hivecast-webview"))?;
        }
        remove_empty(&dir)?;
    }
    Ok(())
}

fn contains_user_content(dir: &Path) -> io::Result<bool> {
    let mut pending = vec![dir.to_owned()];
    while let Some(parent) = pending.pop() {
        for entry in fs::read_dir(parent)? {
            let path = entry?.path();
            if regular(&path, true) {
                pending.push(path);
                continue;
            }
            if !regular(&path, false) {
                // A file removed between read_dir and inspection is not user content.
                if fs::symlink_metadata(&path).is_err() {
                    continue;
                }
                return Ok(true);
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                return Ok(true);
            };
            if !browser_generated_file(name, &path) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// Chromium cache directories store opaque, regenerable blobs whose names vary by
// runtime version. They are not evidence of user files.
fn browser_cache_file(path: &Path) -> bool {
    const CACHE_DIRS: &[&str] = &[
        "Cache",
        "Code Cache",
        "DawnCache",
        "DawnGraphiteCache",
        "DawnWebGPUCache",
        "GPUCache",
        "GPUPersistentCache",
        "GraphiteDawnCache",
        "GrShaderCache",
        "ShaderCache",
        "Service Worker",
        "CacheStorage",
    ];
    path.ancestors().skip(1).any(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| CACHE_DIRS.contains(&name))
    })
}

// WebView2 owns an opaque profile. Keep it intact when a new runtime version
// creates unfamiliar files or a user stores other content alongside browser data.
fn browser_generated_file(name: &str, path: &Path) -> bool {
    if browser_cache_file(path) {
        return true;
    }

    const KNOWN: &[&str] = &[
        "Last Version",
        "Local State",
        "Variations",
        "VariationsRuntimeSeedV2",
        "VariationsSafeSeedV2",
        "VariationsSeedV2",
        "BookmarkMergedSurfaceOrdering",
        "declarative_performance_observer.db",
        "DIPS",
        "ExtensionActivityEdge",
        "Favicons",
        "favorites_diagnostic.log",
        "History",
        "LOCK",
        "LOG",
        "LOG.old",
        "Login Data",
        "Login Data For Account",
        "Network Action Predictor",
        "Preferences",
        "Preferences.bak",
        "QuotaManager",
        "README",
        "Secure Preferences",
        "Secure Preferences.bak",
        "ServerCertificate",
        "settings_diagnostic.log",
        "Top Sites",
        "TransportSecurity",
        "Visited Links",
        "Vpn Tokens",
        "Web Data",
        "CURRENT",
        "index",
        "the-real-index",
        "journal.baj",
        "snapshot.baf",
        "SessionRestoreLog",
        "Cookies",
        "Device Bound Sessions",
        "Network Persistent State",
        "NetworkDataMigrated",
        "Reporting and NEL",
        "SCT Auditing Pending Reports",
        "Sdch Dictionaries",
        "Trust Tokens",
        "Trust Token Key Commitments",
        "db",
        "cache.db",
        "metadata",
        "settings.dat",
        "throttle_store.dat",
    ];
    let base = ["-journal", "-shm", "-wal"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name);
    if name == "cache.journal" && path.ancestors().any(|p| p.ends_with("GPUPersistentCache")) {
        return true;
    }
    if KNOWN.contains(&base) {
        return true;
    }
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if name.strip_prefix("MANIFEST-").is_some_and(digits)
        || name.strip_prefix("data_").is_some_and(digits)
        || name
            .strip_suffix(".log")
            .or_else(|| name.strip_suffix(".ldb"))
            .is_some_and(digits)
        || name
            .strip_suffix("_0")
            .or_else(|| name.strip_suffix("_1"))
            .is_some_and(|s| s.len() == 16 && hex(s))
        || name
            .strip_prefix("BrowserMetrics-")
            .and_then(|s| s.strip_suffix(".pma"))
            .is_some_and(|s| s.split('-').all(hex))
        || name.len() == 64 && hex(name)
    {
        return true;
    }
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if matches!(parent, "extensions_crx_cache" | "component_crx_cache") {
        return name == "metadata.json";
    }
    if parent == "_metadata" {
        return name == "verified_contents.json";
    }
    parent.split('.').all(digits)
        && matches!(
            name,
            "manifest.json"
                | "protocols.json"
                | "crl-set"
                | "crs.pb"
                | "ct_config.pb"
                | "kp_pinslist.pb"
                | "keys.json"
                | "LICENSE"
                | "Microsoft.CognitiveServices.Speech.core.dll"
        )
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
    fn clean_runtime_removes_profile_with_unknown_cache_blobs() {
        let root = tempfile::tempdir().unwrap();
        let webview = root.path().join("webview");
        let profile = webview.join("EBWebView");
        let gpu = profile.join("Default/GPUCache");
        let code = profile.join("Default/Code Cache/js");
        fs::create_dir_all(&gpu).unwrap();
        fs::create_dir_all(&code).unwrap();
        fs::write(profile.join("Local State"), b"{}").unwrap();
        fs::write(gpu.join("f_000001"), b"cache").unwrap();
        fs::write(code.join("f_000002"), b"cache").unwrap();
        fs::write(webview.join(".hivecast-webview"), b"HIVECAST WebView2 v1").unwrap();
        clean_runtime(root.path(), Some(&[])).unwrap();
        assert!(!webview.exists());
    }

    #[test]
    fn clean_runtime_keeps_profile_with_unfamiliar_user_file() {
        let root = tempfile::tempdir().unwrap();
        let webview = root.path().join("webview");
        let profile = webview.join("EBWebView");
        fs::create_dir_all(profile.join("Default")).unwrap();
        fs::write(profile.join("Local State"), b"{}").unwrap();
        fs::write(profile.join("notes.txt"), b"user").unwrap();
        fs::write(webview.join(".hivecast-webview"), b"HIVECAST WebView2 v1").unwrap();
        clean_runtime(root.path(), Some(&[])).unwrap();
        assert!(profile.join("notes.txt").exists());
    }
}
