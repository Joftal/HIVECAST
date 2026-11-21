//! 任务中心的聚合层：把「正在录制的一路」「后处理任务」「历史场次」拼成一条条 Run。
//!
//! 前端不直接面对三张互不相干的数据源（recorder 内存、jobs 内存/任务历史、record_session），
//! 这里统一成同一条链路：录制 → 收尾 → 后处理 → 入库。这样做的好处是任务页不需要知道
//! post::run 的 id 规则，也不会出现「录制在跑、转封装在跑，但页面只有两行互不相关的任务」。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use hivecast_ipc::{
    CredentialInfo, Incident, PlatformHealth, RecordSession, RoomRecord, RunRoom, RunView,
    StageView, SystemHealth, TaskCenter, TaskSummary, TaskView,
};

use crate::db::Db;
use crate::error::AppError;
use crate::jobs::Jobs;
use crate::monitor::MonitorStatusMap;
use crate::recorder::Recorder;

const PLATFORMS: [&str; 4] = ["bilibili", "douyu", "douyin", "huya"];
const DAY_SECS: i64 = 24 * 60 * 60;

/// 聚合任务中心首屏。history 只影响历史场次与任务历史的读取量。
pub fn build(
    db: &Db,
    recorder: &Recorder,
    jobs: &Jobs,
    statuses: &MonitorStatusMap,
    ffmpeg_path: Option<&str>,
    media_root: &Path,
    history: usize,
) -> Result<TaskCenter, AppError> {
    let now = now_ts();
    let history = history.clamp(1, 1_000);

    // 房间缓存：主列表只含未移除的房间；历史场次所属房间可能已经移除，需要按 id 补查。
    let mut room_cache: HashMap<String, RoomRecord> = db
        .list_rooms()?
        .into_iter()
        .map(|room| (room.id.clone(), room))
        .collect();

    let sessions = load_sessions(db, history)?;
    let mut session_cache: HashMap<String, RecordSession> = HashMap::new();
    // 分割录制的一场直播会写成多条 record_session；任务中心必须按 group_id
    // 聚成一条 Run，否则三个片段会出现三张卡、三个「最近完成」。
    let mut group_cache: HashMap<String, Vec<RecordSession>> = HashMap::new();
    for session in &sessions {
        session_cache.insert(session.id.clone(), session.clone());
        group_cache
            .entry(session_group_key(session))
            .or_default()
            .push(session.clone());
    }

    // 任务历史一次多取一些：一个 Run 可能包含多个片段任务，只取 history 条会把
    // 最近 Run 的片段截断。运行中的任务永远来自内存，不受这个上限影响。
    let board = jobs.board((history * 4).clamp(200, 1600));
    let credentials = crate::account::list(db).unwrap_or_default();

    let mut drafts: HashMap<String, RunDraft> = HashMap::new();

    // 1. 正在录制 / 开录中的房间先建 Run。开录中还没有 session，用 room id 做临时键。
    for recording in recorder.active_all() {
        let room = room_of(db, &mut room_cache, &recording.room_id);
        let group_key = session_cache
            .get(&recording.session_id)
            .map(session_group_key)
            .unwrap_or_else(|| recording.session_id.clone());
        let key = format!("session:{group_key}");
        let draft = drafts
            .entry(key.clone())
            .or_insert_with(|| RunDraft::new(key));
        draft.room = room;
        draft.session = session_cache.get(&recording.session_id).cloned();
        draft.sessions = group_cache.get(&group_key).cloned().unwrap_or_default();
        let title = draft
            .room
            .as_ref()
            .map(|room| room.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| recording.room_id.clone());
        draft.record_task = Some(TaskView {
            id: format!("record:{}", recording.session_id),
            kind: "record".into(),
            title,
            detail: Some(recording.quality),
            state: "running".into(),
            stage: Some("recording".into()),
            percent: 0,
            message: None,
            session_id: Some(recording.session_id.clone()),
            room_id: Some(recording.room_id.clone()),
            started_at: recording.started_at,
            finished_at: None,
            clip_index: None,
            clip_count: 0,
            queue: "record".into(),
            queued_at: None,
        });
        draft.started_at = recording.started_at;
    }

    for room_id in recorder.starting_rooms() {
        let room = room_of(db, &mut room_cache, &room_id);
        let key = format!("starting:{room_id}");
        let draft = drafts
            .entry(key.clone())
            .or_insert_with(|| RunDraft::new(key));
        draft.room = room;
        let title = draft
            .room
            .as_ref()
            .map(|room| room.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| room_id.clone());
        draft.record_task = Some(TaskView {
            id: format!("record:starting:{room_id}"),
            kind: "record".into(),
            title,
            detail: None,
            state: "running".into(),
            stage: Some("starting".into()),
            percent: 0,
            message: None,
            session_id: None,
            room_id: Some(room_id.clone()),
            started_at: now,
            finished_at: None,
            clip_index: None,
            clip_count: 0,
            queue: "record".into(),
            queued_at: None,
        });
        draft.started_at = now;
    }

    // 2. 所有处于 recording 的 session 都要有 Run：录制进程刚退出、正在收尾时
    //    recorder 的 active 已经摘掉，但 session 仍然是 recording，不能让它从页面消失。
    let mut seen_groups: HashSet<String> = HashSet::new();
    for session in &sessions {
        if session.status != "recording" {
            continue;
        }
        let group_key = session_group_key(session);
        if !seen_groups.insert(group_key.clone()) {
            continue;
        }
        let members = group_cache.get(&group_key).cloned().unwrap_or_default();
        let key = format!("session:{group_key}");
        let draft = drafts
            .entry(key.clone())
            .or_insert_with(|| RunDraft::new(key));
        draft.room = room_of(db, &mut room_cache, &session.room_id);
        draft.session = members
            .iter()
            .find(|member| member.status == "recording")
            .cloned()
            .or_else(|| Some(session.clone()));
        draft.sessions = members;
        draft.started_at = draft
            .sessions
            .iter()
            .map(|member| member.started_at)
            .min()
            .unwrap_or(session.started_at);
    }

    // 3. 历史场次：按场（group）倒序取满 history 条。最新的失败/中断也在这批里。
    let mut history_sessions: Vec<RecordSession> = Vec::new();
    let mut seen_history_groups: HashSet<String> = HashSet::new();
    let mut closed_group_count = 0usize;
    for session in &sessions {
        if session.status == "recording" {
            continue;
        }
        let group_key = session_group_key(session);
        if !seen_history_groups.insert(group_key.clone()) {
            continue;
        }
        let members = group_cache.get(&group_key).cloned().unwrap_or_default();
        if members.iter().any(|member| member.status == "recording") {
            continue;
        }
        // 先记数再判断是否截断：正好多出的这一组用来告诉前端「还有更多」。
        closed_group_count += 1;
        if history_sessions.len() >= history {
            break;
        }
        let key = format!("session:{group_key}");
        let draft = drafts
            .entry(key.clone())
            .or_insert_with(|| RunDraft::new(key));
        draft.room = room_of(db, &mut room_cache, &session.room_id);
        let representative = group_representative(&members).unwrap_or_else(|| session.clone());
        draft.session = Some(representative.clone());
        draft.sessions = members.clone();
        draft.started_at = members
            .iter()
            .map(|member| member.started_at)
            .min()
            .unwrap_or(session.started_at);
        draft.ended_at = members.iter().filter_map(|member| member.ended_at).max();
        history_sessions.push(representative);
    }

    // 4. 挂任务：同一 session 的转封装/重试归到对应 Run；合并与单独导出没有 session，
    //    各自成一条独立 Run。finalize 不是独立任务，只用于把阶段推进到「收尾」。
    for task in board.running.iter().chain(board.history.iter()) {
        let is_finalize = is_finalize(task);
        let session_key = task.session_id.as_deref().and_then(|id| {
            let group_key = session_cache
                .get(id)
                .map(session_group_key)
                .unwrap_or_else(|| id.to_string());
            let key = format!("session:{group_key}");
            drafts.contains_key(&key).then_some(key)
        });

        if is_finalize {
            if let Some(key) = session_key {
                if let Some(draft) = drafts.get_mut(&key) {
                    // 同一 session 可能留下多条收尾记录（崩溃恢复、手动重试），
                    // 状态由最新一条决定，见 finish_run。
                    draft.finalize_tasks.push(task_view(task));
                }
            }
            continue;
        }

        if let Some(key) = session_key {
            let draft = drafts.get_mut(&key).expect("session run 已存在");
            draft.tasks.push(task_view(task));
            continue;
        }

        // 没有 session 的运行任务：合并、单独导出弹幕，或历史被清理过的手动任务。
        let key = format!("task:{}", task.id);
        let draft = drafts
            .entry(key.clone())
            .or_insert_with(|| RunDraft::new(key));
        if task.kind == "merge" {
            draft.standalone_stage = Some("merging".into());
        } else if task.kind == "danmaku" {
            draft.standalone_stage = Some("exporting".into());
        } else {
            draft.standalone_stage = Some("postprocessing".into());
        }
        if draft.room.is_none() {
            if let Some(room_id) = task.room_id.as_deref() {
                draft.room = room_of(db, &mut room_cache, room_id);
            }
        }
        if task.state == "running" || task.state == "queued" {
            draft.started_at = task.started_at;
        } else {
            draft.ended_at = task.finished_at;
            draft.started_at = task.started_at;
        }
        draft.tasks.push(task_view(task));
    }

    // 5. 把 RunDraft 变成前端契约。任务列表按「录制 → 片段序号 → 开始时间」排，
    //    详情里就不会先看到第二段再看到第一段。
    let mut running = Vec::new();
    let mut history_runs = Vec::new();
    let mut queued = Vec::new();
    let mut queued_runs = 0i64;
    let mut all_runs: Vec<RunView> = drafts
        .into_values()
        .map(|draft| finish_run(draft, now))
        .collect();

    all_runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));

    // 一次性把这一屏 Run 的事件读出来再分发，避免每条 Run 单独查库。
    // 分割录制的片段任务/阶段事件仍按 session_id 落库；先把片段 id 映射回
    // group run，否则详情时间线只会显示首片段的事件。
    let run_ids: Vec<String> = all_runs.iter().map(|run| run.id.clone()).collect();
    let mut event_run_ids = run_ids.clone();
    let mut event_aliases: HashMap<String, String> = HashMap::new();
    for (group_key, members) in &group_cache {
        let run_id = format!("session:{group_key}");
        for member in members {
            let alias = format!("session:{}", member.id);
            if !event_run_ids.contains(&alias) {
                event_run_ids.push(alias.clone());
            }
            event_aliases.insert(alias, run_id.clone());
        }
    }
    let mut event_map: HashMap<String, Vec<hivecast_ipc::TaskEvent>> = HashMap::new();
    for event in db.task_events(&event_run_ids, 4_000).unwrap_or_default() {
        let Some(run_id) = event.run_id.as_deref() else {
            continue;
        };
        let target = event_aliases
            .get(run_id)
            .map(String::as_str)
            .unwrap_or(run_id);
        event_map.entry(target.to_string()).or_default().push(event);
    }
    for run in &mut all_runs {
        run.events = event_map.remove(&run.id).unwrap_or_default();
    }

    for run in all_runs {
        if matches!(
            run.state.as_str(),
            "recording" | "finalizing" | "running" | "queued"
        ) {
            if run.state == "queued" {
                queued_runs += 1;
            }
            for task in &run.tasks {
                if task.state == "queued" {
                    queued.push(task.clone());
                }
            }
            running.push(run);
        } else {
            history_runs.push(run);
        }
    }
    queued.sort_by_key(|task| task.queued_at.unwrap_or(task.started_at));

    let platforms = build_platforms(
        &room_cache,
        &sessions,
        &recorder.active_all(),
        statuses,
        &credentials,
    );

    let failed_24h = history_runs
        .iter()
        .filter(|run| {
            matches!(run.state.as_str(), "failed" | "partial")
                && run.ended_at.is_some_and(|at| now - at <= DAY_SECS)
        })
        .count() as i64;

    let incidents = build_incidents(
        &history_runs,
        &history_sessions,
        &room_cache,
        statuses,
        &credentials,
        ffmpeg_path,
        media_root,
        now,
    );

    let disk_free = crate::disk::free_bytes(media_root);
    let system = SystemHealth {
        ffmpeg_ok: ffmpeg_path.is_some(),
        disk_free_bytes: disk_free.map(|free| free.min(i64::MAX as u64) as i64),
        disk_low: disk_free.is_some_and(|free| free < 1024 * 1024 * 1024),
        media_root: media_root.display().to_string(),
    };

    let summary = TaskSummary {
        rooms: room_cache
            .values()
            .filter(|room| room.removed_at.is_none())
            .count() as i64,
        live: room_cache
            .values()
            .filter(|room| room.removed_at.is_none() && room.is_live)
            .count() as i64,
        // 含「开录中」：recorder.active 还没建立，但页面的 Run 已经显示为录制中。
        recording: running
            .iter()
            .filter(|run| run.state == "recording")
            .count() as i64,
        // 与页面的「排队」筛选一致：按 Run 计数，而不是按片段任务计数。
        queued: queued_runs,
        running: running.len() as i64,
        failed: failed_24h,
        incidents: incidents.len() as i64,
    };

    history_runs.sort_by(|a, b| {
        b.ended_at
            .unwrap_or(b.started_at)
            .cmp(&a.ended_at.unwrap_or(a.started_at))
    });
    history_runs.truncate(history);

    Ok(TaskCenter {
        generated_at: now,
        summary,
        platforms,
        system,
        running,
        queued,
        history: history_runs,
        history_has_more: closed_group_count > history,
        incidents,
    })
}

