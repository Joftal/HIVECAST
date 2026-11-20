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
    /// 当前实际生效的 ffmpeg（手动配置优先，其次 PATH 与常见目录）；未找到时为 None。
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
    /// 最近一场录制的摘要；由 session 写入/收尾时刷新，监控列表不再单独查 sessions。
    pub last_session_id: Option<String>,
    #[ts(type = "number")]
    pub last_session_started_at: Option<i64>,
    #[ts(type = "number")]
    pub last_session_duration_ms: i64,
    /// 最近一场录制的最终状态、原因与文件大小；详情抽屉用来显示结果。
    pub last_session_status: Option<String>,
    pub last_session_reason: Option<String>,
    #[ts(type = "number")]
    pub last_session_file_size: i64,
    /// 最近一场录制实际拿到的清晰度描述（如「2K · FLV · H.264」）；None 表示还没有录制。
    pub last_session_quality: Option<String>,
    /// 主播本地备注（可当成本地 ID）与标签（逗号分隔）。
    pub note: String,
    pub tags: String,
    /// 单房间自动录制规则：标题必须包含 / 不能包含的关键词（逗号分隔）。
    pub title_include: Option<String>,
    pub title_exclude: Option<String>,
    /// 单房间分片覆盖；None = 跟随全局设置，0 = 关闭该项。
    #[ts(type = "number")]
    pub split_max_duration_sec: Option<i64>,
    #[ts(type = "number")]
    pub split_max_size_mb: Option<i64>,
    /// 单房间通知覆盖；None = 跟随全局设置。
    pub notify_live: Option<bool>,
    pub notify_error: Option<bool>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    /// 平台账号 UID；B 站批量状态接口按 UID 查询，关注导入时写入。
    pub platform_uid: Option<String>,
    /// 是否来自关注导入：抖音关注房走 follow/feed 批量状态，手动房仍单房检查。
    #[serde(default)]
    pub batch_follow: bool,
    /// 平台内部房间号：抖音 follow/feed 批量状态必须用它（19 位），
    /// room_key 是页面 URL 上的 web_rid（12 位），两者不能混用。
    #[serde(default)]
    pub internal_room_id: Option<String>,
    /// 软删除时间；仅在「已移除」列表里有值。
    #[ts(type = "number | null")]
    pub removed_at: Option<i64>,
}

/// 已移除的直播间：房间信息 + 仍在磁盘上的录像汇总。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RemovedRoom {
    pub room: RoomRecord,
    /// 这个房间的录像场次（按 group 去重）。
    #[ts(type = "number")]
    pub session_count: i64,
    /// 这些录像的体积之和（字节）。
    #[ts(type = "number")]
    pub total_bytes: i64,
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
    /// 本场目标档位与展示名（实际 quality 可能因匿名/Cookie 失效而降档）
    #[ts(type = "number")]
    pub target_qn: u32,
    pub target_quality: String,
}

/// 单个房间的录制历史汇总；详情抽屉的合计/可合并数不依赖分页结果。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct SessionSummary {
    #[ts(type = "number")]
    pub total: i64,
    #[ts(type = "number")]
    pub total_bytes: i64,
    #[ts(type = "number")]
    pub mergeable: i64,
    #[ts(type = "number")]
    pub total_duration_ms: i64,
    /// 最近 7 天的场次、时长与体积。
    #[ts(type = "number")]
    pub recent_count: i64,
    #[ts(type = "number")]
    pub recent_duration_ms: i64,
    #[ts(type = "number")]
    pub recent_bytes: i64,
    /// 失败/中断场次，用于成功率。
    #[ts(type = "number")]
    pub failed_count: i64,
}

/// 正在录制的文件大小与所在磁盘剩余空间；详情抽屉轮询显示。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RecordingUsage {
    #[ts(type = "number")]
    pub bytes: i64,
    #[ts(type = "number")]
    pub disk_free: i64,
}

/// 监控排期快照：详情抽屉显示下次检查、退避与失败计数。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct MonitorStatus {
    pub room_id: String,
    #[ts(type = "number")]
    pub next_check_in_secs: i64,
    pub inflight: bool,
    pub backoff: bool,
    #[ts(type = "number")]
    pub throttle_streak: u32,
    #[ts(type = "number")]
    pub error_count: u32,
    #[ts(type = "number")]
    pub last_checked_at: Option<i64>,
    pub last_outcome: Option<String>,
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

