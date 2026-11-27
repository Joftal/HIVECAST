import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "./invoke";
import type { ActiveTask } from "./bindings/ActiveTask";
import type { AddRoomResult } from "./bindings/AddRoomResult";
import type { AppInfo } from "./bindings/AppInfo";
import type { ChangelogNote } from "./bindings/ChangelogNote";
import type { CredentialInfo } from "./bindings/CredentialInfo";
import type { FfmpegStatus } from "./bindings/FfmpegStatus";
import type { FilenamePreview } from "./bindings/FilenamePreview";
import type { DouyinFollow } from "./bindings/DouyinFollow";
import type { DouyinFollowingPage } from "./bindings/DouyinFollowingPage";
import type { DouyinPoolAddResult } from "./bindings/DouyinPoolAddResult";
import type { DouyuFollowingPage } from "./bindings/DouyuFollowingPage";
import type { HuyaFollowingPage } from "./bindings/HuyaFollowingPage";
import type { FollowingPage } from "./bindings/FollowingPage";
import type { FollowingUser } from "./bindings/FollowingUser";
import type { ImportRoomInput } from "./bindings/ImportRoomInput";
import type { ImportRoomsResult } from "./bindings/ImportRoomsResult";
import type { LiveStatus } from "./bindings/LiveStatus";
import type { QrPoll } from "./bindings/QrPoll";
import type { QrTicket } from "./bindings/QrTicket";
import type { RefreshStatus } from "./bindings/RefreshStatus";
import type { ReleaseEntry } from "./bindings/ReleaseEntry";
import type { DanmakuBatch } from "./bindings/DanmakuBatch";
import type { DanmakuMessage } from "./bindings/DanmakuMessage";
import type { LineInfo } from "./bindings/LineInfo";
import type { LogStatus } from "./bindings/LogStatus";
import type { MediaFile } from "./bindings/MediaFile";
import type { MediaItem } from "./bindings/MediaItem";
import type { MediaPage } from "./bindings/MediaPage";
import type { RemovedRoom } from "./bindings/RemovedRoom";
import type { RoomRecord } from "./bindings/RoomRecord";
import type { PlatformCount } from "./bindings/PlatformCount";
import type { Playback } from "./bindings/Playback";
import type { QualityInfo } from "./bindings/QualityInfo";
import type { Settings } from "./bindings/Settings";
import type { PostProgress } from "./bindings/PostProgress";
import type { Incident } from "./bindings/Incident";
import type { PlatformHealth } from "./bindings/PlatformHealth";
import type { RunRoom } from "./bindings/RunRoom";
import type { RunView } from "./bindings/RunView";
import type { StageView } from "./bindings/StageView";
import type { TaskBoard } from "./bindings/TaskBoard";
import type { TaskCenter } from "./bindings/TaskCenter";
import type { TaskEvent } from "./bindings/TaskEvent";
import type { TaskEventHit } from "./bindings/TaskEventHit";
import type { TaskEventPage } from "./bindings/TaskEventPage";
import type { TaskEventExport } from "./bindings/TaskEventExport";
import type { TaskEventSearch } from "./bindings/TaskEventSearch";
import type { TaskSummary } from "./bindings/TaskSummary";
import type { SystemHealth } from "./bindings/SystemHealth";
import type { TaskView } from "./bindings/TaskView";
import type { TaskRecord } from "./bindings/TaskRecord";
import type { TrashInfo } from "./bindings/TrashInfo";
import type { UpdateCheckResult } from "./bindings/UpdateCheckResult";
import type { PostResult } from "./bindings/PostResult";
import type { AppNotice } from "./bindings/AppNotice";
import type { RecordSession } from "./bindings/RecordSession";
import type { SessionSummary } from "./bindings/SessionSummary";
import type { MonitorStatus } from "./bindings/MonitorStatus";
import type { RecordingUsage } from "./bindings/RecordingUsage";
import type { RoomView } from "./bindings/RoomView";
import type { StreamDescriptor } from "./bindings/StreamDescriptor";
import type { StreamOptions } from "./bindings/StreamOptions";
import type { ViewSlotInfo } from "./bindings/ViewSlotInfo";