/// 一条 Run 的中间形态；所有数据源先往这里塞，最后统一算状态。
struct RunDraft {
    id: String,
    room: Option<RoomRecord>,
    session: Option<RecordSession>,
    /// 同一场直播的所有片段；普通录制只有一条。
    sessions: Vec<RecordSession>,
    record_task: Option<TaskView>,
    tasks: Vec<TaskView>,
    /// 收尾任务单独收集：它们不参与后处理成功/失败的聚合，但要在详情里可见、可重试。
    finalize_tasks: Vec<TaskView>,
    standalone_stage: Option<String>,
    finalizing: bool,
    finalize_finished: bool,
    finalize_failed: bool,
    finalize_message: Option<String>,
    started_at: i64,
    ended_at: Option<i64>,
}

impl RunDraft {
    fn new(id: String) -> Self {
        Self {
            id,
            room: None,
            session: None,
            sessions: Vec::new(),
            record_task: None,
            tasks: Vec::new(),
            finalize_tasks: Vec::new(),
            standalone_stage: None,
            finalizing: false,
            finalize_finished: false,
            finalize_failed: false,
            finalize_message: None,
            started_at: 0,
            ended_at: None,
        }
    }
}

fn finish_run(mut draft: RunDraft, now: i64) -> RunView {
    let group_members: Vec<RecordSession> = if draft.sessions.is_empty() {
        draft.session.clone().into_iter().collect()
    } else {
        draft.sessions.clone()
    };

    // 后处理重试成功后，session.post_state 会回到 done；同一 Run 里上一次失败的
    // post/remux 任务不应继续把整条 Run 标成失败。分组录制必须等所有片段都 done
    // 才隐藏失败，否则一个片段的成功重试会掩盖另一个片段的真实失败。
    let all_members_post_done = !group_members.is_empty()
        && group_members
            .iter()
            .all(|session| session.post_state == "done");
    if all_members_post_done {
        draft.tasks.retain(|task| {
            !(matches!(task.kind.as_str(), "post" | "remux") && task.state == "failed")
        });
    }

    // 收尾可能失败后重试：只认最新一条收尾任务。若用「或」合并，重试成功后
    // 旧的 failed 会永远把收尾阶段钉在失败。
    draft.finalize_tasks.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.finished_at.cmp(&b.finished_at))
    });
    // 详情只保留最新一次收尾尝试：旧的 failed 记录留在 task_history 里，
    // 若继续放进 Run.tasks，重试成功后详情仍会出现「收尾失败」和重试按钮。
    if draft.finalize_tasks.len() > 1 {
        let latest = draft
            .finalize_tasks
            .pop()
            .expect("finalize_tasks 刚检查过长度");
        draft.finalize_tasks.clear();
        draft.finalize_tasks.push(latest);
    }
    if let Some(task) = draft.finalize_tasks.last() {
        draft.finalizing = task.state == "running" || task.state == "queued";
        draft.finalize_finished = task.state == "done";
        draft.finalize_failed = matches!(task.state.as_str(), "failed" | "interrupted");
        draft.finalize_message = if draft.finalize_failed {
            task.message.clone()
        } else {
            None
        };
    }

    draft.tasks.sort_by(|a, b| {
        a.clip_index
            .unwrap_or(u32::MAX)
            .cmp(&b.clip_index.unwrap_or(u32::MAX))
            .then_with(|| a.started_at.cmp(&b.started_at))
    });

    // 同一 Run 的片段总数 = 最大的片段序号（post 任务从 1 开始编号）。
    let clip_count = draft
        .tasks
        .iter()
        .filter_map(|task| task.clip_index)
        .max()
        .unwrap_or(0)
        .max(if draft.tasks.is_empty() { 0 } else { 1 });
    for task in &mut draft.tasks {
        task.clip_count = clip_count;
    }

    let session_active = group_members
        .iter()
        .any(|session| session.status == "recording");
    // aborted 只表示「没有正常收尾」，磁盘满/掉盘要算失败；app_exit 单独算中断。
    let session_failed = group_members.iter().any(|session| {
        session.status == "failed"
            || (session.status == "aborted" && session.reason.as_deref() != Some("app_exit"))
    });
    let session_interrupted = group_members
        .iter()
        .any(|session| session.reason.as_deref() == Some("app_exit"));
    let session_cancelled = group_members
        .iter()
        .any(|session| session.status == "cancelled");
    let starting_active = draft
        .record_task
        .as_ref()
        .is_some_and(|task| is_active(task) && task.stage.as_deref() == Some("starting"));
    let record_active = draft
        .record_task
        .as_ref()
        .is_some_and(|task| is_active(task) && task.stage.as_deref() != Some("starting"));
    let processing_active = draft.tasks.iter().any(is_active);
    let has_queued = draft.tasks.iter().any(|task| task.state == "queued");
    let has_failed = draft.tasks.iter().any(|task| task.state == "failed");
    let has_done = draft.tasks.iter().any(|task| task.state == "done");
    let has_cancelled = draft.tasks.iter().any(|task| task.state == "cancelled");
    let has_interrupted = draft.tasks.iter().any(|task| task.state == "interrupted");
    let failed = has_failed || session_failed || draft.finalize_failed;

    // 录制、收尾、后处理是三种不同的活动态：任务页要能一眼看出「在录」还是「在转」。
    let state = if draft.finalizing {
        "finalizing"
    } else if record_active || session_active || starting_active {
        "recording"
    } else if processing_active {
        "running"
    } else if has_queued {
        "queued"
    } else if failed {
        // 有成功也有失败片段时是「部分失败」，不能整条 Run 记成成功或全失败。
        if has_failed && has_done && !session_failed && !draft.finalize_failed {
            "partial"
        } else {
            "failed"
        }
    } else if has_interrupted || session_interrupted {
        "interrupted"
    } else if has_cancelled || session_cancelled {
        "cancelled"
    } else {
        "done"
    };

    let stage = if state == "recording" {
        if starting_active && !record_active && !session_active {
            "starting".into()
        } else {
            "recording".into()
        }
    } else if state == "finalizing" {
        "finalizing".into()
    } else if state == "running" {
        if draft
            .record_task
            .as_ref()
            .is_some_and(|task| is_active(task) && task.stage.as_deref() != Some("starting"))
        {
            if draft.finalizing || !draft.tasks.is_empty() {
                current_processing_stage(&draft)
            } else {
                "recording".into()
            }
        } else if draft
            .record_task
            .as_ref()
            .is_some_and(|task| task.stage.as_deref() == Some("starting"))
        {
            "starting".into()
        } else if draft.finalizing {
            "finalizing".into()
        } else if draft.standalone_stage.is_some() {
            draft.standalone_stage.clone().unwrap()
        } else {
            current_processing_stage(&draft)
        }
    } else if state == "queued" {
        "queued".into()
    } else {
        state.into()
    };

    let session = draft.session.as_ref();
    let quality = session
        .and_then(|session| session.quality.clone())
        .or_else(|| {
            draft
                .record_task
                .as_ref()
                .and_then(|task| task.detail.clone())
        });
    let bytes: u64 = group_members
        .iter()
        .map(|member| {
            if member.status == "recording" {
                member
                    .file_path
                    .as_deref()
                    .map(|path| {
                        let path = Path::new(path);
                        let master = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                        master.max(crate::segment::parts_total(path))
                    })
                    .unwrap_or_else(|| member.file_size.max(0) as u64)
            } else {
                member.file_size.max(0) as u64
            }
        })
        .sum();
    let duration_ms: i64 = group_members
        .iter()
        .map(|member| {
            if member.status == "recording" {
                (now - member.started_at).max(0) * 1000
            } else {
                member.duration_ms.max(0)
            }
        })
        .sum();

    let room = draft.room.as_ref().map(|room| RunRoom {
        id: room.id.clone(),
        platform: room.platform.clone(),
        room_key: room.room_key.clone(),
        host_name: room.host_name.clone(),
        title: room.title.clone(),
        avatar_url: room.avatar_url.clone(),
        is_live: room.is_live,
    });

    let stages = if draft.standalone_stage.is_some() {
        vec![StageView {
            key: draft.standalone_stage.clone().unwrap(),
            state: state.to_string(),
            detail: None,
            percent: draft
                .tasks
                .first()
                .map(|task| task.percent)
                .filter(|percent| *percent > 0),
        }]
    } else {
        build_stages(&draft, state, session_active, starting_active)
    };

    // 任务列表顺序与链路一致：录制 → 收尾 → 后处理。
    let mut tasks = Vec::with_capacity(
        draft.tasks.len() + draft.finalize_tasks.len() + usize::from(draft.record_task.is_some()),
    );
    if let Some(record_task) = draft.record_task {
        tasks.push(record_task);
    }
    tasks.extend(draft.finalize_tasks);
    tasks.extend(draft.tasks);

    RunView {
        id: draft.id,
        room,
        state: state.into(),
        stage,
        started_at: draft.started_at,
        ended_at: draft.ended_at.or_else(|| {
            if group_members
                .iter()
                .any(|member| member.status == "recording")
            {
                None
            } else {
                group_members
                    .iter()
                    .filter_map(|member| member.ended_at)
                    .max()
            }
        }),
        duration_ms,
        bytes: bytes.min(i64::MAX as u64) as i64,
        quality,
        tasks,
        stages,
        events: Vec::new(),
    }
}

