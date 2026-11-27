import type { RoomView } from "../../lib/ipc";

/**
 * 监控页的增量更新只替换真正变化的房间：
 * - 事件带完整 RoomView，按 room.id 找到旧行替换；
 * - 全量刷新时复用未变化的旧对象，避免行组件因为新对象引用而重渲染。
 */
export function sameRoomView(a: RoomView, b: RoomView): boolean {
  if (a === b) return true;
  return JSON.stringify(a) === JSON.stringify(b);
}

/** 返回新数组；列表里没有这个房间时返回 null，调用方回退到全量刷新。*/
export function patchRoomView(list: RoomView[], next: RoomView): RoomView[] | null {
  const index = list.findIndex((view) => view.room.id === next.room.id);
  if (index < 0) return null;
  if (sameRoomView(list[index], next)) return list;
  const copy = list.slice();
  copy[index] = next;
  return copy;
}

/** 合并一次全量列表：内容相同的房间保留旧对象引用。*/
export function mergeRoomViews(prev: RoomView[], next: RoomView[]): RoomView[] {
  const byId = new Map(prev.map((view) => [view.room.id, view]));
  let changed = prev.length !== next.length;
  const merged = next.map((view, index) => {
    const old = byId.get(view.room.id);
    if (!old) {
      changed = true;
      return view;
    }
    // 内容一致就复用旧对象：行缓存靠引用相等判断这一行没变。
    if (sameRoomView(old, view)) {
      if (prev[index] !== old) changed = true;
      return old;
    }
    changed = true;
    return view;
  });
  if (!changed && prev.every((view, index) => merged[index] === view)) return prev;
  return merged;
}
