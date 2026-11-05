import { useEffect, useRef } from "react";
import { onDanmakuBatch, type DanmakuMessage } from "../../lib/ipc";

type Props = {
  roomId: string;
  enabled: boolean;
};

type Item = {
  text: string;
  color: string;
  weight: number;
  lane: number;
  /** 出生时刻（performance.now） */
  bornAt: number;
  /** 左边缘的起始 x（画布宽） */
  startX: number;
  width: number;
  /** px / ms */
  speed: number;
};

const LANE_HEIGHT = 26;
const LANE_GAP = 18;
const MAX_LANES = 9;
const MAX_ITEMS = 160;
const DURATION = 9000;
const FONT = '600 15px "PingFang SC", "Microsoft YaHei", system-ui, sans-serif';

function colorOf(value: number): string {
  const rgb = (value & 0xffffff).toString(16).padStart(6, "0");
  return "#" + rgb;
}

function labelOf(message: DanmakuMessage): string {
  const content = message.content;
  if (!message.user) return content;
  // 礼物 / SC / 舰长带用户名更有信息量；普通聊天只显示正文，避免刷屏。
  return message.kind === "chat" ? content : message.user + "：" + content;
}

/**
 * Canvas 弹幕层。
 *
 * 为什么不用 DOM：200 条/分钟的直播弹幕在 4 宫格里会产生大量节点，
 * Canvas 只维护一个绘制循环，帧率稳定且不参与 React 渲染。
 */
export default function DanmakuCanvas({ roomId, enabled }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const enabledRef = useRef(enabled);
  enabledRef.current = enabled;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const queue: DanmakuMessage[] = [];
    const items: Item[] = [];
    let width = 0;
    let height = 0;
    let lanes = 0;
    let laneReadyAt: number[] = [];
    let raf = 0;
    let disposed = false;

    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      width = Math.max(1, Math.round(rect.width));
      height = Math.max(1, Math.round(rect.height));
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      lanes = Math.max(1, Math.min(MAX_LANES, Math.floor((height * 0.72) / LANE_HEIGHT)));
      laneReadyAt = new Array(lanes).fill(0);
      // 尺寸变了就把在飞的清掉，避免坐标错位。
      items.length = 0;
    };

    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();

    const spawn = (now: number) => {
      // 一帧最多放 3 条，多了会同时涌出来糊成一片。
      let placed = 0;
      while (queue.length > 0 && placed < 3) {
        const message = queue.shift();
        if (!message) break;
        const text = labelOf(message);
        if (!text.trim()) continue;

        ctx.font = FONT;
        const textWidth = ctx.measureText(text).width;
        if (textWidth > width * 1.35) continue;

        let lane = -1;
        for (let index = 0; index < lanes; index += 1) {
          if (laneReadyAt[index] <= now) {
            lane = index;
            break;
          }
        }
        if (lane < 0) {
          // 轨道全满就丢掉这条：宁可少一条，也不让画面叠字。
          if (queue.length > MAX_ITEMS) queue.length = 0;
          continue;
        }

        const speed = (width + textWidth) / DURATION;
        laneReadyAt[lane] = now + (textWidth + LANE_GAP) / speed;
        items.push({
          text,
          color: colorOf(message.color),
          weight: message.kind === "chat" ? 600 : 700,
          lane,
          bornAt: now,
          startX: width,
          width: textWidth,
          speed,
        });
        placed += 1;
      }
      if (items.length > MAX_ITEMS) items.splice(0, items.length - MAX_ITEMS);
    };

    const draw = (now: number) => {
      raf = requestAnimationFrame(draw);
      if (disposed) return;

      ctx.clearRect(0, 0, width, height);
      if (!enabledRef.current) {
        queue.length = 0;
        items.length = 0;
        return;
      }

      spawn(now);

      for (let index = items.length - 1; index >= 0; index -= 1) {
        const item = items[index];
        const elapsed = now - item.bornAt;
        const x = item.startX - elapsed * item.speed;
        if (x + item.width < -8) {
          items.splice(index, 1);
          continue;
        }
        const y = 8 + item.lane * LANE_HEIGHT;
        ctx.font = FONT;
        ctx.lineWidth = 3;
        ctx.lineJoin = "round";
        ctx.strokeStyle = "rgba(0, 0, 0, 0.78)";
        ctx.strokeText(item.text, x, y + 16);
        ctx.fillStyle = item.color;
        ctx.fillText(item.text, x, y + 16);
      }
    };

    raf = requestAnimationFrame(draw);

    let off: (() => void) | undefined;
    void onDanmakuBatch((batch) => {
      if (batch.roomId !== roomId) return;
      for (const message of batch.messages) queue.push(message);
      if (queue.length > MAX_ITEMS * 2) queue.splice(0, queue.length - MAX_ITEMS * 2);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else off = unlisten;
    });

    return () => {
      disposed = true;
      cancelAnimationFrame(raf);
      observer.disconnect();
      off?.();
      items.length = 0;
      queue.length = 0;
    };
  }, [roomId]);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden
      className={[
        "pointer-events-none absolute inset-0 h-full w-full transition-opacity duration-200",
        enabled ? "opacity-100" : "opacity-0",
      ].join(" ")}
    />
  );
}
