//! 任务中心：转封装、合并、弹幕导出、自动后处理都是几百毫秒到几分钟的 ffmpeg 调用。
//!
//! 运行中的任务放在内存里给退出拦截和任务页轮询；每次开始/进度/结束同时写 task_history，
//! 进程崩溃或强制退出后，下次启动会把遗留的 running 标成 interrupted，不会凭空消失。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hivecast_ipc::{ActiveTask, TaskBoard, TaskEvent, TaskRecord};

use crate::db::Db;

#[derive(Debug, Clone)]
struct Job {
    id: String,
    kind: String,
    title: String,
    detail: Option<String>,
    session_id: Option<String>,
    room_id: Option<String>,
    state: String,
    percent: i64,
    stage: Option<String>,
    message: Option<String>,
    started_at: i64,
    queued_at: i64,
    started_running_at: Option<i64>,
    /// 是否写入 task_history；启动恢复这类静默任务不参与时间线。
    persisted: bool,
    last_write_ms: i64,
    /// 上一条事件 (stage,state,进度桶)，用来去重 4 Hz 的进度回调。
    last_event_key: (String, String, i64),
}

impl Job {
    fn record(&self) -> TaskRecord {
        TaskRecord {
            id: self.id.clone(),
            kind: self.kind.clone(),
            title: self.title.clone(),
            detail: self.detail.clone(),
            state: self.state.clone(),
            percent: self.percent,
            stage: self.stage.clone(),
            message: self.message.clone(),
            session_id: self.session_id.clone(),
            room_id: self.room_id.clone(),
            started_at: self.started_at,
            queued_at: Some(self.queued_at),
            started_running_at: self.started_running_at,
            finished_at: None,
        }
    }

    fn run_id(&self) -> String {
        self.session_id
            .as_deref()
            .map(|id| format!("session:{id}"))
            .unwrap_or_else(|| format!("task:{}", self.id))
    }
}

#[derive(Default)]
pub struct Jobs {
    inner: Mutex<Vec<Job>>,
    next: AtomicU64,
    db: Mutex<Option<Arc<Db>>>,
}