fn current_processing_stage(draft: &RunDraft) -> String {
    if let Some(stage) = draft
        .tasks
        .iter()
        .find(|task| is_active(task))
        .and_then(|task| task.stage.clone())
    {
        return stage;
    }
    if draft.finalizing {
        return "finalizing".into();
    }
    if draft.tasks.iter().any(|task| task.state == "queued") {
        return "queued".into();
    }
    draft
        .standalone_stage
        .clone()
        .unwrap_or_else(|| "postprocessing".into())
}

fn build_stages(
    draft: &RunDraft,
    run_state: &str,
    session_active: bool,
    starting_active: bool,
) -> Vec<StageView> {
    let members: Vec<&RecordSession> = if draft.sessions.is_empty() {
        draft.session.iter().collect()
    } else {
        draft.sessions.iter().collect()
    };
    let any_failed = members.iter().any(|session| {
        session.status == "failed"
            || (session.status == "aborted" && session.reason.as_deref() != Some("app_exit"))
    });
    let all_ended = !members.is_empty()
        && members
            .iter()
            .all(|session| session.status != "recording" && session.ended_at.is_some());
    let abnormal_abort = members.iter().any(|session| {
        session.status == "aborted" && session.reason.as_deref() != Some("app_exit")
    });
    let interrupted_abort = members.iter().any(|session| {
        session.status == "aborted" && session.reason.as_deref() == Some("app_exit")
    });
    let record_state = if any_failed {
        "failed"
    } else if session_active || draft.record_task.as_ref().is_some_and(is_active) {
        // recorder 可能已经把 active 摘掉，但 session 仍是 recording（正在收尾）；
        // 这时候录制阶段不能因为 record_task 不存在就显示成「已跳过」。
        "active"
    } else if all_ended || draft.ended_at.is_some() {
        "done"
    } else if draft.record_task.is_some() {
        "active"
    } else {
        "skipped"
    };

    let finalize_state = if draft.finalizing {
        "active"
    } else if draft.finalize_failed {
        "failed"
    } else if draft.finalize_finished {
        "done"
    } else if abnormal_abort {
        // 磁盘满/掉盘等异常中断没有正常收尾，不能因为 session 已有结束时间就报「完成」。
        "failed"
    } else if interrupted_abort {
        "skipped"
    } else if all_ended {
        "done"
    } else if session_active || starting_active {
        "pending"
    } else {
        let _ = run_state;
        "skipped"
    };

    let post_tasks: Vec<&TaskView> = draft
        .tasks
        .iter()
        .filter(|task| task.kind == "post" || task.kind == "remux")
        .collect();
    let member_post_failed = members.iter().any(|session| session.post_state == "failed");
    let member_post_done =
        !members.is_empty() && members.iter().all(|session| session.post_state == "done");
    let all_source =
        !members.is_empty() && members.iter().all(|session| session.post_mode == "source");
    let post_state = if post_tasks.iter().any(|task| task.state == "failed") || member_post_failed {
        "failed"
    } else if post_tasks.iter().any(|task| is_active(task)) {
        "active"
    } else if post_tasks.iter().any(|task| task.state == "cancelled") {
        "cancelled"
    } else if !post_tasks.is_empty() && post_tasks.iter().all(|task| task.state == "done") {
        "done"
    } else if member_post_done {
        if all_source {
            "skipped"
        } else {
            "done"
        }
    } else if session_active || starting_active {
        "pending"
    } else if members.is_empty() {
        "skipped"
    } else {
        "pending"
    };

    // 异常中断（磁盘满/掉盘）时文件可能已经部分落库，入库只能算「部分完成」；
    // 完全没有产物才算失败。正常结束仍然走 done。
    let normal_commit = if abnormal_abort {
        if members.iter().any(|session| {
            session.file_size > 0
                || session
                    .file_path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).is_file())
        }) {
            "partial"
        } else {
            "failed"
        }
    } else {
        "done"
    };
    let commit_state = if member_post_failed || post_tasks.iter().any(|task| task.state == "failed")
    {
        "pending"
    } else if member_post_done {
        normal_commit
    } else if post_state == "skipped" && all_ended {
        normal_commit
    } else if abnormal_abort && all_ended {
        // 异常中断的场次不会再有正常的后处理/入库，直接给它最终态。
        normal_commit
    } else if post_state == "active"
        && post_tasks.iter().any(|task| {
            matches!(
                task.stage.as_deref(),
                Some("validating") | Some("committing")
            )
        })
    {
        "active"
    } else {
        "pending"
    };

    let session = draft.session.as_ref().or_else(|| members.first().copied());
    let record_detail = session
        .and_then(|session| session.quality.clone())
        .or_else(|| {
            draft
                .record_task
                .as_ref()
                .and_then(|task| task.detail.clone())
        });
    let post_detail = session.map(|session| session.post_mode.clone());
    let commit_detail = session.and_then(|session| session.file_path.clone());

    vec![
        StageView {
            key: "record".into(),
            state: record_state.into(),
            detail: record_detail,
            percent: None,
        },
        StageView {
            key: "finalize".into(),
            state: finalize_state.into(),
            detail: draft.finalize_message.clone(),
            percent: None,
        },
        StageView {
            key: "post".into(),
            state: post_state.into(),
            detail: post_detail,
            percent: None,
        },
        StageView {
            key: "commit".into(),
            state: commit_state.into(),
            detail: commit_detail,
            percent: None,
        },
    ]
}

