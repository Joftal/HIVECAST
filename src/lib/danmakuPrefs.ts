//! 弹幕观感偏好：只影响渲染，不写数据库。
//!
//! 为什么放在前端：这些参数是「这台机器的显示偏好」，换设备本来就应该重新调；
//! 存 localStorage 比塞进 app_setting 表少一次 IPC 往返，也不需要后端重新导出类型。
import { create } from "zustand";
import { persist } from "zustand/middleware";

export type DanmakuPrefs = {
  /** 不透明度 0.3 – 1 */
  opacity: number;
  /** 字号倍率 0.75 – 1.6 */
  fontScale: number;
  /** 滚动速度倍率 0.5 – 2（越大越快） */
  speed: number;
  /** 弹幕占画面高度的比例 0.3 – 1 */
  area: number;
  /** 密度倍率：1 正常 / 1.35 较多 / 1.8 重叠 */
  density: number;
  /** 字号是否随播放器尺寸缩放 */
  scaleWithScreen: boolean;
  /** 高级设置里的字体族；只影响 Canvas 渲染。 */
  fontFamily: DanmakuFontFamily;
  /** 是否使用粗体。 */
  bold: boolean;
  /** 描边样式，对齐 B 站的重墨 / 描边 / 45°投影三档。 */
  outline: DanmakuOutline;
};

export type DanmakuFontFamily = "sans" | "serif" | "kai";
export type DanmakuOutline = "heavy" | "outline" | "shadow45";

export const DEFAULT_PREFS: DanmakuPrefs = {
  opacity: 1,
  fontScale: 1,
  speed: 1,
  area: 1,
  density: 1,
  scaleWithScreen: true,
  fontFamily: "sans",
  bold: true,
  outline: "heavy",
};

function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, value));
}

/** 把外部输入夹到合法区间；面板与持久化恢复都走它，避免手改 localStorage 把画布搞崩。 */
export function sanitizePrefs(input: Partial<DanmakuPrefs>): Partial<DanmakuPrefs> {
  const out: Partial<DanmakuPrefs> = {};
  if (input.opacity !== undefined) out.opacity = clamp(input.opacity, 0.3, 1);
  if (input.fontScale !== undefined) out.fontScale = clamp(input.fontScale, 0.75, 1.6);
  if (input.speed !== undefined) out.speed = clamp(input.speed, 0.5, 2);
  if (input.area !== undefined) out.area = clamp(input.area, 0.3, 1);
  if (input.density !== undefined) out.density = clamp(input.density, 0.75, 2);
  if (input.scaleWithScreen !== undefined) out.scaleWithScreen = Boolean(input.scaleWithScreen);
  if (input.fontFamily !== undefined) {
    out.fontFamily = ["sans", "serif", "kai"].includes(input.fontFamily)
      ? input.fontFamily
      : DEFAULT_PREFS.fontFamily;
  }
  if (input.bold !== undefined) out.bold = Boolean(input.bold);
  if (input.outline !== undefined) {
    out.outline = ["heavy", "outline", "shadow45"].includes(input.outline)
      ? input.outline
      : DEFAULT_PREFS.outline;
  }
  return out;
}

type Store = DanmakuPrefs & {
  set: (patch: Partial<DanmakuPrefs>) => void;
  reset: () => void;
};

export const useDanmakuPrefs = create<Store>()(
  persist(
    (set) => ({
      ...DEFAULT_PREFS,
      set: (patch) => set((state) => ({ ...state, ...sanitizePrefs(patch) })),
      reset: () => set({ ...DEFAULT_PREFS }),
    }),
    { name: "hivecast.danmaku" },
  ),
);