export type {
  ActiveTask,
  AppInfo,
  CredentialInfo,
  FfmpegStatus,
  FilenamePreview,
  DouyinFollow,
  DouyinFollowingPage,
  DouyinPoolAddResult,
  DouyuFollowingPage,
  FollowingPage,
  FollowingUser,
  ImportRoomInput,
  ImportRoomsResult,
  LiveStatus,
  QrPoll,
  QrTicket,
  RefreshStatus,
  DanmakuBatch,
  DanmakuMessage,
  LineInfo,
  LogStatus,
  MediaFile,
  MediaItem,
  MediaPage,
  RemovedRoom,
  RoomRecord,
  PlatformCount,
  Playback,
  PostProgress,
  PostResult,
  QualityInfo,
  TaskBoard,
  TaskCenter,
  TaskEvent,
  TaskEventHit,
  TaskEventPage,
  TaskEventExport,
  TaskEventSearch,
  TaskRecord,
  TaskSummary,
  SystemHealth,
  TaskView,
  RunRoom,
  RunView,
  StageView,
  PlatformHealth,
  Incident,
  TrashInfo,
  AppNotice,
  RecordSession,
  RoomView,
  SessionSummary,
  MonitorStatus,
  RecordingUsage,
  Settings,
  StreamDescriptor,
  StreamOptions,
  ChangelogNote,
  ReleaseEntry,
  UpdateCheckResult,
  ViewSlotInfo,
};

// 浏览器里跑 Vite 预览时没有 Tauri 运行时，界面上要给出明确提示而不是空白。
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export async function appOpenDataDir(): Promise<void> {
  return invoke<void>("app_open_data_dir");
}

export async function appCheckUpdate(): Promise<UpdateCheckResult> {
  return invoke<UpdateCheckResult>("app_check_update");
}

// M0 演示流：本机 ffmpeg 生成 HLS，经本地 HTTP 服务交给 hls.js。
export async function openDemoStream(): Promise<StreamDescriptor> {
  return invoke<StreamDescriptor>("demo_stream");
}

export function describeError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object" && "message" in err) {
    const message = (err as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return String(err);
}

// ---- M1 监控 ----

export const EVENT_ROOMS_CHANGED = "rooms://changed";
/** 单房间增量事件：payload 是完整 RoomView，监控/观看页只替换这一行。 */
export const EVENT_ROOM_CHANGED = "room://changed";

export async function roomsList(): Promise<RoomView[]> {
  return invoke<RoomView[]>("rooms_list");
}

// input 支持房间号、直播间链接、主播主页链接；解析在 Rust 侧完成。
export async function roomAdd(
  input: string,
  monitor = true,
  autoRecord = true,
): Promise<AddRoomResult> {
  return invoke<AddRoomResult>("room_add", { input, monitor, autoRecord });
}

/** B 站关注列表；关注列表每页最多 50 条。 */
export async function bilibiliFollowings(page = 1): Promise<FollowingPage> {
  return invoke<FollowingPage>("bilibili_followings", { page });
}

/** 按 UID 批量查询直播状态；没有返回的 UID 表示该用户没有直播间。 */
export async function bilibiliFollowStatuses(uids: string[]): Promise<LiveStatus[]> {
  return invoke<LiveStatus[]>("bilibili_follow_statuses", { uids });
}

/** 斗鱼关注列表；每页最多 120 条，条目已带直播状态，不需要再批量查一次。 */
export async function douyuFollowPage(page = 1): Promise<DouyuFollowingPage> {
  return invoke<DouyuFollowingPage>("douyu_follow_page", { page });
}

/** 虎牙关注列表；每页最多 120 条，条目已带直播状态。 */
export async function huyaFollowPage(page = 1): Promise<HuyaFollowingPage> {
  return invoke<HuyaFollowingPage>("huya_follow_page", { page });
}

/** 抖音关注列表的一页；cursor 传上一页的 nextCursor。 */
export async function douyinFollowPage(cursor?: string | null): Promise<DouyinFollowingPage> {
  return invoke<DouyinFollowingPage>("douyin_follow_page", { cursor: cursor ?? null });
}

