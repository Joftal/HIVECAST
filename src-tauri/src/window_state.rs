//! 主窗口几何状态记忆（大小、位置、最大化）与高分屏/多屏安全恢复。
//!
//! 持久化在 `app_setting` 表的 `ui.window_state` 键下，自动随升级与便携模式继承：
//! - 尺寸按**逻辑像素**保存，兼容 macOS Retina 与 Windows 125%–200% 混合 DPI；
//! - 位置按虚拟桌面**物理像素**保存（附带记录时的 `scale_factor`），跨混合 DPI 多屏时
//!   直接与各显示器 `work_area()` 对齐，外接屏断开时自动回落主屏居中；
//! - 最小化、全屏与最大化状态不会覆盖用户上一次的正常窗口尺寸与位置；
//! - 拖拽过程通过 350ms 防抖合批写库，隐藏到托盘或退出前强制同步落盘。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, Runtime, WebviewWindow};

use crate::db::Db;
use crate::state::AppState;

/// 存储在 `app_setting` 中的键名。
pub const KEY_WINDOW_STATE: &str = "ui.window_state";

/// 窗口允许的最小逻辑尺寸（与 `WebviewWindowBuilder::min_inner_size` 保持一致）。
pub const MIN_LOGICAL_WIDTH: f64 = 1180.0;
pub const MIN_LOGICAL_HEIGHT: f64 = 680.0;

/// 首次启动（无历史记录时）默认窗口的逻辑尺寸上限，避免超宽屏初次打开铺满整屏。
const DEFAULT_MAX_LOGICAL_WIDTH: f64 = 1680.0;
const DEFAULT_MAX_LOGICAL_HEIGHT: f64 = 1040.0;
/// 首次启动默认尺寸相对工作区的留边（逻辑像素）。
const DEFAULT_WORK_AREA_MARGIN: f64 = 32.0;

/// 过滤 Windows 最小化等异常窗口状态产生的极端尺寸与坐标。
const MIN_VALID_LOGICAL_DIM: f64 = 200.0;
const MIN_VALID_COORD: i32 = -30_000;
const MAX_VALID_COORD: i32 = 30_000;

/// 拖拽缩放/移动的防抖写盘间隔。
const SAVE_DEBOUNCE_MS: u64 = 350;

/// 持久化的窗口状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// 正常状态（非最大化、非最小化、非全屏）下的逻辑内部宽度。
    pub width: f64,
    /// 正常状态下的逻辑内部高度。
    pub height: f64,
    /// 正常状态下的外部左上角 X（虚拟桌面物理像素）。
    #[serde(default)]
    pub x: Option<i32>,
    /// 正常状态下的外部左上角 Y（虚拟桌面物理像素）。
    #[serde(default)]
    pub y: Option<i32>,
    /// 记录 `(x, y)` 时所在显示器的缩放因子，用于跨屏恢复时估算物理矩形重叠度。
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f64,
    /// 上次是否处于最大化状态。
    #[serde(default)]
    pub maximized: bool,
}

fn default_scale_factor() -> f64 {
    1.0
}

impl WindowState {
    fn is_valid(&self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.width >= MIN_VALID_LOGICAL_DIM
            && self.height >= MIN_VALID_LOGICAL_DIM
            && self.scale_factor.is_finite()
            && self.scale_factor > 0.0
    }

    fn valid_position(&self) -> Option<(i32, i32)> {
        let (x, y) = (self.x?, self.y?);
        if is_valid_coord(x) && is_valid_coord(y) {
            Some((x, y))
        } else {
            None
        }
    }
}

fn is_valid_coord(v: i32) -> bool {
    (MIN_VALID_COORD..=MAX_VALID_COORD).contains(&v)
}

/// 单个显示器的可用工作区（物理像素 + 缩放因子），方便纯函数计算与单测。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorInfo {
    pub work_x: i32,
    pub work_y: i32,
    pub work_w: u32,
    pub work_h: u32,
    pub scale_factor: f64,
}

