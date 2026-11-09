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
