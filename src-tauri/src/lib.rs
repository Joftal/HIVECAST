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
mod logging;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "HIVECAST 进程启动");

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
                    if let Err(err) = handle
                        .notification()
                        .builder()
                        .title("HIVECAST")
                        .body("HIVECAST 已在运行，本次启动已退出")
                        .show()
                    {
                        tracing::warn!(%err, "系统通知发送失败");
                    }
                    // 给通知留出送达时间，再干净退出；文件日志也要落盘。
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    logging::shutdown();
                    std::process::exit(0);
                }
            };
            app.manage(lock);

            // 启动阶段只做轻量初始化：建目录 + 打开数据库 + 绑定回环端口 + 起监控循环。
            let state = match tauri::async_runtime::block_on(AppState::init(&handle)) {
                Ok(state) => state,
                Err(err) => {
                    tracing::error!(error = %err, "应用状态初始化失败，启动中止");
                    return Err(Box::<dyn std::error::Error>::from(err));
                }
            };
            let media_fallback = state.media_fallback.clone();
            let log_fallback = state.log_fallback.clone();
            app.manage(state);
            // 媒体目录回落是「静默改了用户设置」，必须明确告诉用户（H2）。
            if let Some((configured, actual)) = media_fallback {
                if let Err(err) = handle
                    .notification()
                    .builder()
                    .title("HIVECAST 媒体目录已切换")
                    .body(format!(
                        "{configured} 当前不可用，新录像已改存到 {actual}；设置里已同步为后者"
                    ))
                    .show()
                {
                    tracing::warn!(%err, "系统通知发送失败");
                }
            }
            // 日志目录与媒体目录不同：用户设置保留原值，只把实际写入目录降级。
            if let Some((configured, actual)) = log_fallback {
                if let Err(err) = handle
                    .notification()
                    .builder()
                    .title("HIVECAST 日志目录已切换")
                    .body(format!(
                        "{configured} 当前不可用，本次日志写入 {actual}；设置里仍保留原路径"
                    ))
                    .show()
                {
                    tracing::warn!(%err, "系统通知发送失败");
                }
            }

            {
                let state = app.state::<AppState>();
                tracing::info!(
                    target: "hivecast_lib",
                    version = env!("CARGO_PKG_VERSION"),
                    data_dir = %data_dir.display(),
                    media_root = %state.media_root.display(),
                    log_dir = %logging::status().dir,
                    "应用启动完成"
                );
            }

            // 运行中磁盘/权限变化也可能让日志目录失效：后台发现降级后补一条通知。
            let fallback_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    if let Some((configured, actual)) = logging::take_fallback() {
                        if let Err(err) = fallback_handle
                            .notification()
                            .builder()
                            .title("HIVECAST 日志目录已切换")
                            .body(format!(
                                "{configured} 当前不可写，日志已改写到 {actual}；设置里仍保留原路径"
                            ))
                            .show()
                        {
                            tracing::warn!(%err, "系统通知发送失败");
                        }
                    }
                }
            });

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
            commands::close_cancel_quit,
            commands::logs_status,
            commands::logs_open,
            commands::ui_log
        ])
        .build(tauri::generate_context!())
        .expect("failed to build HIVECAST");

    app.run(|handle, event| {
        // Cmd+Q / Dock 退出也走同一套任务检查；只有 quit_now 置过 force 才放行。
        if let tauri::RunEvent::ExitRequested { api, .. } = &event {
            if !crate::lifecycle::force_quit() {
                let tasks = handle
                    .try_state::<AppState>()
                    .map(|state| crate::lifecycle::active_tasks(&state))
                    .unwrap_or_default();
                if !tasks.is_empty() {
                    tracing::warn!(tasks = tasks.len(), "退出请求被拦下：仍有进行中的任务");
                    api.prevent_exit();
                    crate::lifecycle::on_quit_requested(handle);
                } else {
                    tracing::info!("退出请求放行：没有进行中的任务");
                }
            } else {
                tracing::info!("退出请求放行：已确认强制退出");
            }
        }
        // macOS：点 Dock 图标把藏起来的窗口拿回来。
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            tracing::info!("收到 macOS Reopen，重新显示主窗口");
            if let Some(window) = handle.get_webview_window("main") {
                if let Err(err) = window.show() {
                    tracing::warn!(error = %err, "重新显示主窗口失败");
                }
                if let Err(err) = window.set_focus() {
                    tracing::warn!(error = %err, "主窗口聚焦失败");
                }
            }
        }
        if let tauri::RunEvent::Exit = event {
            tracing::info!("应用开始退出收尾");
            if let Some(state) = handle.try_state::<AppState>() {
                tauri::async_runtime::block_on(async {
                    state.local.stop_all().await;
                    state.recorder.shutdown_all(handle).await;
                    state.view.shutdown_all(handle).await;
                    state.danmaku.shutdown_all();
                });
            }
            logging::shutdown();
            tracing::info!("应用退出收尾完成");
        }
    });
}