impl MonitorInfo {
    fn from_tauri(m: &tauri::Monitor) -> Self {
        let work = m.work_area();
        let scale = m.scale_factor();
        Self {
            work_x: work.position.x,
            work_y: work.position.y,
            work_w: work.size.width,
            work_h: work.size.height,
            scale_factor: if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                1.0
            },
        }
    }

    fn logical_work_size(&self) -> (f64, f64) {
        (
            self.work_w as f64 / self.scale_factor,
            self.work_h as f64 / self.scale_factor,
        )
    }
}

/// 启动阶段计算出的目标窗口布局。
#[derive(Debug, Clone, PartialEq)]
pub struct StartupGeometry {
    pub width: f64,
    pub height: f64,
    /// 若为 `Some((x, y))` 则定位到指定物理坐标；为 `None` 时在目标显示器居中。
    pub position: Option<(i32, i32)>,
    pub scale_factor: f64,
    pub maximized: bool,
}

/// 内存中的窗口状态追踪器（挂在 Tauri AppState 之外的独立托管对象上）。
pub struct WindowStateTracker {
    ready: AtomicBool,
    generation: AtomicU64,
    inner: Mutex<TrackerInner>,
}

#[derive(Default)]
struct TrackerInner {
    current: Option<WindowState>,
    last_persisted: Option<WindowState>,
}

impl Default for WindowStateTracker {
    fn default() -> Self {
        Self {
            ready: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            inner: Mutex::new(TrackerInner::default()),
        }
    }
}