/** 把暂时没有直播间的抖音关注放进关注池，开播后自动转成房间。 */
export async function douyinPoolAdd(
  items: DouyinFollow[],
  monitor = true,
  autoRecord = false,
): Promise<DouyinPoolAddResult> {
  return invoke<DouyinPoolAddResult>("douyin_pool_add", { items, monitor, autoRecord });
}

/** 把关注列表里选中的主播导入监控列表；已存在的房间只刷新展示信息。 */
export async function roomsImport(
  items: ImportRoomInput[],
  monitor = true,
  autoRecord = false,
): Promise<ImportRoomsResult> {
  return invoke<ImportRoomsResult>("rooms_import", { items, monitor, autoRecord });
}

export async function removedRooms(): Promise<RemovedRoom[]> {
  return invoke<RemovedRoom[]>("removed_rooms");
}

/** 从“已移除”里找回直播间：恢复后重新进入监控列表。 */
export async function roomRestore(id: string): Promise<RoomView> {
  return invoke<RoomView>("room_restore", { id });
}

export async function roomRemove(id: string): Promise<void> {
  return invoke("room_remove", { id });
}

export async function roomRefresh(id: string): Promise<RoomView> {
  return invoke<RoomView>("room_refresh", { id });
}

export async function roomSetAutoRecord(id: string, enabled: boolean): Promise<void> {
  return invoke("room_set_auto_record", { id, enabled });
}

export type RoomRulesInput = {
  note: string;
  tags: string;
  titleInclude: string | null;
  titleExclude: string | null;
  splitMaxDurationSec: number | null;
  splitMaxSizeMb: number | null;
  notifyLive: boolean | null;
  notifyError: boolean | null;
};

export async function roomSetRules(id: string, rules: RoomRulesInput): Promise<void> {
  return invoke("room_set_rules", { id, ...rules });
}

/** 暂停/恢复直播状态监控（暂停只停轮询，不影响已有录制）。 */
export async function roomSetMonitor(id: string, paused: boolean): Promise<void> {
  return invoke("room_set_monitor", { id, paused });
}

export async function roomSetQuality(id: string, recordQn: number, viewQn: number): Promise<void> {
  return invoke("room_set_quality", { id, recordQn, viewQn });
}

/** 列出当前可用线路（需要触网解析，展开选择器时才调）。 */
export async function roomLines(id: string, qn?: number): Promise<LineInfo[]> {
  return invoke<LineInfo[]>("room_lines", { id, qn: qn ?? null });
}

/** 当前房间的画质与线路能力：平台真实档位 + 线路，菜单直接渲染。 */
export async function roomStreamOptions(id: string, qn?: number): Promise<StreamOptions> {
  return invoke<StreamOptions>("room_stream_options", { id, qn: qn ?? null });
}

/** 指定录制线路；null 回到自动。 */
export async function roomSetLine(id: string, line: string | null): Promise<void> {
  return invoke("room_set_line", { id, line });
}

export async function recordStart(id: string): Promise<void> {
  return invoke("record_start", { id });
}

export async function recordStop(id: string): Promise<void> {
  return invoke("record_stop", { id });
}

export async function sessionsList(roomId?: string, limit = 50, offset = 0): Promise<RecordSession[]> {
  return invoke<RecordSession[]>("sessions_list", { roomId, limit, offset });
}

export async function sessionSummary(roomId: string): Promise<SessionSummary> {
  return invoke<SessionSummary>("session_summary", { roomId });
}

export async function recordingUsage(sessionId: string): Promise<RecordingUsage> {
  return invoke<RecordingUsage>("recording_usage", { sessionId });
}

export async function monitorStatus(roomId: string): Promise<MonitorStatus | null> {
  return invoke<MonitorStatus | null>("monitor_status", { roomId });
}

// ---- M5 本地后处理 ----

export async function sessionRemux(sessionId: string): Promise<PostResult> {
  return invoke<PostResult>("session_remux", { sessionId });
}

/** 重试一条失败/已取消的后处理。 */
export async function postRetry(sessionId: string): Promise<PostResult> {
  return invoke<PostResult>("post_retry", { sessionId });
}

/** 重试一次失败的录制收尾（仍按场次留下的分割清单恢复）。 */
export async function finalizeRetry(sessionId: string): Promise<void> {
  return invoke<void>("finalize_retry", { sessionId });
}

