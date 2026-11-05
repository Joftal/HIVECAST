mod broker;
mod commands;
mod db;
mod disk;
mod error;
mod ffmpeg;
mod monitor;
mod platform;
mod recorder;
mod state;
mod stream;

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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::demo_stream,
            commands::rooms_list,
            commands::room_add,
            commands::room_remove,
            commands::room_refresh,
            commands::room_set_auto_record,
            commands::room_set_quality,
            commands::record_start,
            commands::record_stop,
            commands::sessions_list
        ])
        .build(tauri::generate_context!())
        .expect("failed to build HIVECAST");

    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                tauri::async_runtime::block_on(state.recorder.shutdown_all(handle));
            }
        }
    });
}