impl Jobs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 状态初始化时注入数据库；单测保持不落库。
    pub fn attach_db(&self, db: Arc<Db>) {
        if let Ok(mut guard) = self.db.lock() {
            *guard = Some(db);
        }
    }

    fn db(&self) -> Option<Arc<Db>> {
        self.db.lock().ok().and_then(|guard| guard.clone())
    }

    /// 登记一个任务；guard 在离开作用域时自动注销，命令中途报错也不会漏。
    pub fn begin(
        self: &Arc<Self>,
        kind: &str,
        title: impl Into<String>,
        detail: Option<String>,
    ) -> JobGuard {
        self.begin_for(kind, title, detail, None, None)
    }

    pub fn begin_for(
        self: &Arc<Self>,
        kind: &str,
        title: impl Into<String>,
        detail: Option<String>,
        session_id: Option<String>,
        room_id: Option<String>,
    ) -> JobGuard {
        self.begin_inner(kind, title, detail, session_id, room_id, true)
    }

    /// 只进运行表、不写任务历史；启动恢复这类后台维护任务用。
    pub fn begin_silent(
        self: &Arc<Self>,
        kind: &str,
        title: impl Into<String>,
        detail: Option<String>,
    ) -> JobGuard {
        self.begin_inner(kind, title, detail, None, None, false)
    }

    fn begin_inner(
        self: &Arc<Self>,
        kind: &str,
        title: impl Into<String>,
        detail: Option<String>,
        session_id: Option<String>,
        room_id: Option<String>,
        persist: bool,
    ) -> JobGuard {
        let seq = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("job:{seq}");
        self.insert(
            Job {
                id: id.clone(),
                kind: kind.to_string(),
                title: title.into(),
                detail,
                session_id,
                room_id,
                state: "running".into(),
                percent: 0,
                stage: Some("queued".into()),
                message: None,
                started_at: now_ts(),
                queued_at: now_ts(),
                started_running_at: None,
                persisted: persist,
                last_write_ms: now_ms(),
                last_event_key: ("queued".into(), "queued".into(), 0),
            },
            persist,
        );
        JobGuard {
            jobs: self.clone(),
            id,
            finished: false,
        }
    }

    /// 用一个外部 id 登记任务（post::run 的 task id），重复调用只刷新标题。
    pub fn start_external(
        &self,
        id: &str,
        kind: &str,
        title: impl Into<String>,
        detail: Option<String>,
        session_id: Option<String>,
        room_id: Option<String>,
    ) {
        let title = title.into();
        let exists = self
            .inner
            .lock()
            .map(|guard| guard.iter().any(|job| job.id == id))
            .unwrap_or(false);
        if exists {
            return;
        }
        self.insert(
            Job {
                id: id.to_string(),
                kind: kind.to_string(),
                title,
                detail,
                session_id,
                room_id,
                state: "running".into(),
                percent: 0,
                stage: Some("queued".into()),
                message: None,
                started_at: now_ts(),
                queued_at: now_ts(),
                started_running_at: None,
                persisted: true,
                last_write_ms: 0,
                last_event_key: ("queued".into(), "queued".into(), 0),
            },
            true,
        );
    }

    /// 更新运行中任务；同一任务最多每秒落库一次，进度事件本身是 4 Hz。
    ///
    /// 阶段/状态转换与 25/50/75/100 里程碑才追加 task_event，避免把时间线冲成日志。
    pub fn progress(
        &self,
        id: &str,
        percent: i64,
        stage: &str,
        state: &str,
        message: Option<&str>,
    ) {
        let now_ms = now_ms();
        let now = now_ts();
        let mut to_persist = None;
        let mut event = None;
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(job) = guard.iter_mut().find(|job| job.id == id) {
                job.percent = percent.clamp(0, 100);
                job.stage = Some(stage.to_string());
                job.state = state.to_string();
                job.message = message.map(str::to_string);
                let started_edge =
                    state == "running" && stage != "queued" && job.started_running_at.is_none();
                if started_edge {
                    job.started_running_at = Some(now);
                }
                let bucket = job.percent / 25;
                let key = (stage.to_string(), state.to_string(), bucket);
                if key != job.last_event_key {
                    job.last_event_key = key;
                    event = Some((job.clone(), now));
                }
                if now_ms - job.last_write_ms >= 1_000 || job.percent == 100 || started_edge {
                    job.last_write_ms = now_ms;
                    to_persist = Some(job.record());
                }
            }
        }
        if let (Some(db), Some(record)) = (self.db(), to_persist) {
            if let Err(err) = db.task_progress(
                &record.id,
                record.percent,
                record.stage.as_deref(),
                &record.state,
                record.message.as_deref(),
                now,
            ) {
                tracing::warn!(task = %record.id, error = %err, "更新任务进度失败");
            }
        }
        if let (Some(db), Some((job, at))) = (self.db(), event) {
            if let Err(err) = db.task_event(&event_of(&job, &job.stage, &job.state, at)) {
                tracing::warn!(task = %job.id, error = %err, "写入任务事件失败");
            }
        }
    }

    /// 给某个 session 的 Run 追加一条阶段事件；异步子任务（缩略图）用它补时间线。
    /// 不写 task_history，也不会出现在运行中列表里。
    pub fn emit_session(
        &self,
        session_id: &str,
        kind: &str,
        stage: &str,
        state: &str,
        percent: i64,
        message: Option<&str>,
    ) {
        let Some(db) = self.db() else { return };
        let event = TaskEvent {
            id: 0,
            task_id: format!("phase:{kind}:{session_id}"),
            run_id: Some(format!("session:{session_id}")),
            kind: kind.to_string(),
            stage: stage.to_string(),
            state: state.to_string(),
            percent: percent.clamp(0, 100),
            message: message.map(str::to_string),
            at: now_ts(),
        };
        if let Err(err) = db.task_event(&event) {
            tracing::warn!(session = %session_id, stage, error = %err, "写入阶段事件失败");
        }
    }

    /// 结束一个外部任务：先从运行表移除，再落最终状态。
    pub fn finish_external(&self, id: &str, state: &str, message: Option<&str>) {
        self.finish(id, state, message);
    }

    fn finish(&self, id: &str, state: &str, message: Option<&str>) {
        let finished = self.inner.lock().ok().and_then(|mut guard| {
            let job = guard.iter().find(|job| job.id == id).cloned();
            guard.retain(|job| job.id != id);
            tracing::debug!(id, remaining = guard.len(), "注销后台任务");
            job
        });
        let now = now_ts();
        if let Some(db) = self.db() {
            if let Err(err) = db.task_finish(id, state, message, now) {
                tracing::warn!(task = id, error = %err, "写入任务结束状态失败");
            }
        }
        if let (Some(db), Some(mut job)) = (self.db(), finished) {
            if !job.persisted {
                return;
            }
            job.started_running_at =
                job.started_running_at
                    .or(if state == "running" { Some(now) } else { None });
            let stage = match state {
                "done" | "failed" | "cancelled" | "interrupted" => state,
                other => job.stage.as_deref().unwrap_or(other),
            };
            if state == "done" {
                job.percent = 100;
            }
            // progress 已经写过同一个终态（100% done / 0% failed 等）时不再追加，
            // 否则时间线尾部会出现两条一模一样的 done。
            let already_emitted = job.state == state && job.last_event_key.0 == stage;
            if !already_emitted {
                if let Err(err) =
                    db.task_event(&event_of(&job, &Some(stage.to_string()), state, now))
                {
                    tracing::warn!(task = id, error = %err, "写入任务结束事件失败");
                }
            }
        }
    }

    pub fn list(&self) -> Vec<ActiveTask> {
        let Ok(guard) = self.inner.lock() else {
            return Vec::new();
        };
        guard
            .iter()
            .map(|job| ActiveTask {
                id: job.id.clone(),
                kind: job.kind.clone(),
                title: job.title.clone(),
                detail: job.detail.clone(),
            })
            .collect()
    }

    /// 运行中 + 最近历史。运行中来自内存，历史来自数据库。
    pub fn board(&self, history: usize) -> TaskBoard {
        let running = self
            .inner
            .lock()
            .map(|guard| guard.iter().map(Job::record).collect())
            .unwrap_or_default();
        let history = self
            .db()
            .and_then(|db| db.task_history(history).ok())
            .unwrap_or_default();
        TaskBoard { running, history }
    }

    pub fn mark_interrupted(&self) {
        let Some(db) = self.db() else { return };
        match db.mark_tasks_interrupted() {
            Ok(count) if count > 0 => {
                tracing::info!(count, "任务中心：把上次未完成的任务标为 interrupted")
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "标记遗留任务失败"),
        }
    }

    fn insert(&self, job: Job, persist: bool) {
        if let Ok(mut guard) = self.inner.lock() {
            tracing::debug!(id = %job.id, kind = %job.kind, title = %job.title, "登记后台任务");
            guard.push(job.clone());
        }
        if persist {
            if let Some(db) = self.db() {
                let mut record = job.record();
                record.state = "running".into();
                if let Err(err) = db.task_start(&record) {
                    tracing::warn!(task = %job.id, error = %err, "写入任务记录失败");
                }
                if let Err(err) = db.task_event(&event_of(
                    &job,
                    &Some("queued".to_string()),
                    "queued",
                    job.queued_at,
                )) {
                    tracing::warn!(task = %job.id, error = %err, "写入任务入队事件失败");
                }
            }
        }
    }
}