impl WindowStateTracker {
    fn seed(&self, state: WindowState, persisted: Option<WindowState>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.current = Some(state);
            guard.last_persisted = persisted;
        }
        self.ready.store(true, Ordering::SeqCst);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// 从 `app_setting` 读取历史窗口状态；格式损坏或数值非法时安全回落为 `None`。
pub fn load(db: &Db) -> Option<WindowState> {
    let raw = match db.get_setting(KEY_WINDOW_STATE) {
        Ok(Some(v)) => v,
        Ok(None) => return None,
        Err(err) => {
            tracing::warn!(target: "hivecast_lib::window_state", error = %err, "读取窗口状态失败");
            return None;
        }
    };
    match serde_json::from_str::<WindowState>(&raw) {
        Ok(state) if state.is_valid() => Some(state),
        Ok(state) => {
            tracing::warn!(
                target: "hivecast_lib::window_state",
                ?state,
                "已保存的窗口状态数值非法，回落到默认尺寸"
            );
            None
        }
        Err(err) => {
            tracing::warn!(
                target: "hivecast_lib::window_state",
                error = %err,
                "解析窗口状态 JSON 失败，回落到默认尺寸"
            );
            None
        }
    }
}

/// 将窗口状态写入 `app_setting`。
pub fn save(db: &Db, state: &WindowState) -> bool {
    if !state.is_valid() {
        return false;
    }
    let result = serde_json::to_string(state)
        .map_err(|err| err.to_string())
        .and_then(|json| {
            db.set_setting(KEY_WINDOW_STATE, &json)
                .map_err(|err| err.to_string())
        });
    if let Err(err) = result {
        tracing::warn!(target: "hivecast_lib::window_state", error = %err, "写入窗口状态失败");
        return false;
    }
    true
}

#[derive(Clone, Copy)]
struct Rect {
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
}

/// 计算两个矩形的交集面积（单位：平方物理像素）。
fn intersection_area(a: Rect, b: Rect) -> i64 {
    let w = (a.x2.min(b.x2) - a.x1.max(b.x1)).max(0);
    let h = (a.y2.min(b.y2) - a.y1.max(b.y1)).max(0);
    w.saturating_mul(h)
}

/// 按当前显示器列表与可选的历史窗口状态，计算启动时最安全合理的窗口几何。
///
/// 规则：
/// 1. 有历史状态且其矩形与某个已连接显示器有交集：选中交集面积最大的显示器，
///    将逻辑宽高钳制到该显示器工作区内（不施加 `1680×1040` 默认上限，照顾 2K/4K 大屏），
///    再把物理坐标钳制进工作区，避免窗口越界或标题栏不可拖拽；
/// 2. 有历史状态但原显示器已断开（与所有现有显示器无交集）：回落到主屏，
///    按主屏工作区钳制逻辑宽高并居中；
/// 3. 无历史状态（首次启动）：按主屏工作区 `86% × 88%` 计算舒适尺寸并居中。
pub fn resolve_startup_geometry(
    saved: Option<&WindowState>,
    monitors: &[MonitorInfo],
    primary_idx: usize,
) -> Option<StartupGeometry> {
    if monitors.is_empty() {
        return None;
    }
    let fallback_idx = primary_idx.min(monitors.len() - 1);

    let Some(saved) = saved.filter(|s| s.is_valid()) else {
        let m = monitors[fallback_idx];
        let (logical_w, logical_h) = m.logical_work_size();
        let max_w = (logical_w - DEFAULT_WORK_AREA_MARGIN).max(MIN_LOGICAL_WIDTH.min(logical_w));
        let max_h = (logical_h - DEFAULT_WORK_AREA_MARGIN).max(MIN_LOGICAL_HEIGHT.min(logical_h));
        let min_w = MIN_LOGICAL_WIDTH.min(max_w);
        let min_h = MIN_LOGICAL_HEIGHT.min(max_h);
        let width = (logical_w * 0.86).clamp(min_w, DEFAULT_MAX_LOGICAL_WIDTH.min(max_w));
        let height = (logical_h * 0.88).clamp(min_h, DEFAULT_MAX_LOGICAL_HEIGHT.min(max_h));
        return Some(StartupGeometry {
            width: width.round(),
            height: height.round(),
            position: None,
            scale_factor: m.scale_factor,
            maximized: false,
        });
    };

    // 尝试匹配历史位置所在的显示器（按矩形交集面积最大者优先）。
    let matched = saved.valid_position().and_then(|(sx, sy)| {
        let est_w = (saved.width * saved.scale_factor).round().max(1.0) as i64;
        let est_h = (saved.height * saved.scale_factor).round().max(1.0) as i64;
        let saved_rect = Rect {
            x1: i64::from(sx),
            y1: i64::from(sy),
            x2: i64::from(sx).saturating_add(est_w),
            y2: i64::from(sy).saturating_add(est_h),
        };

        monitors
            .iter()
            .enumerate()
            .filter_map(|(idx, m)| {
                let mon_rect = Rect {
                    x1: i64::from(m.work_x),
                    y1: i64::from(m.work_y),
                    x2: i64::from(m.work_x).saturating_add(i64::from(m.work_w)),
                    y2: i64::from(m.work_y).saturating_add(i64::from(m.work_h)),
                };
                let area = intersection_area(saved_rect, mon_rect);
                (area > 0).then_some((idx, area))
            })
            .max_by_key(|&(_, area)| area)
            .map(|(idx, _)| (idx, sx, sy))
    });

    let (target_monitor, raw_pos) = match matched {
        Some((idx, sx, sy)) => (monitors[idx], Some((sx, sy))),
        None => (monitors[fallback_idx], None),
    };

    let (logical_work_w, logical_work_h) = target_monitor.logical_work_size();
    let min_w = MIN_LOGICAL_WIDTH.min(logical_work_w);
    let min_h = MIN_LOGICAL_HEIGHT.min(logical_work_h);
    let width = saved.width.clamp(min_w, logical_work_w).round();
    let height = saved.height.clamp(min_h, logical_work_h).round();

    let position = raw_pos.map(|(sx, sy)| {
        let win_phys_w = (width * target_monitor.scale_factor).round() as i64;
        let win_phys_h = (height * target_monitor.scale_factor).round() as i64;
        let min_x = i64::from(target_monitor.work_x);
        let min_y = i64::from(target_monitor.work_y);
        let max_x = (min_x + i64::from(target_monitor.work_w) - win_phys_w).max(min_x);
        let max_y = (min_y + i64::from(target_monitor.work_h) - win_phys_h).max(min_y);
        let clamped_x = i64::from(sx).clamp(min_x, max_x) as i32;
        let clamped_y = i64::from(sy).clamp(min_y, max_y) as i32;
        (clamped_x, clamped_y)
    });

    Some(StartupGeometry {
        width,
        height,
        position,
        scale_factor: target_monitor.scale_factor,
        maximized: saved.maximized,
    })
}

/// 收集当前全部显示器及其主屏索引。
fn collect_monitors<R: Runtime>(window: &WebviewWindow<R>) -> (Vec<MonitorInfo>, usize) {
    let available = window.available_monitors().unwrap_or_default();
    let primary = window
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten());

