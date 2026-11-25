//! 托盘图标：菜单、点击唤回窗口、按主题换图标。
//!
//! 关闭主窗口不退出应用（录制要继续跑），只把窗口藏起来；真正退出走托盘菜单或 Cmd+Q。

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

pub const TRAY_ID: &str = "hivecast-tray";

const MENU_SHOW: &str = "tray:show";
const MENU_QUIT: &str = "tray:quit";

/// 托盘文案跟随界面语言；`system` 与未知值按中文处理（与前端默认一致）。
fn labels(language: &str) -> (&'static str, &'static str) {
    if language.starts_with("en") {
        ("Show HIVECAST", "Quit HIVECAST")
    } else {
        ("显示 HIVECAST", "退出 HIVECAST")
    }
}

fn language<R: Runtime>(app: &AppHandle<R>) -> String {
    app.try_state::<AppState>()
        .and_then(|state| state.db.get_setting("ui.language").ok().flatten())
        .unwrap_or_else(|| "zh-CN".to_string())
}

/// Windows 没有模板图机制，得自己按任务栏明暗挑一份；macOS 交给系统着色。
fn tray_image<R: Runtime>(app: &AppHandle<R>) -> Image<'static> {
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        Image::from_bytes(include_bytes!("../icons/tray/tray-mac.png")).expect("托盘图标解码失败")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let bytes: &'static [u8] = if is_dark(app) {
            include_bytes!("../icons/tray/tray-win-dark.png")
        } else {
            include_bytes!("../icons/tray/tray-win-light.png")
        };
        Image::from_bytes(bytes).expect("托盘图标解码失败")
    }
}

#[cfg(not(target_os = "macos"))]
fn is_dark<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.theme().ok())
        .map(|theme| theme == tauri::Theme::Dark)
        .unwrap_or(false)
}

fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let (show, quit) = labels(&language(app));
    let show_item = MenuItem::with_id(app, MENU_SHOW, show, true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, MENU_QUIT, quit, true, None::<&str>)?;
    Menu::with_items(app, &[&show_item, &quit_item])
}

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("HIVECAST")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon(tray_image(app))
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => crate::lifecycle::show_main(app),
            MENU_QUIT => crate::lifecycle::on_quit_requested(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::lifecycle::show_main(tray.app_handle());
            }
        });
    // 模板图让 macOS 按菜单栏明暗自动反色，深色菜单栏下才不会看不见。
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);
    builder.build(app)?;
    tracing::info!("系统托盘已创建");
    Ok(())
}

/// 语言或系统主题变化后刷新托盘（菜单文案 + 图标）。
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        tracing::debug!("托盘尚未创建，跳过刷新");
        return;
    };
    match build_menu(app) {
        Ok(menu) => {
            if let Err(err) = tray.set_menu(Some(menu)) {
                tracing::warn!(error = %err, "更新托盘菜单失败");
            }
        }
        Err(err) => tracing::warn!(error = %err, "重建托盘菜单失败"),
    }
    if let Err(err) = tray.set_icon(Some(tray_image(app))) {
        tracing::warn!(error = %err, "更新托盘图标失败");
    }
    #[cfg(target_os = "macos")]
    {
        // set_icon 会覆盖模板标记，重设一次，深色菜单栏下才不会变成黑块。
        let _ = tray.set_icon_as_template(true);
    }
}

/// Windows 任务栏/窗口图标跟随系统明暗；macOS 由系统按外观自动处理，无需切换。
#[cfg(target_os = "windows")]
pub fn sync_window_icon<R: Runtime>(window: &tauri::WebviewWindow<R>, theme: tauri::Theme) {
    let bytes: &'static [u8] = if theme == tauri::Theme::Dark {
        include_bytes!("../icons/windows/taskbar-dark.png")
    } else {
        include_bytes!("../icons/windows/taskbar-light.png")
    };
    if let Ok(icon) = Image::from_bytes(bytes) {
        let _ = window.set_icon(icon);
    }
}