/** 取消一条排队/执行中的后处理，返回命中的任务数。 */
export async function postCancel(sessionId: string): Promise<number> {
  return invoke<number>("post_cancel", { sessionId });
}

export async function roomMergeSessions(roomId: string): Promise<PostResult> {
  return invoke<PostResult>("room_merge_sessions", { roomId });
}

/// 合并选中的多段录像（后端按开录时间排序，跨房间也可以）。
export async function sessionsMerge(sessionIds: string[]): Promise<PostResult> {
  return invoke<PostResult>("sessions_merge", { sessionIds });
}

// 后端在任何房间状态变化（开播/下播/开录/停录/出错）时广播，前端只刷新列表。
export const EVENT_APP_NOTICE = "app://notice";

// 应用级播报（录制 / 观看失败 / 登录态过期）：前端自己用 i18n 组装文案再弹系统通知。
export function onAppNotice(handler: (notice: AppNotice) => void): Promise<UnlistenFn> {
  return listen<AppNotice>(EVENT_APP_NOTICE, (event) => handler(event.payload));
}

// 后处理进度：task 是任务 id，sessionId 对应媒体库条目，stage/state 见 PostProgress。
export const EVENT_POST_PROGRESS = "post://progress";

export function onPostProgress(handler: (progress: PostProgress) => void): Promise<UnlistenFn> {
  return listen<PostProgress>(EVENT_POST_PROGRESS, (event) => handler(event.payload));
}

export function onRoomsChanged(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_ROOMS_CHANGED, () => handler());
}

export function onRoomChanged(handler: (view: RoomView) => void): Promise<UnlistenFn> {
  return listen<RoomView>(EVENT_ROOM_CHANGED, (event) => handler(event.payload));
}

// ---- M2 观看 ----

export const EVENT_VIEW_CHANGED = "view://changed";

export async function viewSlots(): Promise<ViewSlotInfo[]> {
  return invoke<ViewSlotInfo[]>("view_slots");
}

// 把房间放进空闲槽位；qn 省略时用房间自己的观看画质。
export async function viewOpen(roomId: string, qn?: number): Promise<ViewSlotInfo> {
  return invoke<ViewSlotInfo>("view_open", { roomId, qn });
}

export async function viewClose(slot: number): Promise<void> {
  return invoke("view_close", { slot });
}

// 原子替换：同一槽位停旧起新，不经过「先关后开」的空窗。
export async function viewReplace(slot: number, roomId: string): Promise<ViewSlotInfo> {
  return invoke<ViewSlotInfo>("view_replace", { slot, roomId });
}

// 离开观看页时停掉全部观看管道；房间与布局由前端持久化，回来再恢复。
export async function viewSuspend(): Promise<void> {
  return invoke("view_suspend");
}

// 只影响这一路的租约，录制进程不会被打断。
export async function viewSetQuality(slot: number, qn: number): Promise<ViewSlotInfo> {
  return invoke<ViewSlotInfo>("view_set_quality", { slot, qn });
}

export async function viewSetLine(slot: number, line: string | null): Promise<ViewSlotInfo> {
  return invoke<ViewSlotInfo>("view_set_line", { slot, line });
}

export async function viewRetry(slot: number): Promise<ViewSlotInfo> {
  return invoke<ViewSlotInfo>("view_retry", { slot });
}

export function onViewChanged(handler: (slots: ViewSlotInfo[]) => void): Promise<UnlistenFn> {
  return listen<ViewSlotInfo[]>(EVENT_VIEW_CHANGED, (event) => handler(event.payload));
}

// ---- M3 弹幕 ----

// 后端每 200ms 合并一批，前端按 roomId 分发给对应格子的 Canvas。
export const EVENT_DANMAKU_BATCH = "danmaku://batch";

export function onDanmakuBatch(handler: (batch: DanmakuBatch) => void): Promise<UnlistenFn> {
  return listen<DanmakuBatch>(EVENT_DANMAKU_BATCH, (event) => handler(event.payload));
}

// ---- M6 设置与媒体库 ----

export async function settingsGet(): Promise<Settings> {
  return invoke<Settings>("settings_get");
}