fn build_platforms(
    rooms: &HashMap<String, RoomRecord>,
    _sessions: &[RecordSession],
    recordings: &[hivecast_ipc::RecordingInfo],
    statuses: &MonitorStatusMap,
    credentials: &[CredentialInfo],
) -> Vec<PlatformHealth> {
    let statuses = statuses
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let recording_rooms: HashSet<&str> =
        recordings.iter().map(|rec| rec.room_id.as_str()).collect();

    PLATFORMS
        .iter()
        .map(|platform| {
            let list: Vec<&RoomRecord> = rooms
                .values()
                .filter(|room| room.removed_at.is_none() && room.platform == *platform)
                .collect();
            let room_statuses: Vec<_> = list
                .iter()
                .filter_map(|room| statuses.get(&room.id))
                .collect();
            // 暂停监控的房间可能保留最后一次退避状态；它不应继续把平台卡染成警告。
            let active_statuses: Vec<_> = list
                .iter()
                .filter(|room| !room.monitor_paused)
                .filter_map(|room| statuses.get(&room.id))
                .collect();
            let auth = credentials
                .iter()
                .find(|credential| credential.platform == *platform)
                .map(|credential| credential.status.clone())
                .unwrap_or_else(|| "none".into());
            let backoff = active_statuses.iter().any(|status| status.backoff);
            let checking = active_statuses.iter().any(|status| status.inflight);
            let level = if auth == "expired" {
                "error"
            } else if backoff {
                "warn"
            } else if list.is_empty() {
                "idle"
            } else {
                "ok"
            };
            PlatformHealth {
                platform: platform.to_string(),
                auth,
                rooms: list.len() as i64,
                live: list.iter().filter(|room| room.is_live).count() as i64,
                recording: list
                    .iter()
                    .filter(|room| recording_rooms.contains(room.id.as_str()))
                    .count() as i64,
                paused: list.iter().filter(|room| room.monitor_paused).count() as i64,
                batch_rooms: list.iter().filter(|room| room.batch_follow).count() as i64,
                checking,
                backoff,
                last_checked_at: room_statuses
                    .iter()
                    .filter_map(|status| status.last_checked_at)
                    .max(),
                next_check_in_secs: active_statuses
                    .iter()
                    .map(|status| status.next_check_in_secs)
                    .filter(|secs| *secs > 0)
                    .min(),
                level: level.into(),
                message: None,
            }
        })
        .collect()
}

