mod commands;
mod error;
mod ffmpeg;
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

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            // 启动阶段只做轻量初始化：建目录 + 绑定回环端口。
            let state = tauri::async_runtime::block_on(AppState::init(&handle))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::demo_stream
        ])
        .run(tauri::generate_context!())
        .expect("failed to run HIVECAST");
}