/** 当前实际使用的 ffmpeg 路径、来源与版本。 */
export async function ffmpegStatus(): Promise<FfmpegStatus> {
  return invoke<FfmpegStatus>("ffmpeg_status");
}

export type Preferences = Partial<Pick<Settings, "closeAction" | "preventSleep" | "theme" | "language">>;

let settingsWrites: Promise<void> = Promise.resolve();

function enqueueSettings(write: () => Promise<Settings>): Promise<Settings> {
  const result = settingsWrites.then(write);
  settingsWrites = result.then(() => undefined, () => undefined);
  return result;
}

export function settingsPreferences(patch: Preferences): Promise<Settings> {
  return enqueueSettings(() => invoke<Settings>("settings_preferences", { patch }));
}

export function settingsChanges(before: Settings, after: Settings): Partial<Settings> {
  return Object.fromEntries(Object.entries(after).filter(([key, value]) => before[key as keyof Settings] !== value));
}

export function settingsUpdate(patch: Partial<Settings>): Promise<Settings> {
  return enqueueSettings(async () => {
    const current = await settingsGet();
    return invoke<Settings>("settings_update", { settings: { ...current, ...patch } });
  });
}

/** 日志运行状态：实际目录、当前文件、丢弃计数与最近错误。 */
export async function logsStatus(): Promise<LogStatus> {
  return invoke<LogStatus>("logs_status");
}

/** 打开当前实际生效的日志目录（后端不接受路径参数）。 */
export async function logsOpen(): Promise<void> {
  return invoke("logs_open");
}

/** 用示例房间预览文件名模板（与真正落盘同一套逻辑）。 */
export async function filenamePreview(
  template: string,
  stripEmoji: boolean,
  groupDirs: boolean,
  ext: string,
): Promise<FilenamePreview> {
  return invoke<FilenamePreview>("filename_preview", { template, stripEmoji, groupDirs, ext });
}

/** 媒体库筛选条件：字段全部可选，未传就走后端的默认值。 */
export type MediaQuery = {
  query?: string | null;
  platform?: string | null;
  /** 只看某个房间（主播页下钻） */
  roomId?: string | null;
  /** 最近 N 天 */
  days?: number | null;
  onlyDanmaku?: boolean;
  sort?: "recent" | "duration" | "size";
  limit?: number;
  offset?: number;
};

export async function mediaList(query: MediaQuery = {}): Promise<MediaPage> {
  return invoke<MediaPage>("media_list", {
    query: query.query ?? null,
    platform: query.platform ?? null,
    roomId: query.roomId ?? null,
    days: query.days ?? null,
    onlyDanmaku: query.onlyDanmaku ?? false,
    sort: query.sort ?? "recent",
    limit: query.limit ?? 60,
    offset: query.offset ?? 0,
  });
}

/** 补齐一整场直播的全部片段（分页只拿到部分时用）。 */
export async function mediaGroupClips(groupId: string): Promise<MediaItem[]> {
  return invoke<MediaItem[]>("media_group_clips", { groupId });
}

export const EVENT_MEDIA_THUMB = "media://thumb";

export const EVENT_MEDIA_CHANGED = "media://changed";

/** 启动后的崩溃恢复合并完成：媒体库需要重新拉一次列表。 */
export function onMediaChanged(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_MEDIA_CHANGED, () => handler());
}

export type ThumbReady = { sessionId: string; url: string };

/** 九宫格缩略图生成完成（后台限流，一张一张回推）。 */
export function onMediaThumb(handler: (thumb: ThumbReady) => void): Promise<UnlistenFn> {
  return listen<ThumbReady>(EVENT_MEDIA_THUMB, (event) => handler(event.payload));
}

/** 把录像的弹幕导出成 ASS 字幕。 */
export async function sessionExportDanmaku(sessionId: string): Promise<PostResult> {
  return invoke<PostResult>("session_export_danmaku", { sessionId });
}

/** 读取一段录像的弹幕 JSONL，按 offsetMs 升序，供回放调度。 */
export async function mediaDanmaku(
  sessionId: string,
  fromMs?: number,
  toMs?: number,
): Promise<DanmakuMessage[]> {
  return invoke<DanmakuMessage[]>("media_danmaku", { sessionId, fromMs, toMs });
}

