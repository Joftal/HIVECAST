//! Frameless Liquid Glass WebView2 GUI powered by `tao` + `wry`.

use std::env;
use std::path::PathBuf;
use std::thread;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::WindowBuilder;
use wry::{WebContext, WebViewBuilder};

use crate::install;
use crate::win_os;
use crate::{perform_install, perform_uninstall};

const UI_HTML_TEMPLATE: &str = include_str!("ui.html");
const APP_ICON_PNG: &[u8] = include_bytes!("../../icons/128x128.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiMode {
    Install,
    Uninstall,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BootState {
    mode: String,
    version: String,
    lang: String,
    dark: bool,
    install_dir: String,
    existing_install: bool,
    ffmpeg_found: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
enum IpcCommand {
    Drag,
    Minimize,
    Close,
    Theme {
        dark: bool,
    },
    Inspect {
        dir: String,
    },
    Browse {
        #[serde(rename = "currentDir")]
        current_dir: String,
    },
    Install {
        dir: String,
        #[serde(rename = "desktopShortcut")]
        desktop_shortcut: bool,
    },
    Launch {
        #[serde(rename = "exePath")]
        exe_path: String,
    },
    Uninstall {
        dir: String,
        #[serde(rename = "purgeData", default)]
        purge_data: bool,
    },
}

#[derive(Debug)]
enum AppEvent {
    Ipc(String),
    EmitJs(serde_json::Value),
}

fn build_html(boot: &BootState) -> String {
    let icon_uri = format!("data:image/png;base64,{}", BASE64.encode(APP_ICON_PNG));
    let boot_json = serde_json::to_string(boot)
        .unwrap_or_else(|_| "{}".to_string())
        .replace("</", "<\\/");
    UI_HTML_TEMPLATE
        .replace("__ICON_DATA_URI__", &icon_uri)
        .replace("__BOOT_JSON__", &boot_json)
}

pub fn run(mode: GuiMode, version: String) -> Result<(), String> {
    let zh = win_os::is_system_zh();
    if !win_os::is_webview2_installed() {
        win_os::prompt_webview2_download(zh);
        return Ok(());
    }

    let dark = win_os::is_system_dark();
    let initial_dir = match mode {
        GuiMode::Install => crate::preferred_install_dir(),
        GuiMode::Uninstall => crate::resolve_uninstall_dir(None)?,
    };
    let existing = install::has_existing_install(&initial_dir);

    let boot = BootState {
        mode: match mode {
            GuiMode::Install => "install".to_string(),
            GuiMode::Uninstall => "uninstall".to_string(),
        },
        version,
        lang: if zh {
            "zh".to_string()
        } else {
            "en".to_string()
        },
        dark,
        install_dir: initial_dir.display().to_string(),
        existing_install: existing,
        ffmpeg_found: install::detect_ffmpeg().is_some(),
    };
    let html = build_html(&boot);

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title(match mode {
            GuiMode::Install => "HIVECAST Setup",
            GuiMode::Uninstall => "HIVECAST Uninstall",
        })
        .with_inner_size(LogicalSize::new(432.0, 520.0))
        .with_decorations(false)
        .with_resizable(false)
        .with_undecorated_shadow(true)
        .with_visible(false)
        .build(&event_loop)
        .map_err(|e| format!("无法创建安装窗口：{e}"))?;

    // Center on primary monitor.
    if let Some(monitor) = window.primary_monitor() {
        let mon_pos = monitor.position();
        let mon_size = monitor.size();
        let win_size = window.inner_size();
        let x = mon_pos.x + (mon_size.width as i32 - win_size.width as i32) / 2;
        let y = mon_pos.y + (mon_size.height as i32 - win_size.height as i32) / 2;
        window.set_outer_position(PhysicalPosition::new(x.max(0), y.max(0)));
    }

    win_os::apply_window_material(window.hwnd(), dark);

    // Keep WebView2 user data inside %TEMP% so Downloads stays spotless.
    let wv2_dir = env::temp_dir().join("hivecast-installer-wv2");
    let mut web_context = WebContext::new(Some(wv2_dir));
    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_html(html)
        .with_devtools(false)
        .with_ipc_handler(move |req| {
            let _ = ipc_proxy.send_event(AppEvent::Ipc(req.body().clone()));
        })
        .build(&window)
        .map_err(|e| format!("无法初始化 WebView2 界面：{e}"))?;

    window.set_visible(true);

    let mut busy = false;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if !busy {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(AppEvent::EmitJs(payload)) => {
                if matches!(payload["type"].as_str(), Some("installDone" | "uninstallDone" | "installDirError" | "error")) {
                    busy = false;
                }
                if let Ok(json) = serde_json::to_string(&payload) {
                    let js = format!("window.__installerEvent && window.__installerEvent({json});");
                    let _ = webview.evaluate_script(&js);
                }
            }
            Event::UserEvent(AppEvent::Ipc(raw)) => {
                let Ok(cmd) = serde_json::from_str::<IpcCommand>(&raw) else {
                    return;
                };
                match cmd {
                    IpcCommand::Drag => {
                        let _ = window.drag_window();
                    }
                    IpcCommand::Minimize => {
                        window.set_minimized(true);
                    }
                    IpcCommand::Close => {
                        if !busy {
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                    IpcCommand::Theme { dark } => {
                        win_os::apply_window_material(window.hwnd(), dark);
                    }
                    IpcCommand::Inspect { dir } => {
                        let worker_proxy = proxy.clone();
                        thread::spawn(move || {
                            let existing = install::validate_install_dir(&dir).is_ok_and(|dir| install::has_existing_install(&dir));
                            let _ = worker_proxy.send_event(AppEvent::EmitJs(serde_json::json!({"type":"dirInspected","dir":dir,"existing":existing})));
                        });
                    }
                    IpcCommand::Browse { current_dir } => {
                        if mode != GuiMode::Install {
                            return;
                        }
                        let worker_proxy = proxy.clone();
                        let init = PathBuf::from(current_dir);
                        thread::spawn(move || {
                            if let Some(chosen) = win_os::pick_folder(&init) {
                                let resolved = install::normalize_browsed_dir(&chosen);
                                let existing = install::has_existing_install(&resolved);
                                let _ = worker_proxy.send_event(AppEvent::EmitJs(
                                    serde_json::json!({
                                        "type": "dirSelected",
                                        "dir": resolved.display().to_string(),
                                        "existing": existing,
                                    }),
                                ));
                            }
                        });
                    }
                    IpcCommand::Install {
                        dir,
                        desktop_shortcut,
                    } => {
                        if mode != GuiMode::Install || busy {
                            return;
                        }
                        busy = true;
                        let worker_proxy = proxy.clone();
                        thread::spawn(move || {
                            let progress_proxy = worker_proxy.clone();
                            match perform_install(&dir, desktop_shortcut, |pct, stage| {
                                let _ = progress_proxy.send_event(AppEvent::EmitJs(
                                    serde_json::json!({
                                        "type": "progress",
                                        "percent": pct,
                                        "stage": stage,
                                    }),
                                ));
                            }) {
                                Ok((target, exe_path, upgrading)) => {
                                    let _ = worker_proxy.send_event(AppEvent::EmitJs(
                                        serde_json::json!({
                                            "type": "installDone",
                                            "existing": upgrading,
                                            "installDir": target.display().to_string(),
                                            "exePath": exe_path.display().to_string(),
                                        }),
                                    ));
                                }
                                Err(error) => {
                                    let _ = worker_proxy.send_event(AppEvent::EmitJs(
                                        serde_json::json!(error),
                                    ));
                                }
                            }
                        });
                    }
                    IpcCommand::Launch { exe_path } => {
                        if !exe_path.trim().is_empty() {
                            let _ = win_os::launch_app(&PathBuf::from(exe_path));
                        }
                        *control_flow = ControlFlow::Exit;
                    }
                    IpcCommand::Uninstall { dir, purge_data } => {
                        if mode != GuiMode::Uninstall {
                            return;
                        }
                        let target = match install::validate_install_dir(&dir) {
                            Ok(p) => p,
                            Err(msg) => {
                                let _ = proxy.send_event(AppEvent::EmitJs(
                                    serde_json::json!({ "type": "error", "message": msg }),
                                ));
                                return;
                            }
                        };
                        if busy {
                            return;
                        }
                        busy = true;
                        let worker_proxy = proxy.clone();
                        thread::spawn(move || match perform_uninstall(&target, purge_data) {
                            Ok(()) => {
                                let _ = worker_proxy.send_event(AppEvent::EmitJs(
                                    serde_json::json!({ "type": "progress", "percent": 100, "stage": 2 }),
                                ));
                                let _ = worker_proxy.send_event(AppEvent::EmitJs(
                                    serde_json::json!({ "type": "uninstallDone", "purgeData": purge_data }),
                                ));
                            }
                            Err(msg) => {
                                let _ = worker_proxy.send_event(AppEvent::EmitJs(
                                    serde_json::json!({ "type": "error", "message": msg }),
                                ));
                            }
                        });
                    }
                }
            }
            _ => {}
        }
    });
}