    if available.is_empty() {
        return match primary {
            Some(m) => (vec![MonitorInfo::from_tauri(&m)], 0),
            None => (Vec::new(), 0),
        };
    }

    let primary_info = primary.as_ref().map(MonitorInfo::from_tauri);
    let infos: Vec<MonitorInfo> = available.iter().map(MonitorInfo::from_tauri).collect();
    let primary_idx = primary_info
        .and_then(|p| infos.iter().position(|m| m == &p))
        .unwrap_or(0);
    (infos, primary_idx)
}

/// 在窗口仍隐藏时恢复几何，随后显示并根据历史状态决定是否最大化，最后激活状态追踪。
pub fn restore_and_show<R: Runtime>(
    window: &WebviewWindow<R>,
    db: &Db,
    tracker: &Arc<WindowStateTracker>,
) {
    let saved = load(db);
    let (monitors, primary_idx) = collect_monitors(window);
    let geometry = resolve_startup_geometry(saved.as_ref(), &monitors, primary_idx);

    let mut applied_pos: Option<(i32, i32)> = None;
    let mut applied_size = (1320.0, 860.0);
    let mut applied_scale = window.scale_factor().unwrap_or(1.0).max(1.0);
    let mut want_maximized = false;

    if let Some(geom) = geometry {
        applied_size = (geom.width, geom.height);
        applied_scale = geom.scale_factor;
        want_maximized = geom.maximized;

        // 先定位到目标显示器，再设置逻辑尺寸，最后再校正一次位置：
        // - Windows 混合 DPI 多屏下，先挪到目标屏可让 set_size(LogicalSize) 按目标屏 DPI 换算；
        // - 再次 set_position 可消除 macOS Cocoa 底左原点或 Windows WM_DPICHANGED 造成的位移偏差。
        if let Some((x, y)) = geom.position {
            if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
                tracing::warn!(target: "hivecast_lib::window_state", error = %err, x, y, "恢复窗口位置失败，回落居中");
            } else {
                applied_pos = Some((x, y));
            }
        }
        if let Err(err) = window.set_size(LogicalSize::new(geom.width, geom.height)) {
            tracing::warn!(target: "hivecast_lib::window_state", error = %err, "设置启动窗口尺寸失败");
        }
        if let Some((x, y)) = applied_pos {
            let _ = window.set_position(PhysicalPosition::new(x, y));
        } else if let Err(err) = window.center() {
            tracing::warn!(target: "hivecast_lib::window_state", error = %err, "居中启动窗口失败");
        } else if let Ok(pos) = window.outer_position() {
            if is_valid_coord(pos.x) && is_valid_coord(pos.y) {
                applied_pos = Some((pos.x, pos.y));
            }
        }

        tracing::info!(
            target: "hivecast_lib::window_state",
            restored = saved.is_some(),
            logical_width = geom.width,
            logical_height = geom.height,
            position = ?applied_pos,
            scale_factor = geom.scale_factor,
            maximized = geom.maximized,
            "启动窗口几何已应用"
        );
    }

    if let Err(err) = window.show() {
        tracing::warn!(target: "hivecast_lib::window_state", error = %err, "显示主窗口失败");
    }
    if want_maximized {
        if let Err(err) = window.maximize() {
            tracing::warn!(target: "hivecast_lib::window_state", error = %err, "恢复窗口最大化状态失败");
            want_maximized = false;
        }
    }
    let _ = window.set_focus();

    let baseline = WindowState {
        width: applied_size.0,
        height: applied_size.1,
        x: applied_pos.map(|(x, _)| x),
        y: applied_pos.map(|(_, y)| y),
        scale_factor: applied_scale,
        maximized: want_maximized,
    };
    // 首次启动时把默认几何视为已同步基线，只有用户真正调整过大小/位置/最大化后才落盘，
    // 避免未手工调整过的用户在换大屏后失去自动适配能力。
    let persisted_baseline = saved.or_else(|| Some(baseline.clone()));
    tracker.seed(baseline, persisted_baseline);
}