export async function mediaPlay(
  sessionId: string,
  positionMs?: number | null,
  source = false,
): Promise<Playback> {
  return invoke<Playback>("media_play", { sessionId, positionMs: positionMs ?? null, source });
}

export async function mediaStop(sessionId: string): Promise<void> {
  return invoke("media_stop", { sessionId });
}

export async function mediaReveal(sessionId: string): Promise<void> {
  return invoke("media_reveal", { sessionId });
}

export async function openUrl(url: string): Promise<void> {
  return invoke("open_url", { url });
}

export async function mediaRevealSource(sessionId: string): Promise<void> {
  return invoke("media_reveal_source", { sessionId });
}

/** 定位这条录像登记过的任意文件（派生文件也适用）。 */
export async function mediaRevealFile(sessionId: string, path: string): Promise<void> {
  return invoke("media_reveal_file", { sessionId, path });
}

/** 一条录像登记过的全部文件（主文件 / 源封装 / 派生）。 */
export async function mediaFiles(sessionId: string): Promise<MediaFile[]> {
  return invoke<MediaFile[]>("media_files", { sessionId });
}

/** 只删除源封装母带（后端会先校验主文件）。 */
export async function mediaDeleteSource(sessionId: string): Promise<void> {
  return invoke("media_delete_source", { sessionId });
}

export async function mediaDelete(sessionId: string, deleteFile: boolean): Promise<void> {
  return invoke("media_delete", { sessionId, deleteFile });
}

/** 删除一整场直播的所有片段，返回删掉的片段数。 */
export async function mediaDeleteGroup(groupId: string, deleteFile: boolean): Promise<number> {
  return invoke<number>("media_delete_group", { groupId, deleteFile });
}


/** 当前平台的回收站能力与位置。 */
export async function trashInfo(): Promise<TrashInfo> {
  return invoke<TrashInfo>("trash_info");
}

/** 在文件管理器中打开回收站。 */
export async function trashOpen(): Promise<void> {
  return invoke("trash_open");
}

// ---- 观看页布局 ----

/** 读分屏树 JSON；没有存过就返回 null。 */
export async function layoutGet(): Promise<string | null> {
  return invoke<string | null>("layout_get");
}

export async function layoutSet(layout: string): Promise<void> {
  return invoke<void>("layout_set", { layout });
}

// ---- M7.2 账号 ----

export async function credentialsList(): Promise<CredentialInfo[]> {
  return invoke<CredentialInfo[]>("credentials_list");
}

export async function credentialSave(
  platform: string,
  cookie: string,
): Promise<CredentialInfo> {
  return invoke<CredentialInfo>("credential_save", { platform, cookie });
}

export async function credentialDelete(platform: string): Promise<void> {
  return invoke<void>("credential_delete", { platform });
}

export async function bilibiliQrStart(): Promise<QrTicket> {
  return invoke<QrTicket>("bilibili_qr_start");
}

export async function bilibiliQrPoll(qrcodeKey: string): Promise<QrPoll> {
  return invoke<QrPoll>("bilibili_qr_poll", { qrcodeKey });
}

/** 立即检查一次 B 站 Cookie 续期；结果同时写入账号的 refreshStatus。 */
export async function bilibiliRefreshNow(): Promise<RefreshStatus> {
  return invoke<RefreshStatus>("bilibili_refresh_now");
}

export async function douyuQrStart(): Promise<QrTicket> {
  return invoke<QrTicket>("douyu_qr_start");
}

export async function douyuQrPoll(qrcodeKey: string): Promise<QrPoll> {
  return invoke<QrPoll>("douyu_qr_poll", { qrcodeKey });
}

/** 立即检查一次斗鱼 Cookie 续期；结果同时写入账号的 refreshStatus。 */
export async function douyuRefreshNow(): Promise<RefreshStatus> {
  return invoke<RefreshStatus>("douyu_refresh_now");
}

/** 立即检查一次抖音 Cookie：轮换 ttwid 并复验登录态。 */
export async function douyinRefreshNow(): Promise<RefreshStatus> {
  return invoke<RefreshStatus>("douyin_refresh_now");
}

