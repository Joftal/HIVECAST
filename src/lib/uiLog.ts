import { invoke } from "@tauri-apps/api/core";

// 只有真正跑在 Tauri 里才上报；浏览器预览没有 ui_log 命令。
function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// 同一条消息 5 秒内只写一次，避免渲染循环把日志刷爆。
const recent = new Map<string, number>();
const DEDUPE_MS = 5000;

export function logUi(level: "error" | "warn" | "info" | "debug", message: string, context?: string) {
  if (!inTauri()) return;
  const key = level + "|" + message + "|" + (context ?? "");
  const now = Date.now();
  const last = recent.get(key);
  if (last !== undefined && now - last < DEDUPE_MS) return;
  recent.set(key, now);
  if (recent.size > 200) {
    for (const [oldKey, at] of recent) {
      if (now - at > DEDUPE_MS) recent.delete(oldKey);
    }
    // 错误风暴里可能全是 5 秒内的新键；再按插入顺序硬裁到 200，避免 Map 无限长。
    while (recent.size > 200) {
      const oldest = recent.keys().next().value;
      if (oldest === undefined) break;
      recent.delete(oldest);
    }
  }
  void invoke("ui_log", {
    level,
    message: message.slice(0, 4096),
    context: (context ?? "").slice(0, 4096),
  }).catch(() => {
    // 日志通道自己失败时不能再递归上报。
  });
}

export function formatUnknown(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.stack || err.message;
  if (err && typeof err === "object" && "message" in err) {
    const message = (err as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

export function logUiError(err: unknown, context?: string) {
  logUi("error", formatUnknown(err), context);
}