// During WM_WINDOWPOSCHANGED the toolkit may still expose the previous maximize
// flag with the new client bounds. Check the native restore rectangle as well.
#[cfg(windows)]
fn normal_placement<R: Runtime>(window: &WebviewWindow<R>) -> Option<bool> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, GetWindowRect, IsZoomed, SW_SHOWMAXIMIZED, WINDOWPLACEMENT,
    };
    let hwnd = window.hwnd().ok()?.0;
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let mut rect = RECT::default();
    unsafe {
        if GetWindowPlacement(hwnd, &mut placement) == 0 || GetWindowRect(hwnd, &mut rect) == 0 {
            return None;
        }
        if IsZoomed(hwnd) != 0 || placement.showCmd == SW_SHOWMAXIMIZED as u32 {
            return Some(false);
        }
    }
    let normal = placement.rcNormalPosition;
    if rect.right - rect.left != normal.right - normal.left
        || rect.bottom - rect.top != normal.bottom - normal.top
    {
        return None;
    }
    Some(true)
}

/// 从当前可见主窗口采样几何并更新内存快照；不会在最小化/全屏/隐藏时覆盖正常宽高。
fn sample_window_into_tracker<R: Runtime>(
    window: &WebviewWindow<R>,
    tracker: &WindowStateTracker,
) -> Option<WindowState> {
    if !tracker.is_ready() {
        return None;
    }
    if !window.is_visible().unwrap_or(false) {
        return None;
    }
    if window.is_minimized().unwrap_or(false) {
        return None;
    }
    if window.is_fullscreen().unwrap_or(false) {
        return None;
    }

    #[cfg(windows)]
    let maximized = !normal_placement(window)?;
    #[cfg(not(windows))]
    let maximized = window.is_maximized().ok()?;
    if maximized {
        let mut guard = tracker.inner.lock().ok()?;
        if let Some(state) = guard.current.as_mut() {
            state.maximized = true;
            return Some(state.clone());
        }
        return None;
    }

    let scale = window.scale_factor().unwrap_or(1.0);
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let size = window.inner_size().ok()?;
    let logical: LogicalSize<f64> = size.to_logical(scale);
    if !logical.width.is_finite()
        || !logical.height.is_finite()
        || logical.width < MIN_VALID_LOGICAL_DIM
        || logical.height < MIN_VALID_LOGICAL_DIM
    {
        return None;
    }

    let position = window.outer_position();
    let mut guard = tracker.inner.lock().ok()?;
    let prev = guard.current.as_ref();
    let (x, y) = match position {
        Ok(pos) if is_valid_coord(pos.x) && is_valid_coord(pos.y) => (Some(pos.x), Some(pos.y)),
        _ => (prev.and_then(|s| s.x), prev.and_then(|s| s.y)),
    };

    let next = WindowState {
        width: logical.width.round(),
        height: logical.height.round(),
        x,
        y,
        scale_factor: scale,
        maximized: false,
    };
    guard.current = Some(next.clone());
    Some(next)
}

fn persist_if_changed(db: &Db, tracker: &WindowStateTracker) {
    // Serialize commits with close/exit. This section never calls a window API.
    let Ok(mut guard) = tracker.inner.lock() else {
        return;
    };
    let Some(current) = guard.current.as_ref() else {
        return;
    };
    if guard.last_persisted.as_ref() != Some(current) && save(db, current) {
        guard.last_persisted = Some(current.clone());
    }
}

/// 在事件回调中采样，后台任务只合并数据库写入。
pub fn schedule_save<R: Runtime>(app: &AppHandle<R>) {
    let Some(tracker) = app.try_state::<Arc<WindowStateTracker>>() else {
        return;
    };
    if !tracker.is_ready() {
        return;
    }
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if sample_window_into_tracker(&window, &tracker).is_none() {
        return;
    }
    let gen = tracker.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let tracker = Arc::clone(&*tracker);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(SAVE_DEBOUNCE_MS)).await;
        if tracker.generation.load(Ordering::SeqCst) != gen {
            return;
        }
        if let Some(state) = handle.try_state::<AppState>() {
            persist_if_changed(&state.db, &tracker);
        }
    });
}

