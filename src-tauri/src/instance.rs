//! 单实例锁：两个进程同时录同一批房间会抢分片文件、抢缩略图缓存，
//! 还会各自起一份监控循环。用一把文件锁保证只有第一个进程干活（A7）。
//!
//! 锁随进程退出自动释放（flock/LockFileEx 都是这个语义），
//! 所以崩溃留下的 lock 文件不会把下次启动挡在门外。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs4::fs_std::FileExt;

/// 拿到锁就把它挂在 AppState 里保活；析构（进程退出）时自动释放。
pub struct InstanceLock {
    _file: File,
}

pub fn acquire(dir: &Path) -> io::Result<InstanceLock> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("instance.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    // try_lock_exclusive 用 Ok(false) 表示「锁在别人手里」，不是错误。
    if !file.try_lock_exclusive()? {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "另一个 HIVECAST 实例正持有该锁",
        ));
    }
    Ok(InstanceLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_refused() {
        let dir = std::env::temp_dir().join(format!("hivecast-lock-{}", std::process::id()));
        let first = acquire(&dir).expect("第一次加锁");
        assert!(acquire(&dir).is_err(), "同一目录第二次加锁必须失败");
        drop(first);
        assert!(acquire(&dir).is_ok(), "释放后应该能再加锁");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
