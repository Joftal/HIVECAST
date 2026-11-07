//! 子进程小工具：优雅停止与等待退出。
//!
//! 录制（recorder）与观看转封装（view）都要「先 SIGINT 让 ffmpeg flush 再兜底强杀」，
//! 所以把这套逻辑收在一处，避免两处各写一遍、行为漂移。

use std::time::{Duration, Instant};

use tokio::process::Child;

/// 优先 SIGINT：ffmpeg 会写完文件尾 / 分片索引再退出；拿不到 pid 才退化为 kill。
pub fn send_graceful(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            if unsafe { libc::kill(pid as i32, libc::SIGINT) } == 0 {
                return;
            }
            tracing::warn!(pid, error = %std::io::Error::last_os_error(), "发送 SIGINT 失败，改为强制结束");
            let _ = child.start_kill();
            return;
        }
    }
    tracing::debug!("子进程没有 pid，直接强制结束");
    let _ = child.start_kill();
}

/// 轮询等待退出，超时后强杀再等一次。
///
/// 返回 `Some(status)` 表示拿到了真实退出码；`None` 表示超时被强杀。
pub async fn wait_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "等待子进程退出失败");
                return None;
            }
        }
        if Instant::now() >= deadline {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
