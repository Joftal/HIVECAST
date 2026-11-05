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
mod segment;
mod settings;
mod state;
mod stream;
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
            commands::record_start,
            commands::record_stop,
            commands::sessions_list,
            commands::view_slots,
            commands::view_open,
            commands::view_close,
            commands::view_set_quality,
            commands::view_retry,
            commands::session_remux,
            commands::session_screenshot,
            commands::session_export_danmaku,
            commands::room_merge_sessions,
            commands::settings_get,
            commands::settings_update,
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