/// 添加房间的结果：`created=false` 表示本地已有同一直播间，本次没有新增。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct AddRoomResult {
    pub room: RoomRecord,
    pub recording: Option<RecordingInfo>,
    pub error: Option<String>,
    pub created: bool,
}

/// B 站关注列表中的一位主播。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct FollowingUser {
    pub uid: String,
    pub uname: String,
    pub avatar_url: Option<String>,
}

/// 关注列表的一页；B 站每页最多返回 50 条。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct FollowingPage {
    #[ts(type = "number")]
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub items: Vec<FollowingUser>,
}

/// 斗鱼关注列表的一页。
///
/// 斗鱼接口没有独立的主播 UID，房间号就是稳定键；`items` 直接带直播状态，
/// 导入弹窗不需要再逐房间请求 betard。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct DouyuFollowingPage {
    #[ts(type = "number")]
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub page_count: u32,
    pub items: Vec<LiveStatus>,
}

/// 虎牙关注列表的一页。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct HuyaFollowingPage {
    #[ts(type = "number")]
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub page_count: u32,
    pub items: Vec<LiveStatus>,
}

/// 抖音关注列表中的一位主播。
///
/// `web_rid` 是直播间页面号，能直接建房/取流；`internal_room_id` 是 follow/feed
/// 批量状态用的 19 位内部号。离线关注通常两者都没有，需要留在关注池里等开播。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct DouyinFollow {
    pub uid: String,
    #[serde(default)]
    pub sec_uid: String,
    pub uname: String,
    pub avatar_url: Option<String>,
    pub is_live: bool,
    pub web_rid: Option<String>,
    pub internal_room_id: Option<String>,
    pub title: String,
    pub cover_url: Option<String>,
    pub area_name: Option<String>,
    #[ts(type = "number | null")]
    pub online: Option<u64>,
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    /// 已在关注池里（含已转成正式房间的项）。
    #[serde(default)]
    pub in_pool: bool,
    /// 转成正式房间后是否继续监控。
    #[serde(default = "default_true")]
    pub monitor: bool,
    /// 转成正式房间后是否自动录制。
    #[serde(default)]
    pub auto_record: bool,
    /// 关注池项转成的正式房间 ID；为空表示还在等开播。
    pub promoted_room_id: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 抖音关注列表的一页。分页游标是上一页的 `min_time`，不是页码。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct DouyinFollowingPage {
    pub items: Vec<DouyinFollow>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// 一批关注项加入关注池的结果。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct DouyinPoolAddResult {
    pub added: u32,
    pub updated: u32,
}

/// 一位关注主播的直播状态；room_id 为空表示该用户没有直播间。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct LiveStatus {
    pub uid: String,
    pub room_id: Option<String>,
    pub short_id: Option<String>,
    pub uname: String,
    pub title: String,
    pub avatar_url: Option<String>,
    pub cover_url: Option<String>,
    pub area_name: Option<String>,
    pub is_live: bool,
    /// 斗鱼轮播录像：show_status 可能是 1，但 videoLoop=1 不算真正开播。
    #[serde(default)]
    pub video_loop: bool,
    /// 平台给出的本场开播时间（Unix 秒）；0/缺失表示平台没有可用的开播时间。
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub online: Option<u64>,
}

/// 导入一条关注主播所需的快照；由前端从关注列表与批量状态拼出来。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct ImportRoomInput {
    /// 关注来源平台；缺省按 B 站处理，兼容旧调用方。
    #[serde(default)]
    pub platform: Option<String>,
    pub uid: String,
    pub room_id: String,
    pub short_id: Option<String>,
    pub uname: String,
    pub title: String,
    pub avatar_url: Option<String>,
    pub cover_url: Option<String>,
    pub area_name: Option<String>,
    pub is_live: bool,
    /// 平台给出的本场开播时间；导入时带上，避免沿用上一次的开播时间。
    #[serde(default)]
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub online: Option<u64>,
    /// 斗鱼轮播标志；导入时写进房间 extra，避免刷新后误判。
    #[serde(default)]
    pub video_loop: Option<bool>,
    /// 抖音内部 19 位 room_id（follow/feed 批量状态用）；web_rid 才是 room_key。
    #[serde(default)]
    pub internal_room_id: Option<String>,
}

