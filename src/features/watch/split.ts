/**
 * 分屏布局：二叉分割树。
 * 叶子只记「显示位置」（0..count-1），房间顺序由 WatchPage 的 order 数组维护，
 * 这样交换两个格子只是换房间，不碰后端的 slot 与转封装进程。
 */
export type Leaf = { kind: "leaf"; index: number };
export type Split = {
  kind: "split";
  /** row = 左右分（切宽度），col = 上下分（切高度）。 */
  dir: "row" | "col";
  ratio: number;
  /** 双击分割线复位用。 */
  base: number;
  a: SplitNode;
  b: SplitNode;
};
export type SplitNode = Leaf | Split;

export type Rect = { left: number; top: number; width: number; height: number };

export type Divider = {
  path: string;
  dir: "row" | "col";
  ratio: number;
  base: number;
  avail: number;
  rect: Rect;
};

export type Variant = "default" | "alt";

export const STAGE_PAD = 10;
export const STAGE_GAP = 10;
export const MIN_CELL_WIDTH = 220;
export const MIN_CELL_HEIGHT = 124;
export const MIN_RATIO = 0.2;
export const MAX_RATIO = 0.8;

const leaf = (index: number): Leaf => ({ kind: "leaf", index });
const split = (dir: Split["dir"], ratio: number, a: SplitNode, b: SplitNode): Split => ({
  kind: "split",
  dir,
  ratio,
  base: ratio,
  a,
  b,
});

/** 默认版式：1 全屏；2 左右；3 上大下二；4 田字。variant 是同一路数下的另一种分法。 */
export function buildTree(count: number, variant: Variant = "default"): SplitNode {
  const n = Math.max(1, Math.min(4, count));
  if (n === 1) return leaf(0);
  if (n === 2) {
    return variant === "alt"
      ? split("col", 0.5, leaf(0), leaf(1))
      : split("row", 0.5, leaf(0), leaf(1));
  }
  if (n === 3) {
    return variant === "alt"
      ? split("row", 0.62, leaf(0), split("col", 0.5, leaf(1), leaf(2)))
      : split("col", 0.62, leaf(0), split("row", 0.5, leaf(1), leaf(2)));
  }
  return split(
    "col",
    0.5,
    split("row", 0.5, leaf(0), leaf(1)),
    split("row", 0.5, leaf(2), leaf(3)),
  );
}

/** 竖屏优先：把画面排成竖条，每条尽量占满高度，避免竖屏被压扁。 */
export function portraitTree(count: number): SplitNode {
  const n = Math.max(1, Math.min(4, count));
  if (n === 1) return leaf(0);
  if (n === 2) return split("row", 0.5, leaf(0), leaf(1));
  if (n === 3) return split("row", 1 / 3, leaf(0), split("row", 0.5, leaf(1), leaf(2)));
  return split(
    "row",
    0.25,
    leaf(0),
    split("row", 1 / 3, leaf(1), split("row", 0.5, leaf(2), leaf(3))),
  );
}

export function leafIndices(node: SplitNode): number[] {
  if (node.kind === "leaf") return [node.index];
  return [...leafIndices(node.a), ...leafIndices(node.b)];
}

export function leafCount(node: SplitNode): number {
  return leafIndices(node).length;
}

/** 按容器尺寸算出每个叶子的像素矩形，以及每条分割线的可拖拽区域。 */
export function computeLayout(tree: SplitNode, width: number, height: number) {
  const cells = new Map<number, Rect>();
  const dividers: Divider[] = [];
  const walk = (node: SplitNode, rect: Rect, path: string) => {
    if (node.kind === "leaf") {
      cells.set(node.index, rect);
      return;
    }
    const avail = Math.max(0, (node.dir === "row" ? rect.width : rect.height) - STAGE_GAP);
    const first = Math.round(avail * node.ratio);
    if (node.dir === "row") {
      walk(node.a, { left: rect.left, top: rect.top, width: first, height: rect.height }, path + "a");
      walk(
        node.b,
        { left: rect.left + first + STAGE_GAP, top: rect.top, width: avail - first, height: rect.height },
        path + "b",
      );
      dividers.push({
        path,
        dir: "row",
        ratio: node.ratio,
        base: node.base,
        avail,
        rect: { left: rect.left + first, top: rect.top, width: STAGE_GAP, height: rect.height },
      });
    } else {
      walk(node.a, { left: rect.left, top: rect.top, width: rect.width, height: first }, path + "a");
      walk(
        node.b,
        { left: rect.left, top: rect.top + first + STAGE_GAP, width: rect.width, height: avail - first },
        path + "b",
      );
      dividers.push({
        path,
        dir: "col",
        ratio: node.ratio,
        base: node.base,
        avail,
        rect: { left: rect.left, top: rect.top + first, width: rect.width, height: STAGE_GAP },
      });
    }
  };
  walk(
    tree,
    {
      left: STAGE_PAD,
      top: STAGE_PAD,
      width: Math.max(0, width - STAGE_PAD * 2),
      height: Math.max(0, height - STAGE_PAD * 2),
    },
    "",
  );
  return { cells, dividers };
}