/// 在隐藏窗口或退出程序前同步采样并刷盘，确保最后一次调整不丢失。
pub fn flush_now<R: Runtime>(app: &AppHandle<R>) {
    let Some(tracker) = app.try_state::<Arc<WindowStateTracker>>() else {
        return;
    };
    if !tracker.is_ready() {
        return;
    }
    // 取消尚未触发的防抖任务，避免重复写库。
    tracker.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        let _ = sample_window_into_tracker(&window, &tracker);
    }
    if let Some(state) = app.try_state::<AppState>() {
        persist_if_changed(&state.db, &tracker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: u32, h: u32, scale: f64) -> MonitorInfo {
        MonitorInfo {
            work_x: x,
            work_y: y,
            work_w: w,
            work_h: h,
            scale_factor: scale,
        }
    }

    #[test]
    fn first_launch_scales_to_work_area_and_centers() {
        // 1080p 100% 缩放
        let monitors = [mon(0, 0, 1920, 1040, 1.0)];
        let geom = resolve_startup_geometry(None, &monitors, 0).unwrap();
        assert_eq!(geom.position, None);
        assert!(!geom.maximized);
        assert_eq!(geom.width, 1651.0);
        assert_eq!(geom.height, 915.0);
    }

    #[test]
    fn first_launch_retina_uses_logical_dimensions() {
        // 4K 200% 缩放（逻辑 1920×1040）应与 1080p 得到相同逻辑尺寸，不会翻倍
        let monitors = [mon(0, 0, 3840, 2080, 2.0)];
        let geom = resolve_startup_geometry(None, &monitors, 0).unwrap();
        assert_eq!(geom.width, 1651.0);
        assert_eq!(geom.height, 915.0);
        assert_eq!(geom.scale_factor, 2.0);
    }

    #[test]
    fn restores_large_window_on_4k_without_default_cap() {
        // 4K @ 150% -> 逻辑工作区 2560×1400；用户拉到 2200×1260 不应被 1680×1040 卡死
        let monitors = [mon(0, 0, 3840, 2100, 1.5)];
        let saved = WindowState {
            width: 2200.0,
            height: 1260.0,
            x: Some(200),
            y: Some(100),
            scale_factor: 1.5,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&saved), &monitors, 0).unwrap();
        assert_eq!(geom.width, 2200.0);
        assert_eq!(geom.height, 1260.0);
        assert_eq!(geom.position, Some((200, 100)));
        assert!(!geom.maximized);
    }

    #[test]
    fn clamps_large_4k_window_when_reopened_on_laptop_screen() {
        // 上次在 4K 拉到 2200×1260，这次在 13 寸 MacBook Retina（2880×1640 @ 2.0 -> 逻辑 1440×820）打开
        let monitors = [mon(0, 50, 2880, 1640, 2.0)];
        let saved = WindowState {
            width: 2200.0,
            height: 1260.0,
            x: Some(400),
            y: Some(120),
            scale_factor: 1.5,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&saved), &monitors, 0).unwrap();
        assert_eq!(geom.width, 1440.0);
        assert_eq!(geom.height, 820.0);
        // 撑满工作区时坐标也应钳回工作区左上角
        assert_eq!(geom.position, Some((0, 50)));
    }

    #[test]
    fn restores_on_secondary_mixed_dpi_monitor() {
        // 主屏 4K @ 200% (0..3840)，右侧副屏 1080p @ 100% (3840..5760)
        let monitors = [mon(0, 0, 3840, 2100, 2.0), mon(3840, 0, 1920, 1040, 1.0)];
        let saved = WindowState {
            width: 1400.0,
            height: 900.0,
            x: Some(4000),
            y: Some(80),
            scale_factor: 1.0,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&saved), &monitors, 0).unwrap();
        assert_eq!(geom.scale_factor, 1.0);
        assert_eq!(geom.width, 1400.0);
        assert_eq!(geom.height, 900.0);
        assert_eq!(geom.position, Some((4000, 80)));
    }

    #[test]
    fn falls_back_to_primary_center_when_secondary_monitor_disconnected() {
        // 副屏已拔掉，仅剩主屏 4K @ 200%
        let monitors = [mon(0, 0, 3840, 2100, 2.0)];
        let saved = WindowState {
            width: 1400.0,
            height: 900.0,
            x: Some(4000),
            y: Some(80),
            scale_factor: 1.0,
            maximized: true,
        };
        let geom = resolve_startup_geometry(Some(&saved), &monitors, 0).unwrap();
        assert_eq!(geom.scale_factor, 2.0);
        assert_eq!(geom.width, 1400.0);
        assert_eq!(geom.height, 900.0);
        assert_eq!(geom.position, None);
        assert!(geom.maximized);
    }

    #[test]
    fn supports_negative_monitor_coordinates() {
        // 副屏位于主屏左侧 (-1920..0)
        let monitors = [mon(0, 0, 1920, 1040, 1.0), mon(-1920, 0, 1920, 1040, 1.0)];
        let saved = WindowState {
            width: 1280.0,
            height: 800.0,
            x: Some(-1700),
            y: Some(100),
            scale_factor: 1.0,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&saved), &monitors, 0).unwrap();
        assert_eq!(geom.position, Some((-1700, 100)));
        assert_eq!(geom.width, 1280.0);
        assert_eq!(geom.height, 800.0);
    }

    #[test]
    fn clamps_partially_offscreen_position_back_into_work_area() {
        let monitors = [mon(0, 0, 1920, 1040, 1.0)];
        let saved = WindowState {
            width: 1400.0,
            height: 800.0,
            x: Some(1200),
            y: Some(600),
            scale_factor: 1.0,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&saved), &monitors, 0).unwrap();
        // max_x = 1920 - 1400 = 520, max_y = 1040 - 800 = 240
        assert_eq!(geom.position, Some((520, 240)));
    }

    #[test]
    fn rejects_windows_minimized_sentinel_values() {
        let monitors = [mon(0, 0, 1920, 1040, 1.0)];
        let bad_size = WindowState {
            width: 0.0,
            height: 0.0,
            x: Some(-32000),
            y: Some(-32000),
            scale_factor: 1.0,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&bad_size), &monitors, 0).unwrap();
        // 非法尺寸应回落到首次启动逻辑
        assert_eq!(geom.width, 1651.0);
        assert_eq!(geom.position, None);

        let bad_coord = WindowState {
            width: 1400.0,
            height: 820.0,
            x: Some(-32000),
            y: Some(-32000),
            scale_factor: 1.0,
            maximized: false,
        };
        let geom = resolve_startup_geometry(Some(&bad_coord), &monitors, 0).unwrap();
        // 尺寸有效但坐标非法 -> 保留尺寸并居中
        assert_eq!(geom.width, 1400.0);
        assert_eq!(geom.height, 820.0);
        assert_eq!(geom.position, None);
    }

    #[test]
    fn db_roundtrip_and_corrupted_json_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        assert_eq!(load(&db), None);

        let state = WindowState {
            width: 1520.0,
            height: 940.0,
            x: Some(120),
            y: Some(64),
            scale_factor: 2.0,
            maximized: true,
        };
        save(&db, &state);
        assert_eq!(load(&db), Some(state));

        db.set_setting(KEY_WINDOW_STATE, "{not-valid-json").unwrap();
        assert_eq!(load(&db), None);
    }
    #[test]
    fn failed_commit_stays_dirty_until_flush_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        let tracker = WindowStateTracker::default();
        let state = WindowState {
            width: 1300.0,
            height: 800.0,
            x: Some(20),
            y: Some(30),
            scale_factor: 1.0,
            maximized: true,
        };
        tracker.seed(state.clone(), None);
        db.exec_batch_for_test(
            "CREATE TRIGGER reject_window BEFORE INSERT ON app_setting
            WHEN NEW.key='ui.window_state' BEGIN SELECT RAISE(ABORT,'test'); END;",
        )
        .unwrap();
        persist_if_changed(&db, &tracker);
        assert!(tracker.inner.lock().unwrap().last_persisted.is_none());
        db.exec_batch_for_test("DROP TRIGGER reject_window")
            .unwrap();
        persist_if_changed(&db, &tracker);
        assert_eq!(load(&db), Some(state));
    }
}