/// 单条导入失败的原因；不阻断其他条目。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct ImportFailure {
    pub uid: String,
    pub uname: String,
    pub message: String,
}

/// 批量导入结果：新增、已存在与失败分开计数，前端可逐条提示。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct ImportRoomsResult {
    pub added: u32,
    pub existing: u32,
    pub failed: Vec<ImportFailure>,
}

/// 一场录制内的一个实际画质区间。
///
/// 断流续录可能拿到不同档位：起点 `at` 是这一档开始生效的 Unix 秒，
/// 最后一段的终点取会话结束时间。只在档位真的变化时追加，避免同档重复。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct QualitySegment {
    #[ts(type = "number")]
    pub at: i64,
    pub qn: u32,
    pub quality: String,
    pub line: String,
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
    /// 本场各时段的实际画质；空数组表示旧记录只有单值 quality
    pub quality_history: Vec<QualitySegment>,
    /// 实际使用的线路组名
    pub line: Option<String>,
    pub error: Option<String>,
    /// 与视频同名的弹幕存档（JSONL），未开启弹幕时为空
    pub danmaku_path: Option<String>,
    /// 本场落盘的弹幕条数
    #[ts(type = "number")]
    pub danmaku_count: i64,
    /// 源封装母带路径；后处理删源后仍保留，便于追溯
    pub source_path: Option<String>,
    /// 后处理模式：source | convert | custom
    pub post_mode: String,
    /// 后处理状态：recording | sealed | posting | validating | committing | done | failed
    pub post_state: String,
    /// 源封装母带是否已移入回收站
    pub source_deleted: bool,
    /// 后处理失败原因；失败时 file_path 仍指向可播放的源文件
    pub post_error: Option<String>,
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

/// 后处理进度：task 是任务 id，session_id 用来对应媒体库条目。
///
/// stage 取值：queued | preparing | converting | validating | done | failed | cancelled。
/// state 是粗粒度状态：queued | running | done | failed | cancelled。
/// 转换阶段按 ffmpeg 的 out_time 计算百分比；算不出时保持 5%，
/// 任务结束由 done/failed/cancelled 事件收口。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct PostProgress {
    pub task: String,
    pub session_id: String,
    #[ts(type = "number")]
    pub percent: i64,
    pub stage: String,
    pub state: String,
    pub message: Option<String>,
}

/// 应用级播报：前端据此弹系统通知（文案在前端 i18n，后端不硬编码）。
///
/// kind 取值：started | ended | failed | disk_full | disk_lost（录制）、
/// quality_fallback（所选画质不可用，已降档开录）、view_failed（观看槽位放弃重连）、
/// credential_expired（登录态复验失败）。
/// 非录制类的 room_id 为空串，host_name 放房间名或账号名。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct AppNotice {
    pub kind: String,
    pub room_id: String,
    pub host_name: String,
    pub detail: Option<String>,
    /// 单房间通知关闭时为 true；前端据此静默本条通知。
    pub suppress: bool,
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

/// 一档画质：qn 是应用内请求用的统一值，label/hint 是平台真实名称与补充信息。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct QualityInfo {
    pub qn: u32,
    pub label: String,
    /// 码率、编码或「当前不可用」的原因。
    pub hint: Option<String>,
    /// 当前房间 / 账号是否真的可选。
    pub available: bool,
    /// h264 / h265 / av1 / unknown
    pub codec: Option<String>,
}