export async function huyaQrStart(): Promise<QrTicket> {
  return invoke<QrTicket>("huya_qr_start");
}

export async function huyaQrPoll(qrcodeKey: string): Promise<QrPoll> {
  return invoke<QrPoll>("huya_qr_poll", { qrcodeKey });
}

/** 关闭或刷新虎牙扫码面板时释放后端 session，停止后续轮询。 */
export async function huyaQrCancel(qrcodeKey: string): Promise<void> {
  return invoke<void>("huya_qr_cancel", { qrcodeKey });
}

/** 立即检查一次虎牙 Cookie：cookieExchange/credLogin 并复验登录态。 */
export async function huyaRefreshNow(): Promise<RefreshStatus> {
  return invoke<RefreshStatus>("huya_refresh_now");
}

/** 账号凭据或续期状态变化（登录、保存、过期、续期结果写入）。 */
export const EVENT_ACCOUNT_REFRESH = "account://refresh";

export function onAccountRefresh(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_ACCOUNT_REFRESH, () => handler());
}

/** 主窗口显示/隐藏（关闭到托盘时隐藏）。界面级轮询应只在前台进行。 */
export const EVENT_WINDOW_VISIBILITY = "app://window-visibility";

export function onWindowVisibility(handler: (visible: boolean) => void): Promise<UnlistenFn> {
  return listen<boolean>(EVENT_WINDOW_VISIBILITY, (event) => handler(event.payload));
}

// ---- 关闭与退出 ----

/** 还没结束的录制与后处理任务。 */
export async function activeTasks(): Promise<ActiveTask[]> {
  return invoke<ActiveTask[]>("active_tasks");
}

/** 任务中心：运行中 + 历史任务。 */
export async function taskBoard(limit = 100): Promise<TaskBoard> {
  return invoke<TaskBoard>("task_board", { limit });
}

/// 任务中心全链路快照：Run、平台态势、异常与最近完成。
export async function taskCenter(limit = 100): Promise<TaskCenter> {
  return invoke<TaskCenter>("task_center", { history: limit });
}

/** 任务中心详情时间线：只取一个 Run 的阶段事件，按 (at, id) 游标向更早翻页。 */
export async function taskRunEvents(
  runId: string,
  before?: { at: number; id: number },
  limit = 200,
): Promise<TaskEventPage> {
  return invoke<TaskEventPage>("task_run_events", {
    runId,
    beforeAt: before?.at ?? null,
    beforeId: before?.id ?? null,
    limit,
  });
}

/** 跨 Run 检索阶段事件：不受任务页历史窗口限制。 */
export async function taskEventSearch(
  query: string,
  onlyFailed: boolean,
  platform: string | null,
  limit = 200,
): Promise<TaskEventSearch> {
  return invoke<TaskEventSearch>("task_event_search", {
    query,
    onlyFailed,
    platform,
    limit,
  });
}

/** 把当前检索条件下的阶段事件导出为 CSV，返回实际写入数与匹配总数。 */
export async function taskEventsExport(
  path: string,
  query: string,
  onlyFailed: boolean,
  platform: string | null,
  lang: string,
): Promise<TaskEventExport> {
  return invoke<TaskEventExport>("task_events_export", { path, query, onlyFailed, platform, lang });
}

/** 任务已清空后立即退出（关闭提示里的「立即退出」）。 */
export async function closeQuitNow(): Promise<void> {
  return invoke<void>("close_quit_now");
}

/** 任务完成后自动退出；应用先留在后台。 */
export async function closeArmQuit(): Promise<void> {
  return invoke<void>("close_arm_quit");
}

export async function closeCancelQuit(): Promise<void> {
  return invoke<void>("close_cancel_quit");
}

/** 退出被任务拦下：payload 是任务列表。 */
export function onCloseBlocked(handler: (tasks: ActiveTask[]) => void): Promise<UnlistenFn> {
  return listen<ActiveTask[]>("app://close-blocked", (event) => handler(event.payload));
}

/** 任务全部结束，可以安全退出。 */
export function onCloseReady(handler: () => void): Promise<UnlistenFn> {
  return listen("app://close-ready", () => handler());
}
