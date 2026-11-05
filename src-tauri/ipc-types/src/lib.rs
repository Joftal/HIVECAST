//! 前后端共享的 IPC 数据契约。
//!
//! 这里的类型由 ts-rs 导出成 TypeScript（`pnpm gen:bindings`），
//! 因此任何字段变更都会同步反映到 `src/lib/bindings/`，避免手写两份定义漂移。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 应用与运行环境信息，用于前端环境自检面板。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    /// PATH 中探测到的 ffmpeg；未安装时为 None。
    pub ffmpeg_path: Option<String>,
    pub data_dir: String,
    pub cache_dir: String,
    /// 本地回环流服务的端口（每个进程随机）。
    pub loopback_port: u16,
}

/// 一次可播放的流会话描述。
///
/// 注意：这里是「租约」的快照，消费者拿到后不应期望它随别人的操作变化。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct StreamDescriptor {
    pub url: String,
    pub session_id: String,
    pub kind: String,
    pub quality: String,
    pub format: String,
}

/// 结构化 IPC 错误：前端只依赖 code 做分支，message 仅用于展示。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

/// 房间：用户在监控页看到的一行。
///
/// 平台侧的展示信息会被轮询刷新；auto_record / record_qn / view_qn 是本地设置，
/// 轮询不会覆盖它们。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RoomRecord {
    pub id: String,
    pub platform: String,
    pub room_key: String,
    pub canonical_url: String,
    pub title: String,
    pub host_name: String,
    pub avatar_url: Option<String>,
    pub cover_url: Option<String>,
    pub area_name: Option<String>,
    pub is_live: bool,
    #[ts(type = "number")]
    pub started_at: Option<i64>,
    #[ts(type = "number")]
    pub online: Option<u64>,
    /// 开播后是否自动录制
    pub auto_record: bool,
    /// 录制清晰度（qn 数值，10000 = 原画）
    pub record_qn: u32,
    /// 观看清晰度（0 = 跟随自动降级）
    pub view_qn: u32,
    #[ts(type = "number")]
    pub last_checked_at: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// 正在进行的录制快照。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RecordingInfo {
    pub room_id: String,
    pub session_id: String,
    pub file_path: String,
    #[ts(type = "number")]
    pub started_at: i64,
    pub quality: String,
    pub line: String,
}

/// 监控页列表项 = 房间 + 实时录制状态 + 最近的错误。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RoomView {
    pub room: RoomRecord,
    pub recording: Option<RecordingInfo>,
    pub error: Option<String>,
}

/// 一次录制记录（历史页数据源）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RecordSession {
    pub id: String,
    pub room_id: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number")]
    pub ended_at: Option<i64>,
    /// recording | finished | failed | aborted
    pub status: String,
    /// stream_ended | stopped | error | app_exit
    pub reason: Option<String>,
    pub file_path: Option<String>,
    #[ts(type = "number")]
    pub file_size: i64,
    #[ts(type = "number")]
    pub duration_ms: i64,
    pub quality: Option<String>,
    pub line: Option<String>,
    pub error: Option<String>,
    /// 与视频同名的弹幕存档（JSONL），未开启弹幕时为空
    pub danmaku_path: Option<String>,
    /// 本场落盘的弹幕条数
    #[ts(type = "number")]
    pub danmaku_count: i64,
}

/// 本地后处理的产物：转封装 / 截图 / 弹幕 ASS / 合并。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct PostResult {
    /// mp4 | screenshot | ass | merge
    pub kind: String,
    pub path: String,
    #[ts(type = "number")]
    pub size: i64,
    /// 额外说明（例如合并了几段），前端直接展示
    pub message: Option<String>,
}

/// 录制状态播报：前端据此弹系统通知（文案在前端 i18n，后端不硬编码）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RecordNotice {
    /// started | ended | failed | disk_full
    pub kind: String,
    pub room_id: String,
    pub host_name: String,
    pub detail: Option<String>,
}

/// 观看槽位快照：每个槽位一条独立的转封装管道。
///
/// url 为空表示还没起播（starting / reconnecting / error），前端据此显示占位。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct ViewSlotInfo {
    /// 0..3
    pub slot: u8,
    pub room_id: String,
    /// starting | playing | reconnecting | error
    pub status: String,
    pub url: Option<String>,
    /// 请求的画质（qn 数值，0 表示跟随平台自动）
    pub requested_qn: u32,
    /// 平台实际给的画质
    pub actual_qn: u32,
    pub quality: Option<String>,
    pub line: Option<String>,
    pub error: Option<String>,
}

/// 一条弹幕（已归一化，平台差异在客户端层消化）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct DanmakuMessage {
    /// 相对连接建立（或录制开始）的毫秒偏移
    #[ts(type = "number")]
    pub offset_ms: i64,
    /// chat | gift | super_chat | guard
    pub kind: String,
    pub user: Option<String>,
    pub content: String,
    /// 0xRRGGBB
    pub color: u32,
}

/// 批量推送：每 200ms 合并一次，避免高频事件把 webview 打满。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct DanmakuBatch {
    pub room_id: String,
    pub messages: Vec<DanmakuMessage>,
}