/// 当前房间的画质与线路能力清单。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct StreamOptions {
    pub qualities: Vec<QualityInfo>,
    pub lines: Vec<LineInfo>,
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
    /// 仅礼物、SC、上舰携带；三平台普通聊天在服务端归一化时已清空。
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
    /// 新房间的默认录制画质（qn；0 = 跟随平台，40000 = 最高可用，10000 = 原画）
    pub default_record_qn: u32,
    /// 新房间的默认观看画质（0 = 自动）
    pub default_view_qn: u32,
    /// 拉流源格式偏好：auto | flv | hls | ts
    pub record_source_format: String,
    /// 拉流编码偏好：auto | h264 | h265
    pub record_codec: String,
    /// 房间标题变化时切分录像
    pub split_on_title: bool,
    /// 单片最大时长（秒）；0 = 不限
    pub split_max_duration_sec: u32,
    /// 单片最大体积（MB，1 MB = 1024 × 1024 字节）；0 = 不限
    pub split_max_size_mb: u32,
    /// 媒体库根目录
    pub media_root: String,
    /// 手动指定的 ffmpeg 可执行文件（绝对路径）；空串表示自动检测
    pub ffmpeg_path: String,
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
    /// 后处理模式：source（只保留源封装）| convert（转成指定封装）| custom（自定义 ffmpeg 参数）
    pub post_mode: String,
    /// convert 模式的目标封装：mp4 | mkv | ts
    pub post_convert_format: String,
    /// convert 成功后是否把源封装移入系统回收站
    pub post_delete_source: bool,
    /// custom 模式的完整 ffmpeg 参数模板，必须包含 {input} 与 {output}
    pub post_custom_command: String,
    /// custom 模式的输出扩展名（不含点）
    pub post_custom_output_ext: String,
    /// custom 模式的超时时间（秒）
    pub post_custom_timeout_sec: u32,
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
    /// 日志目录；空串表示使用平台默认目录
    pub log_dir: String,
    /// 日志保留天数（0 = 不按天清理，磁盘占用只受日志量与保留窗口影响）
    pub log_retention_days: u32,
    /// 录像是否保存弹幕（关闭后不生成 JSONL/ASS，观看端实时弹幕不受影响）
    pub record_danmaku: bool,
    /// 录像弹幕是否保存礼物事件
    pub record_danmaku_gift: bool,
    /// 录像弹幕是否保存醒目留言（B 站 SC）
    pub record_danmaku_super_chat: bool,
    /// 录像弹幕是否保存舰长开通（B 站）
    pub record_danmaku_guard: bool,
}

/// ffmpeg 探测状态：设置页用它显示实际使用的路径与来源。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct FfmpegStatus {
    /// 手动配置的路径；空串表示自动检测
    pub configured: String,
    /// 当前实际生效的绝对路径；未找到时为 None
    pub path: Option<String>,
    /// manual | path | candidate | missing
    pub source: String,
    /// ffmpeg -version 的首行（截断到 160 字符）
    pub version: Option<String>,
}

/// 日志运行状态：设置页用它显示「实际写到哪里、有没有降级」。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct LogStatus {
    /// 当前实际生效的目录
    pub dir: String,
    /// 平台默认目录（设置里清空时用它）
    pub default_dir: String,
    /// 设置里保存的目录；空串表示跟随默认
    pub configured: String,
    /// 生效的保留天数
    pub retention_days: u32,
    /// 当前正在写的文件（没有则为空）
    pub current_file: Option<String>,
    /// 目录里的日志文件数（含分段）
    pub file_count: u64,
    /// 日志文件总字节数
    pub total_bytes: u64,
    /// 因队列满/pending 溢出/写失败被丢弃的日志片段数（按写入批次计，不是行数）
    pub dropped: u64,
    /// 最近一次写失败原因
    pub last_error: Option<String>,
    /// 最近一次运行中降级：从哪个目录回落到哪个目录
    pub fallback: Option<String>,
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

/// 任务中心的一条记录：运行中的任务来自内存，结束后的任务来自数据库。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct TaskRecord {
    /// 稳定标识；录制是 record:{session}，后处理沿用 post::run 的 task id
    pub id: String,
    /// record | post | remux | merge | danmaku
    pub kind: String,
    pub title: String,
    pub detail: Option<String>,
    /// running | done | failed | cancelled | interrupted
    pub state: String,
    #[ts(type = "number")]
    pub percent: i64,
    /// queued / preparing / running / validating / committing / done / failed / cancelled
    pub stage: Option<String>,
    pub message: Option<String>,
    pub session_id: Option<String>,
    pub room_id: Option<String>,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number")]
    pub finished_at: Option<i64>,
}

/// 任务中心首页：运行中 + 最近历史。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct TaskBoard {
    pub running: Vec<TaskRecord>,
    pub history: Vec<TaskRecord>,
}

