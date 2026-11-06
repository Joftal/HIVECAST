import { forwardRef, useEffect, useImperativeHandle, useRef, useState, type ReactNode } from "react";
import {
  clampRatio,
  computeLayout,
  updateRatio,
  resetRatio,
  type Divider,
  type Rect,
  type SplitNode,
} from "./split";

export type DragPayload = { kind: "cell"; index: number };

/** 拖到某一格的上/左半边表示插到它前面，下/右半边表示插到它后面。 */
export type DropZone = { index: number; side: "before" | "after" };

export type SplitStageHandle = {
  /** 指针拖拽时用坐标反查落在哪一格、落在哪半边。 */
  hitTest: (clientX: number, clientY: number) => DropZone | null;
};

type Props = {
  tree: SplitNode;
  count: number;
  /** 指针拖拽当前悬停的落点，用于画出 Windows 分屏式的半格预览。 */
  drop?: DropZone | null;
  onTreeChange: (tree: SplitNode, commit: boolean) => void;
  renderCell: (index: number, rect: Rect) => ReactNode;
  /** 格子容器的 React key。用房间 id 时，换位只改位置，不重建播放器。 */
  cellKey?: (index: number) => string | number;
  /** 紧凑模式：去掉外圈与缝，分割线只在悬浮/拖动时出现。 */
  compact?: boolean;
};

/**
 * 舞台：按树算出像素矩形，格子绝对定位，分割线浮在缝里。
 * 指针按下后把 pointermove 挂在分割线自己身上（setPointerCapture），拖出边界也不丢。
 */
const SplitStage = forwardRef<SplitStageHandle, Props>(function SplitStage(
  { tree, count, drop, onTreeChange, renderCell, cellKey, compact = false },
  ref,
) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
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

  const { cells, dividers } =
    size.width > 0
      ? computeLayout(tree, size.width, size.height, compact ? { pad: 0, gap: 0, hit: 10 } : undefined)
      : { cells: new Map<number, Rect>(), dividers: [] as Divider[] };
  const cellsRef = useRef(cells);
  cellsRef.current = cells;

  useImperativeHandle(ref, () => ({
    hitTest(clientX, clientY) {
      const host = hostRef.current;
      if (!host) return null;
      const box = host.getBoundingClientRect();
      const x = clientX - box.left;
      const y = clientY - box.top;
      for (const [index, rect] of cellsRef.current) {
        if (x >= rect.left && x <= rect.left + rect.width && y >= rect.top && y <= rect.top + rect.height) {
          const horizontal = rect.width >= rect.height;
          const side = horizontal
            ? x < rect.left + rect.width / 2
              ? "before"
              : "after"
            : y < rect.top + rect.height / 2
              ? "before"
              : "after";
          return { index, side };
        }
      }
      return null;
    },
  }));

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
      {/*
        子节点按 key 排序后固定渲染顺序：换位只改 style 与 index，React 不会移动含 <video> 的 DOM 子树。
        WKWebView 里移动 video 节点会重新加载媒体，那正是「拖一下就重新拉流」的来源。
      */}
      {Array.from({ length: count }, (_, index) => ({ index, key: String(cellKey ? cellKey(index) : index) }))
        .sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0))
        .map(({ index }) => {
          const rect = cells.get(index);
          if (!rect) return null;
          return (
            <div
              key={cellKey ? cellKey(index) : index}
              className="absolute"
              style={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}
            >
              {renderCell(index, rect)}
              {drop?.index === index ? (
                // 落点整格覆盖：换位的结果就是这一格换成拖来的画面，磨砂层要和目标格严丝合缝。
                <div
                  className={[
                    "pointer-events-none absolute inset-0 z-30 border border-white/45 bg-white/12 shadow-[inset_0_0_0_1px_rgba(255,255,255,0.12),0_6px_24px_rgba(0,0,0,0.25)] backdrop-blur-[7px]",
                    compact ? "" : "rounded-[8px]",
                  ].join(" ")}
                />
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
});

export default SplitStage;