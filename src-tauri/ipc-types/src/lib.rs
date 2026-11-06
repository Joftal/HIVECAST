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
    /// 是否暂停监控：暂停后不再定时刷新直播间信息
    pub monitor_paused: bool,
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
    /// 实际使用的线路组名
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

/// 后处理进度：task 是录像 id，合并任务用 "merge"。
///
/// 只在 ffmpeg 能算出进度时发送（有总时长），0..99，任务结束由返回值表示。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct PostProgress {
    pub task: String,
    #[ts(type = "number")]
    pub percent: i64,
}

/// 应用级播报：前端据此弹系统通知（文案在前端 i18n，后端不硬编码）。
///
/// kind 取值：started | ended | failed | disk_full（录制）、
/// view_failed（观看槽位放弃重连）、credential_expired（登录态复验失败）。
/// 非录制类的 room_id 为空串，host_name 放房间名或账号名。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct AppNotice {
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
    /// 实际使用的线路组名
    pub line: Option<String>,
    /// 用户手选的线路组名；None 表示自动
    pub preferred_line: Option<String>,
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
    /// 播放与录制异常（取流失败、磁盘写满）是否发系统通知
    pub notify_error: bool,
    /// 登录失效是否发系统通知
    pub notify_auth: bool,
    /// zh-CN | en-US | system
    pub language: String,
    /// light | dark | system（默认 light）
    pub theme: String,
    /// 录像保留天数（0 = 不按天删除）
    pub retention_days: u32,
    /// 媒体库总容量上限 GB（0 = 不限）
    pub max_total_gb: u32,
    /// 录制结束后是否把分片合成 mp4（关闭时保留 .ts 拼接，便于手动救援/剪辑）
    pub auto_remux: bool,
    /// 关闭主窗口时的行为：tray = 最小化到托盘，quit = 退出程序
    pub close_action: String,
    /// 阻止系统空闲休眠（录制/观看期间保持唤醒）
    pub prevent_sleep: bool,
    /// 监控检测间隔（秒）；0 = 关闭自动检测，只能手动刷新
    pub monitor_interval_sec: u32,
    /// 连续失败（限流/网络）时是否自动加大检测间隔
    pub monitor_backoff_enabled: bool,
    /// 自动回退的间隔上限（秒）
    pub monitor_backoff_cap_sec: u32,
    /// 录制文件名模板（不含目录与扩展名）
    pub media_filename_template: String,
    /// 去掉文件名里的 emoji 与特殊符号
    pub media_strip_emoji: bool,
    /// 按「平台/主播」建子目录
    pub media_group_dirs: bool,
}

/// 文件名模板预览：用示例房间渲染一次，前端直接展示。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct FilenamePreview {
    /// 渲染出的文件名（不含扩展名与目录）
    pub name: String,
    /// 完整路径（示例根目录 + 目录结构 + 文件名 + .mp4）
    pub path: String,
    /// 模板是否可用
    pub valid: bool,
    /// 不可用时的原因
    pub error: Option<String>,
}

/// 关闭程序时还没结束的任务（录制、转封装、合并、弹幕导出）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct ActiveTask {
    /// 稳定标识，前端用它做列表 key
    pub id: String,
    /// record | remux | merge | danmaku
    pub kind: String,
    /// 房间标题或文件名，直接展示
    pub title: String,
    /// 次要说明（画质、来源文件），可为空
    pub detail: Option<String>,
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
    /// 九宫格缩略图的回环地址；还没生成时为 null
    pub thumbnail_url: Option<String>,
}

/// 媒体库一页数据：items 是本页，total 是当前筛选条件下的总条数。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct MediaPage {
    pub items: Vec<MediaItem>,
    #[ts(type = "number")]
    pub total: i64,
    /// 当前筛选条件下各平台的条数（不受平台筛选本身影响），给下拉菜单显示计数。
    pub platform_counts: Vec<PlatformCount>,
}

/// 媒体库的「主播卡」：按房间聚合出的一行汇总（v1 主播身份 = room.id）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct MediaHost {
    pub room_id: String,
    pub platform: String,
    /// 主播名（room.host_name），卡片主标题
    pub host_name: String,
    /// 房间标题（room.title），卡片副标题
    pub room_title: String,
    pub avatar_url: Option<String>,
    pub cover_url: Option<String>,
    /// 这个主播名下的录像条数
    #[ts(type = "number")]
    pub count: i64,
    /// 这些录像的体积之和（字节）
    #[ts(type = "number")]
    pub bytes: i64,
    /// 最新一条录像的开始时间（unix 秒）
    #[ts(type = "number")]
    pub latest_at: i64,
}

/// 主播墙一页数据。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct MediaHostPage {
    pub items: Vec<MediaHost>,
    /// 当前筛选条件下的主播总数
    #[ts(type = "number")]
    pub total: i64,
    /// 各平台的主播数（不受平台筛选本身影响）
    pub platform_counts: Vec<PlatformCount>,
}

/// 媒体库里某个平台的条数。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct PlatformCount {
    pub platform: String,
    #[ts(type = "number")]
    pub count: i64,
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
    /// 本次窗口从录像的第几毫秒开始（M13 滑窗回放）
    #[ts(type = "number")]
    pub start_ms: i64,
    /// 本次窗口的长度（毫秒）
    #[ts(type = "number")]
    pub window_ms: i64,
    /// 窗口起点比播放点提前的毫秒数（用于「继续往后看」时无缝衔接）
    #[ts(type = "number")]
    pub lead_ms: i64,
    /// 起播时刻在窗口内的偏移（毫秒）：新窗口应该从这里开始播
    #[ts(type = "number")]
    pub offset_ms: i64,
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
    /// 二维码有效期（秒），从生成时刻算起
    #[ts(type = "number")]
    pub expires_in: i64,
}

/// 一次扫码轮询的结果。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct QrPoll {
    /// pending | scanned | confirmed | expired | error
    pub state: String,
    pub message: String,
    /// 还剩多少秒失效（服务端给出的剩余时间，0 表示未知）
    #[ts(type = "number")]
    pub expires_in: i64,
    /// 只有 confirmed 时才有
    pub credential: Option<CredentialInfo>,
}
