//! The installer records the payload digest. The app adds the selected profile
//! class, so an uninstall started from Explorer can respect a prior override.
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub const MARKER: &str = ".hivecast-install";

pub fn read(root: &Path) -> Option<(String, Option<String>)> {
    let path = root.join(MARKER);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > 80 {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let hash = lines.next()?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let class = lines.next();
    if class.is_some_and(|s| !matches!(s, "local" | "system" | "external"))
        || lines.next().is_some()
    {
        return None;
    }
    Some((hash.to_owned(), class.map(str::to_owned)))
}

pub fn content(hash: &str, class: Option<&str>) -> String {
    class.map_or_else(|| hash.to_owned(), |c| format!("{hash}\n{c}"))
}

#[allow(dead_code)] // Also included by the standalone Windows installer.
pub fn record_profile(root: &Path, class: &str) -> io::Result<()> {
    let Some((hash, previous)) = read(root) else {
        return Ok(());
    };
    if previous.as_deref() == Some(class) {
        return Ok(());
    }
    let mut next = tempfile::NamedTempFile::new_in(root)?;
    next.write_all(content(&hash, Some(class)).as_bytes())?;
    next.as_file().sync_all()?;
    next.persist(root.join(MARKER))?;
    Ok(())
}