/** 路径用 a/b 串表示（如 "ab"）：从根往左第一个孩子是 a。 */
export function updateRatio(tree: SplitNode, path: string, ratio: number): SplitNode {
  if (tree.kind === "leaf") return tree;
  if (path.length === 0) return { ...tree, ratio: Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio)) };
  const step = path[0];
  const rest = path.slice(1);
  return step === "a"
    ? { ...tree, a: updateRatio(tree.a, rest, ratio) }
    : { ...tree, b: updateRatio(tree.b, rest, ratio) };
}

export function resetRatio(tree: SplitNode, path: string): SplitNode {
  if (tree.kind === "leaf") return tree;
  if (path.length === 0) return { ...tree, ratio: tree.base };
  const step = path[0];
  const rest = path.slice(1);
  return step === "a"
    ? { ...tree, a: resetRatio(tree.a, rest) }
    : { ...tree, b: resetRatio(tree.b, rest) };
}

/** 拖动时按像素限制比例：既守住 0.2–0.8，也保证子格不小于最小尺寸。 */
export function clampRatio(ratio: number, dir: Split["dir"], avail: number): number {
  const minCell = dir === "row" ? MIN_CELL_WIDTH : MIN_CELL_HEIGHT;
  const min = Math.max(MIN_RATIO, avail > 0 ? minCell / avail : MIN_RATIO);
  const max = Math.min(MAX_RATIO, 1 - min);
  if (!Number.isFinite(ratio)) return 0.5;
  return Math.min(Math.max(ratio, min), Math.max(min, max));
}

export type SavedLayout = { count: number; tree: SplitNode; rooms: (string | null)[] };

/** 结构校验：脏数据（缺字段、dir 不对、children 不是节点）直接拒绝，避免渲染时炸掉。 */
function isTreeShape(node: unknown): node is SplitNode {
  if (!node || typeof node !== "object") return false;
  const value = node as { kind?: unknown; index?: unknown; dir?: unknown; ratio?: unknown; a?: unknown; b?: unknown };
  if (value.kind === "leaf") return typeof value.index === "number";
  if (value.kind !== "split") return false;
  if (value.dir !== "row" && value.dir !== "col") return false;
  if (typeof value.ratio !== "number" || !Number.isFinite(value.ratio)) return false;
  return isTreeShape(value.a) && isTreeShape(value.b);
}

/** 读回持久化的布局；结构不合法就当没存过，绝不因为脏数据崩掉观看页。 */
export function parseLayout(raw: string | null): SavedLayout | null {
  if (!raw) return null;
  try {
    const data = JSON.parse(raw) as Partial<SavedLayout> & { version?: number };
    if (data.version !== 1) return null;
    if (typeof data.count !== "number") return null;
    // 用户把格子全关掉也要记住「空布局」，否则下次回来会复活上一次的房间。
    if (data.count === 0) return { count: 0, tree: buildTree(1), rooms: [] };
    if (!data.tree) return null;
    if (data.count < 1 || data.count > 4) return null;
    if (!isTreeShape(data.tree)) return null;
    const count = data.count;
    const leaves = leafIndices(data.tree);
    if (leaves.length !== count) return null;
    if (new Set(leaves).size !== leaves.length) return null;
    if (leaves.some((index) => index < 0 || index >= count)) return null;
    const rooms = Array.isArray(data.rooms) ? data.rooms.slice(0, count) : [];
    return {
      count,
      tree: sanitizeTree(data.tree),
      rooms: rooms.map((room) => (typeof room === "string" ? room : null)),
    };
  } catch {
    return null;
  }
}

/** 比例夹到合法区间；路径损坏的节点退化成叶子，避免渲染时死循环。 */
function sanitizeTree(node: SplitNode): SplitNode {
  if (node.kind === "leaf") return { kind: "leaf", index: node.index };
  return {
    kind: "split",
    dir: node.dir === "col" ? "col" : "row",
    ratio: Number.isFinite(node.ratio) ? Math.min(MAX_RATIO, Math.max(MIN_RATIO, node.ratio)) : 0.5,
    base: Number.isFinite(node.base) ? Math.min(MAX_RATIO, Math.max(MIN_RATIO, node.base)) : 0.5,
    a: sanitizeTree(node.a),
    b: sanitizeTree(node.b),
  };
}

export function serializeLayout(count: number, tree: SplitNode, rooms: (string | null)[]): string {
  return JSON.stringify({ version: 1, count, tree, rooms: rooms.slice(0, count) });
}
