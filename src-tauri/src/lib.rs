mod account;
mod broker;
mod commands;
mod credential;
mod danmaku;
mod db;
mod disk;
mod douyin_auth;
mod douyin_ws;
mod douyu_auth;
mod error;
mod ffmpeg;
mod filename;
mod huya_auth;
mod instance;
mod jce;
mod jobs;
mod lifecycle;
mod local;
mod logging;
mod monitor;
mod platform;
mod post;
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
mod task_center;
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

            // 防休眠只在真正录制/观看时持有电源断言。2 秒轮询覆盖自动开录、手动开录、
            // 观看开关、后处理等所有入口，避免只改设置却忘了同步实际状态。
            let power_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let Some(state) = power_handle.try_state::<AppState>() else {
                        continue;
                    };
                    let wanted = state
                        .db
                        .get_setting(crate::settings::KEY_PREVENT_SLEEP)
                        .ok()
                        .flatten()
                        .as_deref()
                        == Some("true");
                    let busy = state.recorder.active_count() > 0
                        || state.recorder.starting_count() > 0
                        || !state.view.snapshot().is_empty();
                    if let Err(err) = crate::power::apply(wanted && busy) {
                        tracing::warn!(error = %err, "设置防休眠失败");
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
                // 启动尺寸跟随当前显示器工作区，而不是固定 1320×860：
                // 小屏不越界，大屏不显得空。仍受 tauri.conf 的 minWidth/minHeight 约束。
                if let Ok(Some(monitor)) = window
                    .current_monitor()
                    .or_else(|_| window.primary_monitor())
                {
                    let work = monitor.work_area();
                    let scale = monitor.scale_factor().max(1.0);
                    let logical_w = work.size.width as f64 / scale;
                    let logical_h = work.size.height as f64 / scale;
                    let width = (logical_w * 0.86)
                        .clamp(1040.0, 1680.0)
                        .min((logical_w - 32.0).max(1040.0));
                    let height = (logical_h * 0.88)
                        .clamp(680.0, 1040.0)
                        .min((logical_h - 32.0).max(680.0));
                    if let Err(err) = window.set_size(tauri::LogicalSize::new(width, height)) {
                        tracing::warn!(%err, "设置启动窗口尺寸失败");
                    }
                    if let Err(err) = window.center() {
                        tracing::warn!(%err, "居中启动窗口失败");
                    }
                    tracing::info!(
                        logical_width = width,
                        logical_height = height,
                        screen_width = logical_w,
                        screen_height = logical_h,
                        "启动窗口已按屏幕工作区缩放"
                    );
                }
                // Windows 没有 macOS 的 Overlay 标题栏：去掉原生边框，由前端顶栏统一画标题、导航与窗口按钮。
                #[cfg(target_os = "windows")]
                {
                    if let Err(err) = window.set_decorations(false) {
                        tracing::warn!(%err, "关闭原生标题栏失败");
                    }
                }
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
                    crate::post::shutdown().await;
                    // 走统一出口：置上 force 标记，避免 ExitRequested 再把退出拦下来。
                    crate::lifecycle::quit_now(&handle);
                });
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::ffmpeg_status,
            commands::demo_stream,
            commands::rooms_list,
            commands::removed_rooms,
            commands::room_restore,
            commands::monitor_status,
            commands::room_add,
            commands::room_remove,
            commands::room_refresh,
            commands::room_set_auto_record,
            commands::room_set_rules,
            commands::room_set_quality,
            commands::room_lines,
            commands::room_stream_options,
            commands::room_set_line,
            commands::room_set_monitor,
            commands::record_start,
            commands::record_stop,
            commands::sessions_list,
            commands::session_summary,
            commands::recording_usage,
            commands::view_slots,
            commands::view_open,
            commands::view_close,
            commands::view_replace,
            commands::view_suspend,
            commands::view_set_quality,
            commands::view_set_line,
            commands::view_retry,
            commands::session_remux,
            commands::post_retry,
            commands::post_cancel,
            commands::finalize_retry,
            commands::session_export_danmaku,
            commands::media_danmaku,
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
            commands::bilibili_refresh_now,
            commands::douyu_qr_start,
            commands::douyu_qr_poll,
            commands::douyu_refresh_now,
            commands::douyin_refresh_now,
            commands::huya_qr_start,
            commands::huya_qr_cancel,
            commands::huya_qr_poll,
            commands::huya_refresh_now,
            commands::bilibili_followings,
            commands::bilibili_follow_statuses,
            commands::douyin_follow_page,
            commands::douyin_pool_add,
            commands::douyu_follow_page,
            commands::huya_follow_page,
            commands::rooms_import,
            commands::media_list,
            commands::media_group_clips,
            commands::media_delete,
            commands::media_delete_group,
            commands::media_reveal,
            commands::media_reveal_source,
            commands::media_reveal_file,
            commands::media_delete_source,
            commands::media_play,
            commands::media_stop,
            commands::active_tasks,
            commands::task_board,
            commands::task_center,
            commands::task_run_events,
            commands::media_files,
            commands::trash_info,
            commands::trash_open,
            commands::close_quit_now,
            commands::close_arm_quit,
            commands::close_cancel_quit,
            commands::logs_status,
            commands::logs_open,
            commands::open_url,
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
            crate::lifecycle::show_main(handle);
        }
        if let tauri::RunEvent::Exit = event {
            tracing::info!("应用开始退出收尾");
            if let Some(state) = handle.try_state::<AppState>() {
                tauri::async_runtime::block_on(async {
                    state.local.stop_all().await;
                    state.recorder.shutdown_all(handle).await;
                    state.view.shutdown_all(handle).await;
                    state.danmaku.shutdown_all();
                    // 兜底：非 close_quit_now 的退出路径（系统注销等）也要取消后处理并清临时文件。
                    crate::post::shutdown().await;
                });
            }
            logging::shutdown();
            tracing::info!("应用退出收尾完成");
        }
    });
}
