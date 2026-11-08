import type { PointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useMaximized } from "../lib/useMaximized";

type ResizeDirection = "East" | "North" | "NorthEast" | "NorthWest" | "South" | "SouthEast" | "SouthWest" | "West";

/** Windows 无边框窗口的四边/右下角缩放热区。
    tao 在「无边框 + 阴影」下只处理上边缘，其余方向要靠 startResizeDragging 自己补。
    右侧从顶栏下方开始，避免盖住关闭按钮。 */
export default function ResizeHandles() {
  const maximized = useMaximized();
  if (maximized) return null;

  const win = getCurrentWindow();
  const start = (direction: ResizeDirection) => (event: PointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    void win.startResizeDragging(direction).catch(() => undefined);
  };

  const edge = "fixed z-50";
  return (
    <>
      <div className={`${edge} inset-x-0 top-0 h-[5px] cursor-ns-resize`} onPointerDown={start("North")} />
      <div className={`${edge} inset-x-0 bottom-0 h-[5px] cursor-ns-resize`} onPointerDown={start("South")} />
      <div className={`${edge} bottom-0 left-0 top-12 w-[5px] cursor-ew-resize`} onPointerDown={start("West")} />
      <div className={`${edge} bottom-0 right-0 top-12 w-[5px] cursor-ew-resize`} onPointerDown={start("East")} />
      <div className={`${edge} bottom-0 left-0 h-3 w-3 cursor-nesw-resize`} onPointerDown={start("SouthWest")} />
      <div className={`${edge} bottom-0 right-0 h-3 w-3 cursor-nwse-resize`} onPointerDown={start("SouthEast")} />
    </>
  );
}
