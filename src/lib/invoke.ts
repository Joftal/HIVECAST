import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { logUiError } from "./uiLog";

// 统一的 IPC 入口：任何命令失败都先记一条界面错误，再把原错误抛给调用方。
// ui_log 自己失败时跳过，避免递归。
export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await tauriInvoke<T>(command, args);
  } catch (err) {
    if (command !== "ui_log") logUiError(err, "ipc:" + command);
    throw err;
  }
}
