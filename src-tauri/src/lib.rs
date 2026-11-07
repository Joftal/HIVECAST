mod account;
mod broker;
mod commands;
mod credential;
mod danmaku;
mod db;
mod disk;
mod douyin_ws;
mod error;
mod ffmpeg;
mod filename;
mod instance;
mod jce;
mod jobs;
mod lifecycle;
mod local;
mod monitor;
mod platform;
mod postprocess;
mod power;
mod process;
mod recorder;
mod retention;
mod secret;
mod segment;
mod settings;
mod state;
mod stream;
mod thumbs;
mod tray;
mod view;

use state::AppState;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;
use tracing_subscriber::EnvFilter;

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,hivecast_lib=debug"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // 单实例锁：两个进程同时录同一批房间会抢分片、抢缩略图缓存（A7）。
            // 锁跟着进程走，崩溃遗留的 lock 文件不会挡住下次启动。
            let data_dir = handle
                .path()
                .app_data_dir()
                .map_err(|err| Box::<dyn std::error::Error>::from(err.to_string()))?;
            let lock = match crate::instance::acquire(&data_dir) {
                Ok(lock) => lock,
                Err(err) => {
                    tracing::warn!(%err, "已有实例在运行，本次启动退出");
                    eprintln!("HIVECAST 已在运行，本次启动退出");
                    let _ = handle
                        .notification()
                        .builder()
                        .title("HIVECAST")
                        .body("HIVECAST 已在运行，本次启动已退出")
                        .show();
                    // 给通知留出送达时间，再干净退出。
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    std::process::exit(0);
                }
            };
            app.manage(lock);

            // 启动阶段只做轻量初始化：建目录 + 打开数据库 + 绑定回环端口 + 起监控循环。
            let state = tauri::async_runtime::block_on(AppState::init(&handle))?;
            let media_fallback = state.media_fallback.clone();
            app.manage(state);
            // 媒体目录回落是「静默改了用户设置」，必须明确告诉用户（H2）。
            if let Some((configured, actual)) = media_fallback {
                let _ = handle
                    .notification()
                    .builder()
                    .title("HIVECAST 媒体目录已切换")
                    .body(format!(
                        "{configured} 当前不可用，新录像已改存到 {actual}；设置里已同步为后者"
                    ))
                    .show();
            }

            // 托盘：关闭窗口只隐藏，录制与监控继续跑。托盘建不出来就退回「关窗即退出」。
            let tray_ready = match crate::tray::build(&handle) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!("托盘创建失败：{err}");
                    false
                }
            };
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "windows")]
                {
                    let theme = window.theme().unwrap_or(tauri::Theme::Light);
                    crate::tray::sync_window_icon(&window, theme);
                }
                let target = window.clone();
                window.on_window_event(move |event| match event {
                    // 一律先拦下：是隐藏还是退出由设置与任务表决定。
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        crate::lifecycle::on_close_requested(target.app_handle(), tray_ready);
                    }
                    #[cfg(target_os = "windows")]
                    tauri::WindowEvent::ThemeChanged(theme) => {
                        crate::tray::sync_window_icon(&target, *theme);
                        crate::tray::refresh(target.app_handle());
                    }
                    _ => {}
                });
            }

            // dev 服务器重启 / 系统注销发的是 SIGTERM，它不会触发 RunEvent::Exit；
            // 不接住就会留下一堆还在写文件的 ffmpeg。
            #[cfg(unix)]
            {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let Ok(mut term) = signal(SignalKind::terminate()) else {
                        return;
                    };
                    term.recv().await;
                    tracing::info!("收到 SIGTERM，开始收尾");
                    let state = handle.state::<AppState>();
                    state.recorder.shutdown_all(&handle).await;
                    state.view.shutdown_all(&handle).await;
                    state.danmaku.shutdown_all();
                    // 走统一出口：置上 force 标记，避免 ExitRequested 再把退出拦下来。
                    crate::lifecycle::quit_now(&handle);
                });
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::demo_stream,
            commands::rooms_list,
            commands::room_add,
            commands::room_remove,
            commands::room_refresh,
            commands::room_set_auto_record,
            commands::room_set_quality,
            commands::room_lines,
            commands::room_set_line,
            commands::room_set_monitor,
            commands::record_start,
            commands::record_stop,
            commands::sessions_list,
            commands::view_slots,
            commands::view_open,
            commands::view_close,
            commands::view_replace,
            commands::view_suspend,
            commands::view_set_quality,
            commands::view_set_line,
            commands::view_retry,
            commands::session_remux,
            commands::session_export_danmaku,
            commands::room_merge_sessions,
            commands::sessions_merge,
            commands::settings_get,
            commands::settings_update,
            commands::filename_preview,
            commands::layout_get,
            commands::layout_set,
            commands::credentials_list,
            commands::credential_save,
            commands::credential_delete,
            commands::bilibili_qr_start,
            commands::bilibili_qr_poll,
            commands::media_list,
            commands::media_hosts,
            commands::media_delete,
            commands::media_reveal,
            commands::media_play,
            commands::media_stop,
            commands::active_tasks,
            commands::close_quit_now,
            commands::close_arm_quit,
            commands::close_cancel_quit
        ])
        .build(tauri::generate_context!())
        .expect("failed to build HIVECAST");

    app.run(|handle, event| {
        // Cmd+Q / Dock 退出也走同一套任务检查；只有 quit_now 置过 force 才放行。
        if let tauri::RunEvent::ExitRequested { api, .. } = &event {
            if !crate::lifecycle::force_quit() {
                let blocked = handle
                    .try_state::<AppState>()
                    .map(|state| !crate::lifecycle::active_tasks(&state).is_empty())
                    .unwrap_or(false);
                if blocked {
                    api.prevent_exit();
                    crate::lifecycle::on_quit_requested(handle);
                }
            }
        }
        // macOS：点 Dock 图标把藏起来的窗口拿回来。
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                tauri::async_runtime::block_on(async {
                    state.local.stop_all().await;
                    state.recorder.shutdown_all(handle).await;
                    state.view.shutdown_all(handle).await;
                    state.danmaku.shutdown_all();
                });
            }
        }
    });
}