fn build_incidents(
    history: &[RunView],
    sessions: &[RecordSession],
    rooms: &HashMap<String, RoomRecord>,
    statuses: &MonitorStatusMap,
    credentials: &[CredentialInfo],
    ffmpeg_path: Option<&str>,
    media_root: &Path,
    now: i64,
) -> Vec<Incident> {
    let mut incidents: Vec<Incident> = Vec::new();
    let mut failed_task_sessions: HashSet<String> = HashSet::new();
    let statuses = statuses
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    for run in history {
        if run.ended_at.is_some_and(|at| now - at > DAY_SECS) {
            continue;
        }
        if !matches!(run.state.as_str(), "failed" | "partial") {
            continue;
        }
        if let Some(task) = run.tasks.iter().find(|task| task.state == "failed") {
            if let Some(session_id) = task.session_id.as_deref() {
                failed_task_sessions.insert(session_id.to_string());
            }
            incidents.push(Incident {
                id: format!("task:{}", task.id),
                code: "task_failed".into(),
                severity: "error".into(),
                scope: "task".into(),
                platform: run.room.as_ref().map(|room| room.platform.clone()),
                room_id: task.room_id.clone(),
                task_id: Some(task.id.clone()),
                title: task.title.clone(),
                detail: task.message.clone(),
                at: task.finished_at.unwrap_or(task.started_at),
                action: if task.kind == "finalize" && task.session_id.is_some() {
                    "retry_finalize".into()
                } else if task.session_id.is_some() {
                    "retry_post".into()
                } else {
                    "open_task".into()
                },
            });
        }
    }

    // 录制失败但同一场次没有任何后处理任务时，单独报一条；否则会和上面的
    // task_failed 重复。按 session 去重，不能按房间：同一房间的另一场失败
    // 不应把这一场的录制失败藏掉。
    for session in sessions {
        // 长录制可能超过 24 小时；失败是否「最近」要看结束时间，而不是开始时间。
        let failed_at = session.ended_at.unwrap_or(session.started_at);
        let record_failed = session.status == "failed"
            || (session.status == "aborted" && session.reason.as_deref() != Some("app_exit"));
        if !record_failed
            || failed_at < now - DAY_SECS
            || failed_task_sessions.contains(&session.id)
        {
            continue;
        }
        incidents.push(Incident {
            id: format!("record:{}", session.id),
            code: "record_failed".into(),
            severity: "error".into(),
            scope: "record".into(),
            platform: rooms
                .get(&session.room_id)
                .map(|room| room.platform.clone()),
            room_id: Some(session.room_id.clone()),
            task_id: None,
            title: session
                .quality
                .clone()
                .unwrap_or_else(|| session.id.clone()),
            detail: session.error.clone(),
            at: session.ended_at.unwrap_or(session.started_at),
            action: "open_room".into(),
        });
    }

    for status in statuses.values().filter(|status| status.backoff) {
        // 已移除 / 暂停监控的房间不应该继续占着「需要处理」；旧退避也不再提示。
        let Some(room) = rooms.get(&status.room_id) else {
            continue;
        };
        if room.removed_at.is_some() || room.monitor_paused {
            continue;
        }
        let at = status.last_checked_at.unwrap_or(now);
        if now - at > DAY_SECS {
            continue;
        }
        incidents.push(Incident {
            id: format!("backoff:{}", status.room_id),
            code: "monitor_backoff".into(),
            severity: "warn".into(),
            scope: "monitor".into(),
            platform: Some(room.platform.clone()),
            room_id: Some(status.room_id.clone()),
            task_id: None,
            title: String::new(),
            detail: status.last_outcome.clone(),
            at,
            action: "open_room".into(),
        });
    }

    for credential in credentials
        .iter()
        .filter(|credential| credential.status == "expired")
    {
        incidents.push(Incident {
            id: format!("auth:{}", credential.platform),
            code: "login_expired".into(),
            severity: "error".into(),
            scope: "platform".into(),
            platform: Some(credential.platform.clone()),
            room_id: None,
            task_id: None,
            title: credential.platform.clone(),
            detail: None,
            at: credential.updated_at,
            action: "open_account".into(),
        });
    }

    if ffmpeg_path.is_none() {
        incidents.push(Incident {
            id: "system:ffmpeg".into(),
            code: "ffmpeg_missing".into(),
            severity: "error".into(),
            scope: "system".into(),
            platform: None,
            room_id: None,
            task_id: None,
            title: "ffmpeg".into(),
            detail: None,
            at: now,
            action: "open_storage".into(),
        });
    }

    if let Some(free) = crate::disk::free_bytes(media_root) {
        if free < 1024 * 1024 * 1024 {
            incidents.push(Incident {
                id: "system:disk".into(),
                code: "disk_low".into(),
                severity: "warn".into(),
                scope: "system".into(),
                platform: None,
                room_id: None,
                task_id: None,
                title: media_root.display().to_string(),
                detail: Some(free.to_string()),
                at: now,
                action: "open_storage".into(),
            });
        }
    }

    // 同一个 id 只留一条：tasks 与 history 可能把同一次失败投影到两个来源。
    let mut seen = HashSet::new();
    incidents.retain(|incident| seen.insert(incident.id.clone()));
    incidents.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| b.at.cmp(&a.at))
    });
    incidents.truncate(8);
    incidents
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "error" => 0,
        "warn" => 1,
        _ => 2,
    }
}

