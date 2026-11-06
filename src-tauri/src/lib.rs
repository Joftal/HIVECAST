mod account;
mod broker;
mod commands;
mod credential;
mod danmaku;
mod douyin_ws;
mod db;
mod disk;
mod error;
mod ffmpeg;
mod jce;
mod local;
mod monitor;
mod platform;
mod postprocess;
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
            // 启动阶段只做轻量初始化：建目录 + 打开数据库 + 绑定回环端口 + 起监控循环。
            let state = tauri::async_runtime::block_on(AppState::init(&handle))?;
            app.manage(state);

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
                    tauri::WindowEvent::CloseRequested { api, .. } if tray_ready => {
                        api.prevent_close();
                        let _ = target.hide();
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
                    handle.exit(0);
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
            commands::layout_get,
            commands::layout_set,
            commands::credentials_list,
            commands::credential_save,
            commands::credential_delete,
            commands::bilibili_qr_start,
            commands::bilibili_qr_poll,
            commands::media_list,
            commands::media_delete,
            commands::media_reveal,
            commands::media_play,
            commands::media_stop
        ])
        .build(tauri::generate_context!())
        .expect("failed to build HIVECAST");

    app.run(|handle, event| {
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
