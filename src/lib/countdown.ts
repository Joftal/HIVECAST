import { useEffect, useState } from "react";

/**
 * 倒计时：target 是绝对毫秒时刻，这里只负责每 500ms 重算一次剩余秒数。
 * 传 null / undefined 时不启动定时器（例如没有重连计划、二维码已关闭）。
 */
export function useSecondsLeft(target: number | null | undefined): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!target) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [target]);
  if (!target) return null;
  return Math.max(0, Math.ceil((target - now) / 1000));
}

/** 秒数转 m:ss，用于二维码有效期这类 1–3 分钟的倒计时。 */
export function formatCountdown(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(total / 60);
  const rest = total % 60;
  return minutes + ":" + String(rest).padStart(2, "0");
}