pub struct JobGuard {
    jobs: Arc<Jobs>,
    id: String,
    finished: bool,
}

impl JobGuard {
    /// 给包装型任务推进阶段（录制收尾用它把「收尾中」显示出来）。
    pub fn progress(&self, percent: i64, stage: &str, message: Option<&str>) {
        self.jobs
            .progress(&self.id, percent, stage, "running", message);
    }

    pub fn finish(mut self, state: &str, message: Option<&str>) {
        self.jobs.finish(&self.id, state, message);
        self.finished = true;
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.jobs.finish(&self.id, "done", None);
    }
}

/// 事件统一从 Job 取快照，避免各处漏字段。
fn event_of(job: &Job, stage: &Option<String>, state: &str, at: i64) -> TaskEvent {
    TaskEvent {
        id: 0,
        task_id: job.id.clone(),
        run_id: Some(job.run_id()),
        kind: job.kind.clone(),
        stage: stage.clone().unwrap_or_else(|| "queued".into()),
        state: state.to_string(),
        percent: job.percent,
        message: job.message.clone(),
        at,
    }
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_millis() as i64)
        .unwrap_or(0)
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

    #[test]
    fn external_task_progress_and_finish() {
        let jobs = Jobs::new();
        jobs.start_external("manual:s1", "remux", "a.flv", None, Some("s1".into()), None);
        assert_eq!(jobs.list().len(), 1);
        jobs.progress("manual:s1", 42, "running", "running", None);
        assert_eq!(jobs.board(10).running[0].percent, 42);
        jobs.finish_external("manual:s1", "done", None);
        assert!(jobs.list().is_empty());
    }
}