fn task_view(task: &hivecast_ipc::TaskRecord) -> TaskView {
    let (clip_index, queue) = parse_task_id(&task.id, &task.kind);
    TaskView {
        id: task.id.clone(),
        kind: task.kind.clone(),
        title: task.title.clone(),
        detail: task.detail.clone(),
        state: task.state.clone(),
        stage: task.stage.clone(),
        percent: task.percent,
        message: task.message.clone(),
        session_id: task.session_id.clone(),
        room_id: task.room_id.clone(),
        started_at: task.started_at,
        finished_at: task.finished_at,
        clip_index,
        clip_count: 0,
        queue,
        queued_at: task.queued_at,
    }
}

/// 从 post::run 的 id 里恢复片段序号与进入队列的方式：
/// auto:{session}:{index}、auto:{session}:recover、manual:{session}、manual:{session}:retry。
fn parse_task_id(id: &str, kind: &str) -> (Option<u32>, String) {
    if id.starts_with("auto:") {
        let index = id
            .rsplit(':')
            .next()
            .and_then(|tail| tail.parse::<u32>().ok());
        return (index.map(|index| index + 1), "auto".into());
    }
    if id.starts_with("manual:") {
        return (None, "manual".into());
    }
    if kind == "merge" {
        return (None, "merge".into());
    }
    if kind == "record" {
        return (None, "record".into());
    }
    (None, "manual".into())
}

/// 分割录制的片段共享 group_id（首片段 id）；普通录制回退到自己的 id。
/// 分页读到足够的已结束场次为止，避免 history*4 的固定上限把长分割场截断。
///
/// 一场直播可能有很多片段；固定条数既可能不够，也可能一次读太多。这里按
/// 唯一 group 数收敛，正常库通常 1–3 页就停。硬上限防止异常数据拖慢 2 秒轮询。
fn load_sessions(db: &Db, history: usize) -> Result<Vec<RecordSession>, AppError> {
    const PAGE: usize = 200;
    const MAX_ROWS: usize = 5_000;
    let target = history.saturating_add(8);
    let mut sessions = Vec::new();
    let mut closed_seen: HashSet<String> = HashSet::new();
    let mut closed_order: Vec<String> = Vec::new();
    let mut capped = false;
    let mut offset = 0usize;
    loop {
        let batch = db.list_sessions(None, PAGE, offset)?;
        if batch.is_empty() {
            break;
        }
        for session in &batch {
            if session.status != "recording" {
                let key = session_group_key(session);
                if closed_seen.insert(key.clone()) {
                    closed_order.push(key);
                }
            }
        }
        let fetched = batch.len();
        sessions.extend(batch);
        if fetched < PAGE {
            break;
        }
        // 必须多读一个分组：目标分组可能正好跨页，读到下一组才能确认它已经完整。
        if closed_order.len() > target {
            break;
        }
        if sessions.len() >= MAX_ROWS {
            capped = true;
            tracing::warn!(
                rows = sessions.len(),
                groups = closed_order.len(),
                target,
                "任务中心历史分页达到上限，较早场次可能未纳入"
            );
            break;
        }
        offset += PAGE;
    }

    if capped {
        // 达到行数上限时最后一组可能被截断，宁可少一组，也不要把半场聚合成整场。
        if let Some(last) = closed_order.last() {
            sessions.retain(|session| {
                session.status == "recording" || session_group_key(session) != *last
            });
        }
    } else if closed_order.len() > target {
        trim_to_target_groups(&mut sessions, target);
    }
    Ok(sessions)
}

/// 只保留最前面的 target 个分组；多读的末尾分组可能不完整，直接丢弃。
fn trim_to_target_groups(sessions: &mut Vec<RecordSession>, target: usize) {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for session in sessions.iter().filter(|s| s.status != "recording") {
        let key = session_group_key(session);
        if seen.insert(key.clone()) {
            order.push(key);
        }
    }
    if order.len() <= target {
        return;
    }
    let keep: HashSet<String> = order.into_iter().take(target).collect();
    sessions.retain(|session| {
        session.status == "recording" || keep.contains(&session_group_key(session))
    });
}

fn session_group_key(session: &RecordSession) -> String {
    session
        .group_id
        .clone()
        .unwrap_or_else(|| session.id.clone())
}

/// 一场直播里优先拿失败/异常片段做代表，否则取最后一个结束的片段。
fn group_representative(members: &[RecordSession]) -> Option<RecordSession> {
    members
        .iter()
        .find(|session| {
            session.status == "failed"
                || (session.status == "aborted" && session.reason.as_deref() != Some("app_exit"))
        })
        .cloned()
        .or_else(|| {
            members
                .iter()
                .max_by_key(|session| session.ended_at.unwrap_or(session.started_at))
                .cloned()
        })
}

