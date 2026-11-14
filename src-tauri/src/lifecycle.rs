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
/// 主窗口显示/隐藏：前端据此暂停只在界面上有意义的后台轮询（例如扫码登录）。
pub const EVENT_WINDOW_VISIBILITY: &str = "app://window-visibility";

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
    // 开录还没挂进 active 的那几百毫秒也算任务：此时退出会把刚拉起的 ffmpeg
    // 留在后台写文件（H6）。
    for room_id in state.recorder.starting_rooms() {
        let title = state
            .db
            .get_room(&room_id)
            .ok()
            .flatten()
            .map(|room| room.title)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| room_id.clone());
        tasks.push(ActiveTask {
            id: format!("record:starting:{room_id}"),
            kind: "record".into(),
            title,
            detail: Some("开录中".into()),
        });
    }
    tasks.extend(state.jobs.list());
    tasks
}

pub fn force_quit() -> bool {
    FORCE_QUIT.load(Ordering::SeqCst)
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!(target: "hivecast_lib::lifecycle", "唤回主窗口失败：窗口不存在");
        return;
    };
    if let Err(err) = window.show() {
        tracing::warn!(target: "hivecast_lib::lifecycle", error = %err, "显示主窗口失败");
    }
    if let Err(err) = window.unminimize() {
        tracing::warn!(target: "hivecast_lib::lifecycle", error = %err, "取消最小化主窗口失败");
    }
    if let Err(err) = window.set_focus() {
        tracing::warn!(target: "hivecast_lib::lifecycle", error = %err, "聚焦主窗口失败");
    }
    emit_visibility(app, true);
}

fn hide_main<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!(target: "hivecast_lib::lifecycle", "隐藏主窗口失败：窗口不存在");
        return;
    };
    if let Err(err) = window.hide() {
        tracing::warn!(target: "hivecast_lib::lifecycle", error = %err, "隐藏主窗口失败");
    }
    emit_visibility(app, false);
}

/// 窗口可能已经不在前台；事件发不出去也不能影响隐藏/显示本身。
fn emit_visibility<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    if let Err(err) = app.emit(EVENT_WINDOW_VISIBILITY, visible) {
        tracing::warn!(target: "hivecast_lib::lifecycle", visible, error = %err, "推送窗口可见性事件失败");
    }
}

/// 窗口的关闭按钮：按设置决定隐藏还是走退出流程。
pub fn on_close_requested<R: Runtime>(app: &AppHandle<R>, tray_ready: bool) {
    let state = app.state::<AppState>();
    let action = settings::load(&state.db).close_action;
    if tray_ready && action == CLOSE_TRAY {
        tracing::info!(target: "hivecast_lib::lifecycle", "关闭窗口，继续后台运行");
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
    tracing::info!(
        target: "hivecast_lib::lifecycle",
        tasks = tasks.len(),
        "退出被未完成任务拦下"
    );
    // 窗口可能是藏起来的，先拿到前台再弹提示，否则用户看不到。
    show_main(app);
    if let Err(err) = app.emit(EVENT_CLOSE_BLOCKED, &tasks) {
        tracing::warn!(target: "hivecast_lib::lifecycle", error = %err, "推送退出拦截事件失败");
    }
    start_watch(app);
}

pub fn quit_now<R: Runtime>(app: &AppHandle<R>) {
    tracing::info!(target: "hivecast_lib::lifecycle", "应用退出");
    FORCE_QUIT.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// 前端点了「任务完成后退出」。
pub fn arm_auto_quit<R: Runtime>(app: &AppHandle<R>) {
    AUTO_QUIT.store(true, Ordering::SeqCst);
    tracing::info!(target: "hivecast_lib::lifecycle", "已安排任务完成后自动退出");
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
            tracing::info!(target: "hivecast_lib::lifecycle", "任务已清空，可以退出");
            if let Err(err) = handle.emit(EVENT_CLOSE_READY, ()) {
                tracing::warn!(target: "hivecast_lib::lifecycle", error = %err, "推送退出就绪事件失败");
            }
            if AUTO_QUIT.swap(false, Ordering::SeqCst) {
                quit_now(&handle);
            }
            return;
        }
    });
}
