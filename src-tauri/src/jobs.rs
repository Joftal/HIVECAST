//! 后处理任务登记表：转封装、合并、弹幕导出都是几百毫秒到几分钟的 ffmpeg 调用，
//! 关闭程序时要能列出它们，所以命令层进来先登记、拿到 guard，函数返回即自动注销。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hivecast_ipc::ActiveTask;

#[derive(Debug, Clone)]
struct Job {
    id: u64,
    kind: String,
    title: String,
    detail: Option<String>,
}

#[derive(Default)]
pub struct Jobs {
    inner: Mutex<Vec<Job>>,
    next: AtomicU64,
}

impl Jobs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 登记一个任务；guard 在离开作用域时自动注销，命令中途报错也不会漏。
    pub fn begin(
        self: &Arc<Self>,
        kind: &str,
        title: impl Into<String>,
        detail: Option<String>,
    ) -> JobGuard {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Job {
            id,
            kind: kind.to_string(),
            title: title.into(),
            detail,
        };
        if let Ok(mut guard) = self.inner.lock() {
            tracing::debug!(id, kind = %job.kind, title = %job.title, "登记后台任务");
            guard.push(job);
        }
        JobGuard {
            jobs: self.clone(),
            id,
        }
    }

    pub fn list(&self) -> Vec<ActiveTask> {
        let Ok(guard) = self.inner.lock() else {
            return Vec::new();
        };
        guard
            .iter()
            .map(|job| ActiveTask {
                id: format!("job:{}", job.id),
                kind: job.kind.clone(),
                title: job.title.clone(),
                detail: job.detail.clone(),
            })
            .collect()
    }

    fn finish(&self, id: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.retain(|job| job.id != id);
            tracing::debug!(id, remaining = guard.len(), "注销后台任务");
        }
    }
}

pub struct JobGuard {
    jobs: Arc<Jobs>,
    id: u64,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.jobs.finish(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_registers_and_unregisters() {
        let jobs = Jobs::new();
        assert_eq!(jobs.list().len(), 0);
        {
            let _first = jobs.begin("remux", "a.flv", None);
            let _second = jobs.begin("merge", "b.flv", Some("3 段".into()));
            assert_eq!(jobs.list().len(), 2);
            let list = jobs.list();
            assert_eq!(list[0].kind, "remux");
            assert_eq!(list[1].detail.as_deref(), Some("3 段"));
            assert!(list[0].id != list[1].id, "任务 id 必须唯一");
        }
        assert!(jobs.list().is_empty(), "guard 离开作用域要自动注销");
    }

    #[test]
    fn kind_is_preserved() {
        let jobs = Jobs::new();
        let guard = jobs.begin("danmaku", "x.jsonl", None);
        assert_eq!(jobs.list()[0].kind, "danmaku");
        drop(guard);
        assert!(jobs.list().is_empty());
    }
}