/// 任务中心的一条 Run：从监控/开录到后处理入库的一条完整链路。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct TaskCenter {
    #[ts(type = "number")]
    pub generated_at: i64,
    pub summary: TaskSummary,
    pub platforms: Vec<PlatformHealth>,
    pub system: SystemHealth,
    /// 正在录制或正在后处理的 Run（按开始时间倒序）
    pub running: Vec<RunView>,
    /// 正在等后处理槽位的任务（不伪造队列序号）
    pub queued: Vec<TaskView>,
    /// 最近完成的 Run（按结束时间倒序）
    pub history: Vec<RunView>,
    pub incidents: Vec<Incident>,
}

/// 系统行：ffmpeg 是否可用，以及媒体库磁盘剩余。P1 只读，不单独建表。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct SystemHealth {
    pub ffmpeg_ok: bool,
    #[ts(type = "number | null")]
    pub disk_free_bytes: Option<i64>,
    pub disk_low: bool,
    pub media_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct TaskSummary {
    #[ts(type = "number")]
    pub rooms: i64,
    #[ts(type = "number")]
    pub live: i64,
    #[ts(type = "number")]
    pub recording: i64,
    #[ts(type = "number")]
    pub queued: i64,
    #[ts(type = "number")]
    pub running: i64,
    /// 最近 24 小时失败的 Run 数
    #[ts(type = "number")]
    pub failed: i64,
    #[ts(type = "number")]
    pub incidents: i64,
}

/// Run 关联的直播间；房间被移除后历史 Run 仍然保留标题。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RunRoom {
    pub id: String,
    pub platform: String,
    pub room_key: String,
    pub host_name: String,
    pub title: String,
    pub avatar_url: Option<String>,
    /// 当前是否直播中；用于筛选单个直播间后的统计卡。
    pub is_live: bool,
}

/// 一条 Run 的阶段快照；只有可量化阶段才带 percent。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct StageView {
    /// record | finalize | post | commit，或 merging / exporting
    pub key: String,
    /// pending | active | done | failed | cancelled | skipped
    pub state: String,
    pub detail: Option<String>,
    #[ts(type = "number | null")]
    pub percent: Option<i64>,
}

/// 一条 Run 展示所需的全部信息。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RunView {
    /// session:{id} 或 task:{id}，前端做 key 与详情选择
    pub id: String,
    pub room: Option<RunRoom>,
    /// recording | finalizing | running | queued | done | failed | partial | cancelled | interrupted
    pub state: String,
    /// recording / starting / finalizing / postprocessing / merging / exporting / queued / done / failed / cancelled / interrupted
    pub stage: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub ended_at: Option<i64>,
    #[ts(type = "number")]
    pub duration_ms: i64,
    #[ts(type = "number")]
    pub bytes: i64,
    pub quality: Option<String>,
    pub tasks: Vec<TaskView>,
    pub stages: Vec<StageView>,
}

/// Run 下的一个可执行任务（录制、转封装、合并、弹幕导出）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct TaskView {
    pub id: String,
    /// record | post | remux | merge | danmaku | finalize
    pub kind: String,
    pub title: String,
    pub detail: Option<String>,
    /// running | queued | done | failed | cancelled | interrupted
    pub state: String,
    /// queued / preparing / running / validating / committing / done / failed / cancelled
    pub stage: Option<String>,
    #[ts(type = "number")]
    pub percent: i64,
    pub message: Option<String>,
    pub session_id: Option<String>,
    pub room_id: Option<String>,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
    /// 多片段收尾时的片段序号（从 1 开始）；单片段或非分片任务为 null。
    #[ts(type = "number | null")]
    pub clip_index: Option<u32>,
    /// 同一条 Run 的片段总数；0 表示未知/不适用。
    #[ts(type = "number")]
    pub clip_count: u32,
    /// auto | manual | merge | record
    pub queue: String,
    #[ts(type = "number | null")]
    pub queued_at: Option<i64>,
}

/// 四平台运行态势：登录态、房间/直播/录制数、批量关注与监控退避。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct PlatformHealth {
    pub platform: String,
    /// ok | expired | none
    pub auth: String,
    #[ts(type = "number")]
    pub rooms: i64,
    #[ts(type = "number")]
    pub live: i64,
    #[ts(type = "number")]
    pub recording: i64,
    #[ts(type = "number")]
    pub paused: i64,
    #[ts(type = "number")]
    pub batch_rooms: i64,
    pub checking: bool,
    pub backoff: bool,
    #[ts(type = "number | null")]
    pub last_checked_at: Option<i64>,
    #[ts(type = "number | null")]
    pub next_check_in_secs: Option<i64>,
    /// ok | warn | error | idle
    pub level: String,
    pub message: Option<String>,
}

