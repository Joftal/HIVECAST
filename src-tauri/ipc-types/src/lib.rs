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
    /// 录制线路偏好（线路组名，None = 自动选最优）
    pub record_line: Option<String>,
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
    /// 断线重连的锚点（epoch 毫秒）；None 表示当前不在等待重连
    #[ts(type = "number | null")]
    pub retry_at: Option<i64>,
}

/// 一条可用线路（来自取流结果的分组），用于手动指定录制线路。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct LineInfo {
    pub name: String,
    pub priority: u32,
    /// 该线路首选的画质名（用于在选择器里给用户一点判断依据）
    pub quality: String,
    pub candidates: usize,
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

/// 全局设置（v1 只有一个作用域，即「全局」）。
///
/// 为什么不用 docs/19 里的四级作用域表：v1 只有单账号单库，
/// 四级覆盖没有真实使用场景；等会话级覆盖真正出现时再拆表。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct Settings {
    /// 新房间的默认录制画质（qn，10000 = 原画）
    pub default_record_qn: u32,
    /// 新房间的默认观看画质（0 = 自动）
    pub default_view_qn: u32,
    /// 媒体库根目录
    pub media_root: String,
    /// 同时录制的上限
    pub max_concurrent_records: u32,
    /// 录制开始/结束时是否发系统通知
    pub notify_enabled: bool,
    /// zh-CN | en-US | system
    pub language: String,
    /// 录像保留天数（0 = 不按天删除）
    pub retention_days: u32,
    /// 媒体库总容量上限 GB（0 = 不限）
    pub max_total_gb: u32,
}

/// 媒体库里的一条录像（会话 + 房间展示信息的扁平快照）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct MediaItem {
    pub id: String,
    pub room_id: String,
    pub platform: String,
    pub title: String,
    pub host_name: String,
    /// 视频文件绝对路径（可能已不存在，配合 has_file 判断）
    pub path: String,
    #[ts(type = "number")]
    pub size: i64,
    #[ts(type = "number")]
    pub duration_ms: i64,
    #[ts(type = "number")]
    pub started_at: i64,
    pub status: String,
    #[ts(type = "number")]
    pub danmaku_count: i64,
    /// 磁盘上还能不能找到这个文件
    pub has_file: bool,
}

/// 本地回放的地址与标题。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct Playback {
    pub url: String,
    pub session_id: String,
    pub title: String,
    #[ts(type = "number")]
    pub duration_ms: i64,
}


/// 已保存的平台账号（v1 单账号，account_id 预留）。
///
/// 不返回 Cookie 本身，只给条目名：设置页只需要展示「带了哪些字段」。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct CredentialInfo {
    pub platform: String,
    pub account_id: String,
    pub account_name: String,
    pub uid: String,
    pub avatar: Option<String>,
    /// Cookie 里的 name 列表（有序、去重），用于确认导入内容
    pub cookie_names: Vec<String>,
    /// ok | expired
    pub status: String,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// 扫码登录的起点：二维码 SVG 由后端渲染，前端直接 innerHTML。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct QrTicket {
    pub qrcode_key: String,
    /// 二维码承载的内容（B 站登录页 URL）
    pub url: String,
    pub svg: String,
}

/// 一次扫码轮询的结果。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct QrPoll {
    /// pending | scanned | confirmed | expired | error
    pub state: String,
    pub message: String,
    /// 只有 confirmed 时才有
    pub credential: Option<CredentialInfo>,
}
