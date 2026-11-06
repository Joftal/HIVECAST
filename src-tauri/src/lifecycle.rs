//! 关闭与退出：窗口关闭 / 托盘退出 / Cmd+Q 都先经过这里。
//!
//! 规则：
//! - 设置里 `close_action = tray` 时，点窗口关闭只隐藏窗口，录制与监控继续跑；
//! - 真要退出时，如果还有录制 / 转封装 / 合并 / 弹幕导出没结束，就拦下来，
//!   把任务列表发给前端弹提示；用户可以选择「任务完成后自动退出」。
//! - 只有任务清空后才真正 `app.exit(0)`，避免 ffmpeg 被硬杀留下半截文件。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hivecast_ipc::ActiveTask;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::settings::{self, CLOSE_TRAY};
use crate::state::AppState;

/// 有任务挡着退出：payload 是 `ActiveTask[]`。
pub const EVENT_CLOSE_BLOCKED: &str = "app://close-blocked";
/// 任务已经全部结束，可以安全退出。
pub const EVENT_CLOSE_READY: &str = "app://close-ready";

static FORCE_QUIT: AtomicBool = AtomicBool::new(false);
static AUTO_QUIT: AtomicBool = AtomicBool::new(false);
static WATCHING: AtomicBool = AtomicBool::new(false);

/// 当前所有未结束的长任务：录制（含自动录制）与后处理队列。
pub fn active_tasks(state: &AppState) -> Vec<ActiveTask> {
    let mut tasks = Vec::new();
    for recording in state.recorder.active_all() {
        let room = state.db.get_room(&recording.room_id).ok().flatten();
        let title = room
            .as_ref()
            .map(|room| room.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| recording.room_id.clone());
        tasks.push(ActiveTask {
            id: format!("record:{}", recording.session_id),
            kind: "record".into(),
            title,
            detail: Some(recording.quality),
        });
    }
    tasks.extend(state.jobs.list());
    tasks
}

pub fn force_quit() -> bool {
    FORCE_QUIT.load(Ordering::SeqCst)
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// 窗口的关闭按钮：按设置决定隐藏还是走退出流程。
pub fn on_close_requested<R: Runtime>(app: &AppHandle<R>, tray_ready: bool) {
    let state = app.state::<AppState>();
    let action = settings::load(&state.db).close_action;
    if tray_ready && action == CLOSE_TRAY {
        hide_main(app);
        return;
    }
    quit_or_warn(app);
}

/// 托盘「退出」、Cmd+Q、Dock 退出：跳过托盘设置，直接按退出处理。
pub fn on_quit_requested<R: Runtime>(app: &AppHandle<R>) {
    quit_or_warn(app);
}

fn quit_or_warn<R: Runtime>(app: &AppHandle<R>) {
    let tasks = {
        let state = app.state::<AppState>();
        active_tasks(&state)
    };
    if tasks.is_empty() {
        quit_now(app);
        return;
    }
    // 窗口可能是藏起来的，先拿到前台再弹提示，否则用户看不到。
    show_main(app);
    let _ = app.emit(EVENT_CLOSE_BLOCKED, &tasks);
    start_watch(app);
}

pub fn quit_now<R: Runtime>(app: &AppHandle<R>) {
    FORCE_QUIT.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// 前端点了「任务完成后退出」。
pub fn arm_auto_quit<R: Runtime>(app: &AppHandle<R>) {
    AUTO_QUIT.store(true, Ordering::SeqCst);
    let tasks = {
        let state = app.state::<AppState>();
        active_tasks(&state)
    };
    if tasks.is_empty() {
        quit_now(app);
    } else {
        start_watch(app);
    }
}

pub fn cancel_auto_quit() {
    AUTO_QUIT.store(false, Ordering::SeqCst);
}

/// 800ms 轮询一次任务表；任务清空时通知前端，并在需要时自动退出。
fn start_watch<R: Runtime>(app: &AppHandle<R>) {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let tasks = {
                let state = handle.state::<AppState>();
                active_tasks(&state)
            };
            if !tasks.is_empty() {
                continue;
            }
            WATCHING.store(false, Ordering::SeqCst);
            let _ = handle.emit(EVENT_CLOSE_READY, ());
            if AUTO_QUIT.swap(false, Ordering::SeqCst) {
                quit_now(&handle);
            }
            return;
        }
    });
}