/// 需要处理的一条异常；id 稳定，前端可去重。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct Incident {
    pub id: String,
    /// task_failed | record_failed | monitor_backoff | login_expired | ffmpeg_missing | disk_low
    pub code: String,
    /// error | warn
    pub severity: String,
    /// task | record | monitor | platform | system
    pub scope: String,
    pub platform: Option<String>,
    pub room_id: Option<String>,
    pub task_id: Option<String>,
    pub title: String,
    pub detail: Option<String>,
    #[ts(type = "number")]
    pub at: i64,
    /// retry_post | open_room | open_account | open_storage | open_task
    pub action: String,
}
/// 一条录像登记在册的文件（主文件、源封装母带、派生文件）。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct MediaFile {
    pub session_id: String,
    pub path: String,
    /// main | source | derived
    pub role: String,
    #[ts(type = "number")]
    pub size: i64,
    /// 扩展名（不含点），未知为 null
    pub format: Option<String>,
    /// present | deleted | missing
    pub state: String,
    /// 此刻磁盘上是否还能找到
    pub exists: bool,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub deleted_at: Option<i64>,
}

/// 当前平台的回收站能力，用于设置页和删源确认文案。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct TrashInfo {
    /// macos | windows | linux
    pub platform: String,
    /// system | freedesktop | plain
    pub mode: String,
    /// 系统文件管理器里能否直接还原
    pub can_restore: bool,
    /// 回收站目录（文件实际被移动到的位置）
    pub location: String,
    /// 给用户看的平台差异说明
    pub note: String,
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
    /// 弹幕 JSONL 的绝对路径；没有弹幕时为 null
    pub danmaku_path: Option<String>,
    /// 磁盘上还能不能找到这个文件
    pub has_file: bool,
    /// 九宫格缩略图的回环地址；还没生成时为 null
    pub thumbnail_url: Option<String>,
    /// 同一场直播的分组：首片段的会话 id；老数据等于自己的 id
    pub group_id: String,
    /// 片段序号，从 0 开始
    #[ts(type = "number")]
    pub clip_index: i64,
    /// 这一场直播一共有多少个片段
    #[ts(type = "number")]
    pub clip_count: i64,
    /// 为什么从这里切开：title / duration / size；未分割为 null
    pub split_reason: Option<String>,
    /// 源封装母带路径；源被删除后仍保留
    pub source_path: Option<String>,
    /// 后处理模式：source | convert | custom
    pub post_mode: String,
    /// 后处理状态：recording | sealed | posting | validating | committing | done | failed
    pub post_state: String,
    /// 源封装母带是否已移入回收站
    pub source_deleted: bool,
    /// 后处理失败原因
    pub post_error: Option<String>,
    /// 磁盘上是否还有源封装母带
    pub has_source: bool,
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

/// 最近一次 B 站 Cookie 自动续期检查的持久化结果。
///
/// 只记录「检查」本身，不包含 Cookie 内容；时间戳为 0 表示从未发生。
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../src/lib/bindings/")]
pub struct RefreshStatus {
    /// 最近一次检查完成时间（Unix 秒）；0 表示尚未检查
    #[ts(type = "number")]
    pub checked_at: i64,
    /// never | fresh | refreshed | failed | not_refreshable | key_stale
    pub outcome: String,
    /// 失败时的可读原因；成功或未检查时为 None
    pub message: Option<String>,
    /// 续期公钥/页面结构疑似变更：需要重新扫码登录
    pub key_stale: bool,
    /// 最近一次真正轮换 Cookie 的时间（Unix 秒）；0 表示从未轮换
    #[ts(type = "number")]
    pub last_refreshed_at: i64,
}

impl Default for RefreshStatus {
    fn default() -> Self {
        Self {
            checked_at: 0,
            outcome: "never".into(),
            message: None,
            key_stale: false,
            last_refreshed_at: 0,
        }
    }
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
    /// 是否具备自动续期条件（B 站：Cookie 里有 ac_time_value + bili_jct）
    pub refreshable: bool,
    /// 最近一次自动续期检查的时间与结果（非 B 站恒为 never）
    pub refresh_status: RefreshStatus,
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