fn is_finalize(task: &hivecast_ipc::TaskRecord) -> bool {
    task.kind == "finalize" || (task.kind == "post" && task.id.starts_with("job:"))
}

/// 只有真正在占用槽位/进程的任务才算 active；排队中的任务由 has_queued 单独表达。
fn is_active(task: &TaskView) -> bool {
    task.state == "running"
}

fn room_of(db: &Db, cache: &mut HashMap<String, RoomRecord>, room_id: &str) -> Option<RoomRecord> {
    if let Some(room) = cache.get(room_id) {
        return Some(room.clone());
    }
    match db.get_room(room_id) {
        Ok(Some(room)) => {
            cache.insert(room_id.to_string(), room.clone());
            Some(room)
        }
        _ => None,
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|dur| dur.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, kind: &str, state: &str, clip: Option<u32>) -> TaskView {
        TaskView {
            id: id.into(),
            kind: kind.into(),
            title: id.into(),
            detail: None,
            state: state.into(),
            stage: Some(state.into()),
            percent: if state == "running" { 40 } else { 0 },
            message: None,
            session_id: Some("session-1".into()),
            room_id: Some("room-1".into()),
            started_at: 100,
            finished_at: None,
            clip_index: clip,
            clip_count: 0,
            queue: "auto".into(),
            queued_at: if state == "queued" { Some(100) } else { None },
        }
    }

    fn draft(tasks: Vec<TaskView>) -> RunDraft {
        let mut draft = RunDraft::new("session:session-1".into());
        draft.started_at = 100;
        draft.tasks = tasks;
        draft
    }

    #[test]
    fn parses_clip_index_and_queue_from_task_id() {
        assert_eq!(
            parse_task_id("auto:sess:0", "post"),
            (Some(1), "auto".into())
        );
        assert_eq!(
            parse_task_id("auto:sess:2", "post"),
            (Some(3), "auto".into())
        );
        assert_eq!(
            parse_task_id("auto:sess:recover", "post"),
            (None, "auto".into())
        );
        assert_eq!(
            parse_task_id("manual:sess", "remux"),
            (None, "manual".into())
        );
        assert_eq!(parse_task_id("merge:room", "merge"), (None, "merge".into()));
    }

    #[test]
    fn queued_task_states_a_queued_run() {
        let run = finish_run(
            draft(vec![task("auto:sess:0", "post", "queued", Some(1))]),
            200,
        );
        assert_eq!(run.state, "queued");
        assert_eq!(run.stage, "queued");
        // 排队不是「运行中」，否则任务页会把它画成有进度的活动 Run。
        assert!(run.tasks.iter().all(|task| !is_active(task)));
    }

    fn record(id: &str, kind: &str) -> hivecast_ipc::TaskRecord {
        hivecast_ipc::TaskRecord {
            id: id.into(),
            kind: kind.into(),
            title: id.into(),
            detail: None,
            state: "running".into(),
            percent: 0,
            stage: Some("finalizing".into()),
            message: None,
            session_id: Some("session-1".into()),
            room_id: Some("room-1".into()),
            started_at: 100,
            queued_at: Some(100),
            started_running_at: None,
            finished_at: None,
        }
    }

    #[test]
    fn finalize_tasks_are_recognized_and_never_listed_as_post_work() {
        assert!(is_finalize(&record("finalize:session-1", "finalize")));
        // 旧版本把收尾父任务写成 post + job: 前缀，升级后仍要识别。
        assert!(is_finalize(&record("job:legacy", "post")));
        // 片段后处理必须继续作为独立任务显示。
        assert!(!is_finalize(&record("auto:sess:0", "post")));
    }

    #[test]
    fn clip_count_uses_the_highest_clip_index() {
        let run = finish_run(
            draft(vec![
                task("auto:sess:0", "post", "done", Some(1)),
                task("auto:sess:2", "post", "running", Some(3)),
            ]),
            200,
        );
        assert_eq!(run.state, "running");
        assert!(run.tasks.iter().all(|task| task.clip_count == 3));
        // 任务按片段序号排，详情里不会先看到第三段。
        let clips: Vec<_> = run
            .tasks
            .iter()
            .filter_map(|task| task.clip_index)
            .collect();
        assert_eq!(clips, vec![1, 3]);
    }

    #[test]
    fn recording_session_without_active_task_keeps_record_stage_active() {
        let mut session = recorded_session("source");
        session.status = "recording".into();
        session.ended_at = None;
        let mut draft = RunDraft::new("session:session-1".into());
        draft.started_at = 100;
        draft.session = Some(session);
        let run = finish_run(draft, 200);

        assert_eq!(run.state, "recording");
        assert_eq!(run.stages[0].state, "active");
        assert_eq!(run.stages[1].state, "pending");
    }

    #[test]
    fn grouped_clips_aggregate_bytes_duration_and_end() {
        let mut first = recorded_session("done");
        first.group_id = Some("group-1".into());
        first.file_size = 1_000_000;
        first.duration_ms = 600_000;
        first.started_at = 100;
        first.ended_at = Some(700);
        let mut second = first.clone();
        second.id = "session-2".into();
        second.clip_index = 1;
        second.file_size = 2_000_000;
        second.duration_ms = 700_000;
        second.started_at = 700;
        second.ended_at = Some(1_400);

        let mut draft = RunDraft::new("session:group-1".into());
        draft.started_at = 100;
        draft.ended_at = Some(1_400);
        draft.session = Some(second.clone());
        draft.sessions = vec![first, second];
        let run = finish_run(draft, 1_500);

        assert_eq!(run.state, "done");
        assert_eq!(run.bytes, 3_000_000);
        assert_eq!(run.duration_ms, 1_300_000);
        assert_eq!(run.ended_at, Some(1_400));
    }

    #[test]
    fn aborted_disk_full_is_a_failed_run_and_record_stage() {
        let mut session = recorded_session("source");
        session.status = "aborted".into();
        session.reason = Some("disk_full".into());
        session.file_size = 3_000_000;
        let mut draft = RunDraft::new("session:session-1".into());
        draft.started_at = 100;
        draft.ended_at = Some(200);
        draft.session = Some(session);
        let run = finish_run(draft, 300);

        assert_eq!(run.state, "failed");
        assert_eq!(run.stages[0].state, "failed");
        assert_eq!(run.stages[1].state, "failed");
        // 已经写进库的字节还在，入库只能算部分完成，不能显示成完成。
        assert_eq!(run.stages[3].state, "partial");
    }

    #[test]
    fn latest_finalize_task_wins_over_an_older_failure() {
        let mut failed = task("finalize:session-1", "finalize", "failed", None);
        failed.started_at = 100;
        failed.finished_at = Some(150);
        failed.message = Some("合并失败".into());
        let mut retried = task("finalize:session-1:retry", "finalize", "done", None);
        retried.started_at = 200;
        retried.finished_at = Some(250);

        let mut draft = RunDraft::new("session:session-1".into());
        draft.started_at = 100;
        draft.ended_at = Some(200);
        draft.session = Some(recorded_session("done"));
        draft.finalize_tasks = vec![failed, retried];
        let run = finish_run(draft, 300);

        assert_eq!(run.state, "done");
        assert_eq!(run.stages[1].state, "done");
        // 只把最新一次收尾尝试放进详情，旧失败仍留在 task_history。
        assert_eq!(run.tasks.len(), 1);
        assert_eq!(run.tasks[0].kind, "finalize");
        assert_eq!(run.tasks[0].state, "done");
    }

    #[test]
    fn aborted_on_app_exit_is_interrupted_not_failed() {
        let mut session = recorded_session("source");
        session.status = "aborted".into();
        session.reason = Some("app_exit".into());
        let mut draft = RunDraft::new("session:session-1".into());
        draft.started_at = 100;
        draft.ended_at = Some(200);
        draft.session = Some(session);
        let run = finish_run(draft, 300);

        assert_eq!(run.state, "interrupted");
        assert_eq!(run.stages[0].state, "done");
        assert_eq!(run.stages[1].state, "skipped");
    }

    #[test]
    fn failed_finalize_task_is_a_failed_run_and_stage() {
        let mut draft = RunDraft::new("session:session-1".into());
        draft.started_at = 100;
        draft.ended_at = Some(200);
        draft.session = Some(recorded_session("source"));
        draft.finalize_failed = true;
        let run = finish_run(draft, 300);

        assert_eq!(run.state, "failed");
        assert_eq!(run.stages[1].state, "failed");
    }

    fn recorded_session(post_state: &str) -> RecordSession {
        RecordSession {
            id: "session-1".into(),
            room_id: "room-1".into(),
            started_at: 100,
            ended_at: Some(200),
            status: "finished".into(),
            reason: Some("stopped".into()),
            file_path: Some("/tmp/a.mp4".into()),
            file_size: 1,
            duration_ms: 100,
            quality: None,
            quality_history: Vec::new(),
            line: None,
            error: None,
            danmaku_path: None,
            danmaku_count: 0,
            source_path: Some("/tmp/a.flv".into()),
            post_mode: "convert".into(),
            post_state: post_state.into(),
            source_deleted: false,
            post_error: None,
            group_id: None,
            clip_index: 0,
        }
    }

    #[test]
    fn successful_post_retry_hides_the_previous_failure() {
        let mut draft = draft(vec![
            task("auto:sess:0", "post", "failed", Some(1)),
            task("manual:sess:retry", "post", "done", Some(1)),
        ]);
        draft.session = Some(recorded_session("done"));
        let run = finish_run(draft, 300);

        assert_eq!(run.state, "done");
        assert_eq!(run.tasks.len(), 1);
        assert_eq!(run.tasks[0].id, "manual:sess:retry");
    }

    #[test]
    fn grouped_post_retry_hides_failure_only_after_every_clip_is_done() {
        let mut failed_clip = recorded_session("failed");
        failed_clip.id = "s1".into();
        failed_clip.group_id = Some("g1".into());
        let mut done_clip = recorded_session("done");
        done_clip.id = "s2".into();
        done_clip.group_id = Some("g1".into());

        let mut failed_task = task("auto:s1:0", "post", "failed", Some(1));
        failed_task.session_id = Some("s1".into());

        let mut draft = RunDraft::new("session:g1".into());
        draft.started_at = 100;
        draft.ended_at = Some(200);
        draft.session = Some(done_clip.clone());
        draft.sessions = vec![failed_clip.clone(), done_clip.clone()];
        draft.tasks = vec![failed_task];
        let run = finish_run(draft, 300);
        // 另一个片段成功了，不能把这一段的失败藏掉。
        assert_eq!(run.state, "failed");
        assert!(run.tasks.iter().any(|task| task.state == "failed"));

        failed_clip.post_state = "done".into();
        let mut draft = RunDraft::new("session:g1".into());
        draft.started_at = 100;
        draft.ended_at = Some(200);
        draft.session = Some(done_clip.clone());
        draft.sessions = vec![failed_clip, done_clip];
        draft.tasks = vec![task("auto:s1:0", "post", "failed", Some(1))];
        let run = finish_run(draft, 300);
        assert_eq!(run.state, "done");
        assert!(run.tasks.iter().all(|task| task.state != "failed"));
    }

    #[test]
    fn trim_to_target_groups_drops_the_extra_partial_group() {
        let mut a1 = recorded_session("done");
        a1.id = "a1".into();
        a1.group_id = Some("a".into());
        let mut a2 = recorded_session("done");
        a2.id = "a2".into();
        a2.group_id = Some("a".into());
        let mut b = recorded_session("done");
        b.id = "b1".into();
        b.group_id = Some("b".into());
        let mut c = recorded_session("done");
        c.id = "c1".into();
        c.group_id = Some("c".into());
        let mut c_recording = recorded_session("source");
        c_recording.id = "c2".into();
        c_recording.group_id = Some("c".into());
        c_recording.status = "recording".into();
        c_recording.ended_at = None;

        let mut sessions = vec![a1, a2, b, c, c_recording];
        trim_to_target_groups(&mut sessions, 2);

        let ids: Vec<_> = sessions.iter().map(|session| session.id.as_str()).collect();
        // 两个完整分组保留，多读的 c 丢掉；正在录制的 session 不参与截断。
        assert_eq!(ids, vec!["a1", "a2", "b1", "c2"]);
    }

    #[test]
    fn starting_record_is_running_not_done() {
        let mut record = task("record:room-1", "record", "running", None);
        record.stage = Some("starting".into());
        record.session_id = None;
        let mut draft = RunDraft::new("task:record:room-1".into());
        draft.started_at = 100;
        draft.record_task = Some(record);

        let run = finish_run(draft, 150);
        assert_eq!(run.state, "recording");
        assert_eq!(run.stage, "starting");
        // 还没拿到 session，收尾/后处理/入库都应等待，而不是提前标成 skipped。
        assert_eq!(
            run.stages
                .iter()
                .map(|stage| (stage.key.as_str(), stage.state.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("record", "active"),
                ("finalize", "pending"),
                ("post", "pending"),
                ("commit", "pending"),
            ]
        );
    }

    #[test]
    fn failed_task_marks_the_run_failed() {
        let run = finish_run(
            draft(vec![task("auto:sess:0", "post", "failed", Some(1))]),
            200,
        );
        assert_eq!(run.state, "failed");
        assert_eq!(run.stage, "failed");
    }
}
