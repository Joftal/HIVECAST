import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppInfo } from "./bindings/AppInfo";
import type { CredentialInfo } from "./bindings/CredentialInfo";
import type { QrPoll } from "./bindings/QrPoll";
import type { QrTicket } from "./bindings/QrTicket";
import type { DanmakuBatch } from "./bindings/DanmakuBatch";
import type { DanmakuMessage } from "./bindings/DanmakuMessage";
import type { LineInfo } from "./bindings/LineInfo";
import type { MediaItem } from "./bindings/MediaItem";
import type { Playback } from "./bindings/Playback";
import type { Settings } from "./bindings/Settings";
import type { PostResult } from "./bindings/PostResult";
import type { RecordNotice } from "./bindings/RecordNotice";
import type { RecordSession } from "./bindings/RecordSession";
import type { RoomView } from "./bindings/RoomView";
import type { StreamDescriptor } from "./bindings/StreamDescriptor";
import type { ViewSlotInfo } from "./bindings/ViewSlotInfo";

export type {
  AppInfo,
  CredentialInfo,
  QrPoll,
  QrTicket,
  DanmakuBatch,
  DanmakuMessage,
  LineInfo,
  MediaItem,
  Playback,
  PostResult,
  RecordNotice,
  RecordSession,
  RoomView,
  Settings,
  StreamDescriptor,
  ViewSlotInfo,
};

// 浏览器里跑 Vite 预览时没有 Tauri 运行时，界面上要给出明确提示而不是空白。
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
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

export async function roomsList(): Promise<RoomView[]> {
  return invoke<RoomView[]>("rooms_list");
}

// input 支持房间号、直播间链接、主播主页链接；解析在 Rust 侧完成。
export async function roomAdd(input: string): Promise<RoomView> {
  return invoke<RoomView>("room_add", { input });
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

export async function roomSetQuality(id: string, recordQn: number, viewQn: number): Promise<void> {
  return invoke("room_set_quality", { id, recordQn, viewQn });
}

/** 列出当前可用线路（需要触网解析，展开选择器时才调）。 */
export async function roomLines(id: string, qn?: number): Promise<LineInfo[]> {
  return invoke<LineInfo[]>("room_lines", { id, qn: qn ?? null });
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

export async function sessionsList(roomId?: string, limit = 50): Promise<RecordSession[]> {
  return invoke<RecordSession[]>("sessions_list", { roomId, limit });
}

// ---- M5 本地后处理 ----

export async function sessionRemux(sessionId: string): Promise<PostResult> {
  return invoke<PostResult>("session_remux", { sessionId });
}

export async function sessionScreenshot(sessionId: string, atSecs?: number | null): Promise<PostResult> {
  return invoke<PostResult>("session_screenshot", { sessionId, atSecs: atSecs ?? null });
}

export async function sessionExportDanmaku(sessionId: string): Promise<PostResult> {
  return invoke<PostResult>("session_export_danmaku", { sessionId });
}

export async function roomMergeSessions(roomId: string): Promise<PostResult> {
  return invoke<PostResult>("room_merge_sessions", { roomId });
}

// 后端在任何房间状态变化（开播/下播/开录/停录/出错）时广播，前端只刷新列表。
export const EVENT_RECORD_NOTICE = "record://notice";

// 录制状态播报：前端拿到后自己用 i18n 组装文案再弹系统通知。
export function onRecordNotice(handler: (notice: RecordNotice) => void): Promise<UnlistenFn> {
  return listen<RecordNotice>(EVENT_RECORD_NOTICE, (event) => handler(event.payload));
}

export function onRoomsChanged(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_ROOMS_CHANGED, () => handler());
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

// 只影响这一路的租约，录制进程不会被打断。
export async function viewSetQuality(slot: number, qn: number): Promise<ViewSlotInfo> {
  return invoke<ViewSlotInfo>("view_set_quality", { slot, qn });
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

export async function settingsUpdate(settings: Settings): Promise<Settings> {
  return invoke<Settings>("settings_update", { settings });
}

export async function mediaList(limit?: number): Promise<MediaItem[]> {
  return invoke<MediaItem[]>("media_list", { limit });
}

export async function mediaPlay(sessionId: string): Promise<Playback> {
  return invoke<Playback>("media_play", { sessionId });
}

export async function mediaStop(sessionId: string): Promise<void> {
  return invoke("media_stop", { sessionId });
}

export async function mediaReveal(sessionId: string): Promise<void> {
  return invoke("media_reveal", { sessionId });
}

export async function mediaDelete(sessionId: string, deleteFile: boolean): Promise<void> {
  return invoke("media_delete", { sessionId, deleteFile });
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
