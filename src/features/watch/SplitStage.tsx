import { useEffect, useRef, useState, type DragEvent, type ReactNode } from "react";
import {
  clampRatio,
  computeLayout,
  updateRatio,
  resetRatio,
  type Rect,
  type SplitNode,
} from "./split";

const MIME = "application/x-hivecast";

export type DragPayload =
  | { kind: "cell"; index: number }
  | { kind: "room"; roomId: string };

export function readDrag(event: DragEvent): DragPayload | null {
  const raw = event.dataTransfer.getData(MIME) || event.dataTransfer.getData("text/plain");
  if (!raw) return null;
  try {
    const data = JSON.parse(raw) as DragPayload;
    if (data.kind === "cell" && typeof data.index === "number") return data;
    if (data.kind === "room" && typeof data.roomId === "string") return data;
  } catch {
    return null;
  }
  return null;
}

export function writeDrag(event: DragEvent, payload: DragPayload) {
  event.dataTransfer.setData(MIME, JSON.stringify(payload));
  event.dataTransfer.setData("text/plain", JSON.stringify(payload));
  event.dataTransfer.effectAllowed = "move";
}

/** dragover 阶段浏览器不允许读 dataTransfer 的内容，只能看 types 判断有没有拖东西。 */
function hasDrag(event: DragEvent): boolean {
  const types = Array.from(event.dataTransfer.types);
  return types.includes(MIME) || types.includes("text/plain");
}

type Props = {
  tree: SplitNode;
  count: number;
  onTreeChange: (tree: SplitNode, commit: boolean) => void;
  onSwap: (from: number, to: number) => void;
  onDropRoom: (roomId: string, index: number) => void;
  renderCell: (index: number, rect: Rect) => ReactNode;
};

/**
 * 舞台：按树算出像素矩形，格子绝对定位，分割线浮在缝里。
 * 指针按下后把 pointermove 挂在分割线自己身上（setPointerCapture），拖出边界也不丢。
 */
export default function SplitStage({ tree, count, onTreeChange, onSwap, onDropRoom, renderCell }: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const [over, setOver] = useState<number | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  const treeRef = useRef(tree);
  treeRef.current = tree;
  const dragRef = useRef<{
    path: string;
    dir: "row" | "col";
    start: number;
    ratio: number;
    avail: number;
  } | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const measure = () => setSize({ width: host.clientWidth, height: host.clientHeight });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    return () => observer.disconnect();
  }, []);

  const { cells, dividers } = size.width > 0 ? computeLayout(tree, size.width, size.height) : { cells: new Map(), dividers: [] };

  const startDragDivider = (
    event: React.PointerEvent<HTMLDivElement>,
    path: string,
    dir: "row" | "col",
    ratio: number,
    avail: number,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      path,
      dir,
      start: dir === "row" ? event.clientX : event.clientY,
      ratio,
      avail,
    };
    setDragging(path);
  };

  const moveDivider = (event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const delta = (drag.dir === "row" ? event.clientX : event.clientY) - drag.start;
    const ratio = clampRatio(drag.ratio + (drag.avail > 0 ? delta / drag.avail : 0), drag.dir, drag.avail);
    onTreeChange(updateRatio(treeRef.current, drag.path, ratio), false);
  };

  const endDivider = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    setDragging(null);
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    onTreeChange(treeRef.current, true);
  };

  return (
    <div ref={hostRef} className="relative h-full w-full overflow-hidden">
      {Array.from({ length: count }).map((_, index) => {
        const rect = cells.get(index);
        if (!rect) return null;
        return (
          <div
            key={index}
            className="absolute"
            style={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}
            onDragOver={(event) => {
              if (!hasDrag(event)) return;
              event.preventDefault();
              event.dataTransfer.dropEffect = "move";
              setOver(index);
            }}
            onDragLeave={() => setOver((value) => (value === index ? null : value))}
            onDrop={(event) => {
              event.preventDefault();
              setOver(null);
              const payload = readDrag(event);
              if (!payload) return;
              if (payload.kind === "cell") {
                if (payload.index !== index) onSwap(payload.index, index);
              } else {
                onDropRoom(payload.roomId, index);
              }
            }}
          >
            {renderCell(index, rect)}
            {over === index ? (
              <div className="pointer-events-none absolute inset-0 z-30 rounded-[8px] ring-2 ring-accent ring-inset" />
            ) : null}
          </div>
        );
      })}

      {dividers.map((divider) => (
        <div
          key={divider.path}
          role="separator"
          aria-orientation={divider.dir === "row" ? "vertical" : "horizontal"}
          onPointerDown={(event) =>
            startDragDivider(event, divider.path, divider.dir, divider.ratio, divider.avail)
          }
          onPointerMove={moveDivider}
          onPointerUp={endDivider}
          onPointerCancel={endDivider}
          onDoubleClick={() => onTreeChange(resetRatio(treeRef.current, divider.path), true)}
          className={[
            "group absolute z-20 flex items-center justify-center",
            divider.dir === "row" ? "cursor-col-resize" : "cursor-row-resize",
          ].join(" ")}
          style={{
            left: divider.rect.left,
            top: divider.rect.top,
            width: divider.rect.width,
            height: divider.rect.height,
          }}
        >
          <span
            className={[
              "rounded-full transition-colors",
              divider.dir === "row" ? "h-8 w-[3px]" : "h-[3px] w-8",
              dragging === divider.path ? "bg-accent" : "bg-white/0 group-hover:bg-accent/80",
            ].join(" ")}
          />
        </div>
      ))}
    </div>
  );
}
