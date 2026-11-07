import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { useTranslation } from "react-i18next";
import { createPortal } from "react-dom";
import DanmakuCanvas from "./DanmakuCanvas";
import { useDanmakuPrefs } from "../../lib/danmakuPrefs";
import { useSecondsLeft } from "../../lib/countdown";
import Player from "./Player";
import SplitStage, { type DragPayload, type DropZone, type SplitStageHandle } from "./SplitStage";
import {
  buildTree,
  leafCount,
  parseLayout,
  portraitTree,
  serializeLayout,
  type SplitNode,
  type Variant,
} from "./split";
import { Button, cx } from "../../components/ui/Controls";
import { LayoutGlyph, type LayoutKind } from "../../components/ui/LayoutGlyph";
import {
  describeError,
  isTauri,
  layoutGet,
  layoutSet,
  onRoomsChanged,
  onViewChanged,
  roomAdd,
  roomLines,
  roomsList,
  viewClose,
  viewOpen,
  viewReplace,
  viewRetry,
  viewSetLine,
  viewSetQuality,
  viewSlots,
  type LineInfo,
  type RoomView,
  type ViewSlotInfo,
} from "../../lib/ipc";
import { whenViewsIdle } from "../../lib/viewLifecycle";
import { useWatchPrefs } from "../../lib/watchPrefs";
import { getCurrentWindow } from "@tauri-apps/api/window";

const QN_OPTIONS = [0, 40000, 10000, 400, 250, 150, 80] as const;
const MAX_SLOTS = 4;

export type WatchSeed = { ids: string[]; nonce: number; kind?: LayoutKind };

type Props = {
  /** 从监控页多选进入时带过来的房间；nonce 变化才会重新应用。 */
  seed?: WatchSeed | null;
  /** 应用完立刻通知上层清空 seed，避免切回本页时又覆盖一次。 */
  onSeedApplied?: () => void;
  /** 全屏时上层要收起顶栏；退出全屏再放回来。 */
  onImmersiveChange?: (immersive: boolean) => void;
};

export default function WatchPage({ seed, onSeedApplied, onImmersiveChange }: Props) {
  const { t } = useTranslation();
  const [rooms, setRooms] = useState<RoomView[]>([]);
  const [slots, setSlots] = useState<ViewSlotInfo[]>([]);
  const [volumes, setVolumes] = useState<Record<number, number>>({});
  const [loudHint, setLoudHint] = useState(true);
  const [danmaku, setDanmaku] = useState<Record<number, boolean>>({});
  const [danmakuOpen, setDanmakuOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 紧凑模式与全屏都只影响这一页；紧凑持久化，全屏跟随窗口状态。
  const { compact, setCompact } = useWatchPrefs();
  const [fullscreen, setFullscreen] = useState(false);
  const fullscreenRef = useRef(false);
  // 全屏时顶栏与候选竖条自动收起，指针进入上/右边缘才滑出；240ms 延迟避免穿出时闪一下。
  const [hoverTop, setHoverTop] = useState(false);
  const [hoverRail, setHoverRail] = useState(false);
  const hoverTimers = useRef<{ top?: number; rail?: number }>({});
  fullscreenRef.current = fullscreen;
  const immersiveRef = useRef(onImmersiveChange);
  immersiveRef.current = onImmersiveChange;

  // 分屏：树管版式与比例，order 管「第几格放哪个房间」。order 与后端 slot 解耦。
  const [tree, setTree] = useState<SplitNode>(() => buildTree(1));
  const [variant, setVariant] = useState<Variant>("default");
  const [portrait, setPortrait] = useState(false);
  const [order, setOrder] = useState<(string | null)[]>([]);


  const [panelOpen, setPanelOpen] = useState(false);
  // 首帧还不知道上次的布局，先别把空布局写回去覆盖它。
  const [booted, setBooted] = useState(false);
  const [roomsLoaded, setRoomsLoaded] = useState(false);
  // 指针拖拽：不依赖 HTML5 DnD，WKWebView 里更可靠；拖拽中显示跟随指针的浮标。
  const [drag, setDrag] = useState<{ payload: DragPayload; x: number; y: number } | null>(null);
  const [drop, setDrop] = useState<DropZone | null>(null);
  const stageRef = useRef<SplitStageHandle | null>(null);
  const dragRef = useRef<{ payload: DragPayload; x: number; y: number; moved: boolean } | null>(null);
  const dragCleanupRef = useRef<(() => void) | null>(null);
  const dropRef = useRef<DropZone | null>(null);
  // 悬停候选时浮出的「放到哪一格」选择器。
  const [picker, setPicker] = useState<{ roomId: string; x: number; y: number } | null>(null);
  const pickerTimer = useRef<number | undefined>(undefined);
  const [query, setQuery] = useState("");
  const [input, setInput] = useState("");

  const seedRef = useRef(0);
  const latestLayout = useRef("");
  // 存下来的布局解析不出来时不要再写回去，否则一次脏数据就把用户布局抹成空。
  const persistOk = useRef(true);

  const load = useCallback(async () => {
    try {
      const [nextRooms, nextSlots] = await Promise.all([roomsList(), viewSlots()]);
      setRooms(nextRooms);
      setSlots(nextSlots);
      setRoomsLoaded(true);
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let offView: (() => void) | undefined;
    let offRooms: (() => void) | undefined;

    void (async () => {
      // 先等上一次「离开观看页」的挂起跑完，避免读到还没清掉的旧槽位。
      await whenViewsIdle();
      // seed 由下面的 effect 应用，这里别抢着恢复旧布局。
      const seeded = Boolean(seed && seed.nonce !== seedRef.current);
      let canPersist = true;
      try {
        const [raw, nextRooms, nextSlots] = await Promise.all([layoutGet(), roomsList(), viewSlots()]);
        if (disposed) return;
        setRooms(nextRooms);
        setSlots(nextSlots);
        setRoomsLoaded(true);
        const savedLayout = parseLayout(raw);
        if (raw && !savedLayout) canPersist = false;
        if (seeded) {
          // 等 applySeed 把 order/tree 一次设好。
        } else if (nextSlots.length > 0) {
          // 已有会话（例如开发时热更新）：以会话为准，只套用同路数的版式。
          const ids = [...nextSlots].sort((a, b) => a.slot - b.slot).map((session) => session.roomId);
          if (savedLayout && savedLayout.count === ids.length) setTree(savedLayout.tree);
          setOrder(ids);
        } else if (savedLayout && savedLayout.rooms.some(Boolean)) {
          // 上次离开时的布局：先摆好格子（离线房间保留占位），再逐路拉起。
          setOrder(savedLayout.rooms);
          setTree(savedLayout.tree);
          await restore(savedLayout.rooms);
        }
      } catch (err) {
        if (!disposed) setError(describeError(err));
      }
      if (disposed) return;
      persistOk.current = canPersist;
      setBooted(true);
      offView = await onViewChanged((next) => setSlots(next));
      offRooms = await onRoomsChanged(() => {
        void roomsList().then(setRooms).catch(() => undefined);
      });
    })();

    return () => {
      disposed = true;
      offView?.();
      offRooms?.();
    };
    // 只在挂载时跑一次；seed 变化由下面的 effect 处理
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const sessions = useMemo(() => [...slots].sort((a, b) => a.slot - b.slot), [slots]);

  const watchedIds = useMemo(() => new Set(sessions.map((session) => session.roomId)), [sessions]);
  const roomById = useMemo(() => {
    const map = new Map<string, RoomView>();
    for (const view of rooms) map.set(view.room.id, view);
    return map;
  }, [rooms]);

  // 版面格数由 order 决定，不再跟着后端会话数跳：替换房间时 count 不会短暂掉一格，
  // 也就不会触发「版式被重置」的莫名状态。
  const count = order.length;

  // 版式树的叶子数必须与格数一致；只在真正增删格子时重建。
  useEffect(() => {
    if (count === 0) return;
    if (leafCount(tree) !== count) setTree(buildTree(count));
  }, [count, tree]);

  useEffect(() => {
    if (!isTauri() || !booted || !persistOk.current) return;
    const payload = serializeLayout(count, tree, order);
    latestLayout.current = payload;
    const timer = window.setTimeout(() => {
      void layoutSet(payload).catch(() => undefined);
    }, 500);
    return () => window.clearTimeout(timer);
  }, [tree, order, count, booted]);

  // 房间被移除后，格子里的 id 也要跟着消失，避免留下点不掉的占位。
  useEffect(() => {
    if (!booted || !roomsLoaded) return;
    const alive = new Set(rooms.map((view) => view.room.id));
    setOrder((prev) => {
      if (!prev.some((id) => id && !alive.has(id))) return prev;
      return prev.filter((id) => id && alive.has(id));
    });
  }, [rooms, booted, roomsLoaded]);

  // 切 Tab 会卸载本页，防抖还没到点就先把布局落库。
  useEffect(() => {
    return () => {
      if (isTauri() && persistOk.current && latestLayout.current) {
        void layoutSet(latestLayout.current).catch(() => undefined);
      }
    };
  }, []);

  useEffect(() => {
    if (!seed || seed.nonce === seedRef.current) return;
    seedRef.current = seed.nonce;
    void applySeed(seed.ids, seed.kind).finally(() => onSeedApplied?.());
    // applySeed 用最新 tabs，不需要进依赖
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seed]);

  const sounding = sessions.filter((session) => (volumes[session.slot] ?? 0) > 0).length;
  useEffect(() => {
    if (sounding < 3) setLoudHint(true);
  }, [sounding]);

  const setVolume = (slot: number, volume: number) => setVolumes((prev) => ({ ...prev, [slot]: volume }));
  const toggleDanmaku = (slot: number) => setDanmaku((prev) => ({ ...prev, [slot]: !(prev[slot] ?? true) }));
  const muteAll = () => {
    const next: Record<number, number> = {};
    for (const session of sessions) next[session.slot] = 0;
    setVolumes(next);
  };

  /** 版式 kind → 分割树；路数对不上时退回该路数的默认版式。 */
  function treeFor(kind: LayoutKind | undefined, n: number) {
    const candidate =
      kind === "single"
        ? buildTree(1)
        : kind === "row"
          ? buildTree(2)
          : kind === "col"
            ? buildTree(2, "alt")
            : kind === "top"
              ? buildTree(3)
              : kind === "left"
                ? buildTree(3, "alt")
                : kind === "quad"
                  ? buildTree(4)
                  : kind === "portrait"
                    ? portraitTree(n)
                    : buildTree(n);
    return {
      tree: leafCount(candidate) === n ? candidate : buildTree(n),
      variant: (kind === "col" || kind === "left" ? "alt" : "default") as Variant,
      portrait: kind === "portrait",
    };
  }

  /** 从监控页多选进入：先摆好格子与版式，再按差异复用/新开槽位，不整页重建。 */
  async function applySeed(ids: string[], kind?: LayoutKind) {
    const wanted = ids.slice(0, MAX_SLOTS);
    const layout = treeFor(kind, wanted.length);
    setOrder(wanted);
    setTree(layout.tree);
    setVariant(layout.variant);
    setPortrait(layout.portrait);
    setBusy(true);
    setError(null);
    try {
      await whenViewsIdle();
      const current = await viewSlots();
      const wantedSet = new Set(wanted);
      await Promise.all(
        current
          .filter((session) => !wantedSet.has(session.roomId))
          .map((session) => viewClose(session.slot).catch(() => undefined)),
      );
      const opened = new Set(current.map((session) => session.roomId));
      await Promise.all(
        wanted.map(async (id) => {
          if (opened.has(id)) return;
          try {
            await viewOpen(id);
          } catch {
            // 单路失败保留占位，不影响其它格
          }
        }),
      );
      setPanelOpen(false);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  /** 回到观看页：按上次布局逐路恢复；离线/失败的路保留占位，不改变格子数量。 */
  async function restore(roomsToOpen: (string | null)[]) {
    const ids = roomsToOpen.filter((id): id is string => Boolean(id));
    await Promise.all(
      ids.map(async (id) => {
        try {
          await viewOpen(id);
        } catch {
          // 保留占位
        }
      }),
    );
    await load();
  }

  async function watchRoom(roomId: string, replaceAt?: number) {
    // 选完候选后放置器要立刻收掉：候选行随即从列表里消失，pointerleave 不会再来。
    window.clearTimeout(pickerTimer.current);
    setPicker(null);
    if (replaceAt !== undefined) {
      const previous = order[replaceAt];
      if (previous === roomId) return;
      // 目标房间已经在别的格子里：只交换两个格子的位置，后端一根管道都不用动。
      const existing = order.indexOf(roomId);
      if (existing >= 0) {
        setOrder((prev) => {
          const next = [...prev];
          [next[replaceAt], next[existing]] = [next[existing], next[replaceAt]];
          return next;
        });
        return;
      }
    } else {
      if (order.includes(roomId)) return;
      if (count >= MAX_SLOTS) {
        setError(t("watch.slotsFull"));
        return;
      }
    }

    setBusy(true);
    setError(null);
    try {
      if (replaceAt !== undefined) {
        const previous = order[replaceAt];
        const session = previous ? sessions.find((item) => item.roomId === previous) : undefined;
        // 先换格子里的房间，再让后端原子替换：事件回来时顺序已经就位，不会闪一下占位。
        setOrder((prev) => prev.map((id, index) => (index === replaceAt ? roomId : id)));
        try {
          if (session) await viewReplace(session.slot, roomId);
          else await viewOpen(roomId);
        } catch (err) {
          setOrder((prev) => prev.map((id, index) => (index === replaceAt ? previous ?? id : id)));
          throw err;
        }
      } else {
        await viewOpen(roomId);
        const next = order.includes(roomId) ? order : [...order, roomId];
        setOrder(next);
        // 同一帧把树补齐，避免多出一格空白再被 effect 补上。
        if (leafCount(tree) !== next.length) setTree(buildTree(next.length));
      }
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function addAndWatch() {
    const value = input.trim();
    if (!value) return;
    setBusy(true);
    setError(null);
    try {
      const view = await roomAdd(value);
      if (!order.includes(view.room.id)) {
        if (count >= MAX_SLOTS) {
          setError(t("watch.slotsFull"));
          return;
        }
        await viewOpen(view.room.id);
        const next = order.includes(view.room.id) ? order : [...order, view.room.id];
        setOrder(next);
        if (leafCount(tree) !== next.length) setTree(buildTree(next.length));
      }
      setInput("");
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function closeSlot(slot: number) {
    const roomId = sessions.find((session) => session.slot === slot)?.roomId;
    if (!roomId) {
      await viewClose(slot).catch((err) => setError(describeError(err)));
      await load();
      return;
    }
    // 同样先摘格子：关流事件回来时格子已经收掉，不会出现空占位。
    const next = order.filter((id) => id !== roomId);
    setOrder(next);
    if (next.length === 0) setTree(buildTree(1));
    else if (leafCount(tree) !== next.length) setTree(buildTree(next.length));
    try {
      await viewClose(slot);
      await load();
    } catch (err) {
      setOrder(order);
      setError(describeError(err));
    }
  }

  async function changeQuality(slot: number, qn: number) {
    try {
      await viewSetQuality(slot, qn);
    } catch (err) {
      setError(describeError(err));
    }
  }

  async function changeLine(slot: number, line: string | null) {
    try {
      await viewSetLine(slot, line);
    } catch (err) {
      setError(describeError(err));
    }
  }

  async function retrySlot(slot: number) {
    try {
      await viewRetry(slot);
    } catch (err) {
      setError(describeError(err));
    }
  }

  /** 占位格重试：房间已开播时把这一格拉起来，不改动版式与其它格。 */
  async function openPlaceholder(roomId: string) {
    setBusy(true);
    setError(null);
    try {
      await viewOpen(roomId);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  const swapCells = (from: number, to: number) => {
    setOrder((prev) => {
      const next = [...prev];
      const a = next[from];
      next[from] = next[to];
      next[to] = a;
      return next;
    });
  };

  const applyLayout = (kind: "default" | "alt" | "portrait") => {
    if (kind === "portrait") {
      setPortrait(true);
      setVariant("default");
      setTree(portraitTree(count));
      return;
    }
    setPortrait(false);
    setVariant(kind);
    setTree(buildTree(count, kind));
  };

  /** 拖到哪一格就和哪一格交换：语义固定为「换房间」，与位置前后无关，任何落点都会变化。 */
  const swapRef = useRef(swapCells);
  swapRef.current = swapCells;

  // 监听器在 pointerdown 里同步挂上，不依赖下一次 render，快速拖拽也不会丢事件。
  const startDrag = (payload: DragPayload, event: ReactPointerEvent) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    // 上一次拖拽若没收尾（指针在窗口外松开），先清掉旧监听，避免下一次拖拽被它吞掉。
    dragCleanupRef.current?.();
    // 把指针捕获在抓手上：即使中途重排/重渲染，pointermove/up 也不会丢。
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // 合成事件没有真实 pointerId，捕获会抛错；window 上的监听已经足够收尾。
    }
    dragRef.current = { payload, x: event.clientX, y: event.clientY, moved: false };
    dropRef.current = null;
    setDrag({ payload, x: event.clientX, y: event.clientY });
    setDrop(null);

    const move = (moveEvent: PointerEvent) => {
      const current = dragRef.current;
      if (!current) return;
      if (Math.abs(moveEvent.clientX - current.x) + Math.abs(moveEvent.clientY - current.y) > 4) {
        current.moved = true;
      }
      current.x = moveEvent.clientX;
      current.y = moveEvent.clientY;
      setDrag({ payload: current.payload, x: moveEvent.clientX, y: moveEvent.clientY });
      const hit = stageRef.current?.hitTest(moveEvent.clientX, moveEvent.clientY) ?? null;
      // 拖回自己身上不画落点，避免「看起来能放，松手却没变化」。
      const zone = hit && hit.index !== current.payload.index ? hit : null;
      dropRef.current = zone;
      setDrop(zone);
    };
    const cleanup = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      dragCleanupRef.current = null;
    };
    const finish = () => {
      cleanup();
      const current = dragRef.current;
      dragRef.current = null;
      const zone = dropRef.current;
      dropRef.current = null;
      if (current && current.moved && zone && zone.index !== current.payload.index) {
        swapRef.current(current.payload.index, zone.index);
      }
      setDrag(null);
      setDrop(null);
    };
    dragCleanupRef.current = cleanup;
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
  };

  useEffect(() => {
    if (panelOpen) return;
    window.clearTimeout(pickerTimer.current);
    setPicker(null);
  }, [panelOpen]);

  const hoverChrome = useCallback((area: "top" | "rail", on: boolean) => {
    const timer = hoverTimers.current[area];
    if (timer) window.clearTimeout(timer);
    const set = area === "top" ? setHoverTop : setHoverRail;
    if (on) {
      set(true);
      return;
    }
    hoverTimers.current[area] = window.setTimeout(() => set(false), 240);
  }, []);

  const applyFullscreen = useCallback((next: boolean) => {
    setFullscreen(next);
    if (!next) {
      setHoverTop(false);
      setHoverRail(false);
    }
    immersiveRef.current?.(next);
  }, []);

  const toggleFullscreen = useCallback(async () => {
    if (!isTauri()) return;
    const next = !fullscreenRef.current;
    try {
      await getCurrentWindow().setFullscreen(next);
      applyFullscreen(next);
    } catch (err) {
      setError(describeError(err));
    }
  }, [applyFullscreen]);

  // 系统全屏（绿灯按钮、Cmd+Ctrl+F）也会改窗口状态，用 resize 事件把本地状态对齐。
  useEffect(() => {
    if (!isTauri()) return;
    const win = getCurrentWindow();
    let alive = true;
    const sync = () => {
      void win
        .isFullscreen()
        .then((value) => {
          if (alive && value !== fullscreenRef.current) applyFullscreen(value);
        })
        .catch(() => undefined);
    };
    sync();
    const unlisten = win.onResized(sync);
    return () => {
      alive = false;
      void unlisten.then((off) => off()).catch(() => undefined);
    };
  }, [applyFullscreen]);

  // 离开观看页时把窗口还原，避免顶栏被隐藏着退出。
  useEffect(() => {
    return () => {
      if (!fullscreenRef.current) return;
      if (isTauri()) void getCurrentWindow().setFullscreen(false).catch(() => undefined);
      immersiveRef.current?.(false);
    };
  }, []);

  // Esc 退出、F11 切换；输入框里不抢键。
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
      if (event.key === "Escape" && fullscreenRef.current) {
        event.preventDefault();
        void toggleFullscreen();
        return;
      }
      if (event.key === "F11") {
        event.preventDefault();
        void toggleFullscreen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggleFullscreen]);

  const candidates = rooms.filter((view) => view.room.isLive && !watchedIds.has(view.room.id));

  const filtered = candidates.filter((view) => {
    const needle = query.trim().toLowerCase();
    if (!needle) return true;
    return (
      (view.room.title ?? "").toLowerCase().includes(needle) ||
      (view.room.hostName ?? "").toLowerCase().includes(needle)
    );
  });
  const prefs = useDanmakuPrefs();

  const layoutOptions: Array<{ kind: "default" | "alt" | "portrait"; label: string; glyph: LayoutKind }> =
    count === 4
      ? [
          { kind: "default", label: t("watch.layout.quad"), glyph: "quad" },
          { kind: "portrait", label: t("watch.layout.portrait"), glyph: "portrait" },
        ]
      : count === 3
        ? [
            { kind: "default", label: t("watch.layout.threeBig"), glyph: "top" },
            { kind: "alt", label: t("watch.layout.threeCol"), glyph: "left" },
            { kind: "portrait", label: t("watch.layout.portrait"), glyph: "portrait" },
          ]
        : [
            { kind: "default", label: t("watch.layout.twoH"), glyph: "row" },
            { kind: "alt", label: t("watch.layout.twoV"), glyph: "col" },
          ];

  return (
    <div className="relative flex h-full min-h-0 bg-canvas text-ink">
      <div className="relative flex min-w-0 flex-1 flex-col">
        <div
          onPointerEnter={() => hoverChrome("top", true)}
          onPointerLeave={() => hoverChrome("top", false)}
          className={cx(
            "flex h-14 shrink-0 items-center gap-2 border-b border-line px-5",
            fullscreen &&
              "absolute inset-x-0 top-0 z-30 bg-surface/92 backdrop-blur-md transition-[transform,opacity] duration-200",
            fullscreen && !hoverTop && "-translate-y-full opacity-0",
          )}
        >
          <span className="font-mono text-[11px] tabular-nums text-muted">
            {count} <span className="text-faint">/ 4</span>
          </span>

          {count >= 2 ? (
            <div className="flex items-center rounded-[6px] border border-line p-0.5">
              {layoutOptions.map((option) => {
                const active = option.kind === "portrait" ? portrait : !portrait && variant === option.kind;
                return (
                  <button
                    key={option.kind}
                    type="button"
                    onClick={() => applyLayout(option.kind)}
                    className={[
                      "focus-ring inline-flex items-center gap-1.5 rounded-[4px] px-2 py-1 text-[11px] transition-colors",
                      active ? "bg-raised text-ink" : "text-muted hover:text-ink-soft",
                    ].join(" ")}
                  >
                    <LayoutGlyph kind={option.glyph} />
                    {option.label}
                  </button>
                );
              })}
            </div>
          ) : null}

          <div className="ml-auto flex items-center gap-1">
            <Button
              size="sm"
              variant={compact ? "secondary" : "ghost"}
              title={t("watch.compactHint")}
              onClick={() => setCompact(!compact)}
            >
              <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                {compact ? (
                  <>
                    <rect x="2.2" y="2.2" width="5" height="5" />
                    <rect x="8.8" y="2.2" width="5" height="5" />
                    <rect x="2.2" y="8.8" width="5" height="5" />
                    <rect x="8.8" y="8.8" width="5" height="5" />
                  </>
                ) : (
                  <>
                    <rect x="1.6" y="1.6" width="12.8" height="12.8" rx="1.4" />
                    <path d="M8 1.6v12.8M1.6 8h12.8" />
                  </>
                )}
              </svg>
              {t("watch.compact")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              title={t("watch.fullscreenHint")}
              onClick={() => void toggleFullscreen()}
            >
              <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                {fullscreen ? (
                  <path d="M6 2v4H2M10 2v4h4M6 14v-4H2M10 14v-4h4" strokeLinecap="round" strokeLinejoin="round" />
                ) : (
                  <path d="M2 6V2h4M14 6V2h-4M2 10v4h4M14 10v4h-4" strokeLinecap="round" strokeLinejoin="round" />
                )}
              </svg>
              {fullscreen ? t("watch.exitFullscreen") : t("watch.fullscreen")}
            </Button>
            {count > 0 ? (
              <Button size="sm" variant="ghost" onClick={() => setDanmakuOpen((open) => !open)}>
                {t("watch.danmakuSettings")}
              </Button>
            ) : null}
            {sounding > 0 ? (
              <Button size="sm" variant="ghost" onClick={muteAll}>
                {t("watch.muteAll")}
              </Button>
            ) : null}
          </div>
        </div>

        {error ? (
          <div className="mx-3 mt-2 flex shrink-0 items-center gap-2 rounded-[6px] border border-rec/35 bg-rec/12 px-2.5 py-1.5 text-[11px] text-rec">
            <span className="flex-1 break-all">{error}</span>
            <button type="button" onClick={() => setError(null)} className="text-rec/70 hover:text-rec">
              ✕
            </button>
          </div>
        ) : null}

        {sounding >= 3 && loudHint ? (
          <div className="mx-3 mt-2 flex shrink-0 items-center gap-2 rounded-[6px] border border-line bg-raised px-2.5 py-1.5 text-[11px] text-ink-soft">
            <span className="flex-1">{t("watch.multiSound")}</span>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                muteAll();
                setLoudHint(false);
              }}
            >
              {t("watch.muteAll")}
            </Button>
            <button
              type="button"
              aria-label={t("watch.dismissHint")}
              onClick={() => setLoudHint(false)}
              className="text-faint hover:text-ink-soft"
            >
              ✕
            </button>
          </div>
        ) : null}

        {danmakuOpen && count > 0 ? (
          <div className="mx-3 mt-2 grid shrink-0 gap-3 rounded-[8px] border border-line bg-surface p-3 sm:grid-cols-2 lg:grid-cols-4">
            {(
              [
                ["opacity", t("watch.danmakuOpacity"), 0.3, 1, 0.05, 2],
                ["fontScale", t("watch.danmakuFont"), 0.75, 1.6, 0.05, 2],
                ["speed", t("watch.danmakuSpeed"), 0.5, 2, 0.1, 1],
                ["area", t("watch.danmakuArea"), 0.3, 1, 0.05, 2],
              ] as const
            ).map(([key, label, min, max, step, digits]) => (
              <label key={key} className="flex flex-col gap-1">
                <span className="flex items-baseline justify-between text-[11px] text-muted">
                  {label}
                  <span className="font-mono text-[10px] tabular-nums text-faint">
                    {(prefs[key] as number).toFixed(digits)}
                  </span>
                </span>
                <input
                  type="range"
                  min={min}
                  max={max}
                  step={step}
                  value={prefs[key] as number}
                  onChange={(event) => prefs.set({ [key]: Number(event.target.value) })}
                  className="accent-accent"
                />
              </label>
            ))}
            <Button size="sm" variant="ghost" className="self-end justify-self-start" onClick={() => prefs.reset()}>
              {t("watch.danmakuReset")}
            </Button>
          </div>
        ) : null}

        <div className="relative min-h-0 flex-1">
          {count === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-4 px-6 text-center">
              <div className="grid grid-cols-2 gap-1.5 opacity-50">
                {[0, 1, 2, 3].map((index) => (
                  <span key={index} className="h-6 w-10 rounded-[4px] border border-line-strong bg-raised" />
                ))}
              </div>
              <div className="space-y-1">
                <p className="text-[13px] font-medium text-ink">{t("watch.emptyTitle")}</p>
                <p className="max-w-[380px] text-[11.5px] leading-5 text-muted">{t("watch.emptyBody")}</p>
              </div>
              {isTauri() ? (
                <Button variant="primary" onClick={() => setPanelOpen(true)}>
                  {t("watch.addRoom")}
                </Button>
              ) : null}
            </div>
          ) : (
            <SplitStage
              ref={stageRef}
              tree={tree}
              count={count}
              drop={drop}
              compact={compact}
              onTreeChange={(next) => setTree(next)}
              cellKey={(index) => order[index] ?? `empty-${index}`}
              renderCell={(index) => {
                const roomId = order[index] ?? null;
                const session = roomId ? sessions.find((item) => item.roomId === roomId) : undefined;
                const view = roomId ? roomById.get(roomId) : undefined;
                if (!session) {
                  const room = view?.room;
                  return (
                    <div className="flex h-full flex-col items-center justify-center gap-1.5 rounded-[8px] border border-dashed border-line bg-surface/60 px-4 text-center">
                      <span className="max-w-full truncate text-[12px] font-medium text-ink-soft">
                        {room ? room.hostName || room.title || room.roomKey : t("watch.stage.empty")}
                      </span>
                      {room ? (
                        <span className="text-[10.5px] text-faint">
                          {room.isLive ? t("watch.stage.starting") : t("watch.stage.offline")}
                        </span>
                      ) : null}
                      {roomId ? (
                        <div className="mt-1 flex items-center gap-1.5">
                          {room ? (
                            <Button size="sm" variant="secondary" onClick={() => void openPlaceholder(room.id)}>
                              {t("watch.retry")}
                            </Button>
                          ) : null}
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => setOrder((prev) => prev.filter((id) => id !== roomId))}
                          >
                            {t("watch.remove")}
                          </Button>
                        </div>
                      ) : null}
                    </div>
                  );
                }
                return (
                  <SlotCell
                    key={session.roomId}
                    session={session}
                    room={view?.room}
                    index={index}
                    count={count}
                    compact={compact}
                    volume={volumes[session.slot] ?? 0}
                    danmaku={danmaku[session.slot] ?? true}
                    onVolume={(value) => setVolume(session.slot, value)}
                    onToggleDanmaku={() => toggleDanmaku(session.slot)}
                    onQuality={(qn) => void changeQuality(session.slot, qn)}
                    onLine={(line) => void changeLine(session.slot, line)}
                    onDanmakuSettings={() => setDanmakuOpen(true)}
                    onClose={() => void closeSlot(session.slot)}
                    onRetry={() => void retrySlot(session.slot)}
                    onSwapNext={() => swapCells(index, (index + 1) % count)}
                    onDragStart={(event) => startDrag({ kind: "cell", index }, event)}
                  />
                );
              }}
            />
          )}

          {panelOpen ? (
            <div className="absolute inset-y-0 right-0 z-30 flex w-[300px] flex-col border-l border-line bg-surface/95 shadow-2xl backdrop-blur-md">
              <div className="flex h-11 shrink-0 items-center gap-2 border-b border-line px-3">
                <span className="text-[12px] font-medium text-ink">{t("watch.panel.title")}</span>
                <span className="font-mono text-[10px] tabular-nums text-faint">
                  {count} / 4
                </span>
                <button
                  type="button"
                  onClick={() => setPanelOpen(false)}
                  aria-label={t("watch.close")}
                  className="focus-ring ml-auto flex h-6 w-6 items-center justify-center rounded-[5px] text-faint hover:bg-raised hover:text-ink-soft"
                >
                  ✕
                </button>
              </div>

              <div className="shrink-0 px-2 pt-2">
                <input
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder={t("watch.panel.search")}
                  className="focus-ring h-8 w-full rounded-[6px] border border-line bg-raised px-2.5 text-[11.5px] text-ink placeholder:text-faint"
                />
              </div>
              <p className="shrink-0 px-3 pb-1 pt-1.5 text-[10px] leading-4 text-faint">
                {t("watch.panel.dragHint")}
              </p>

              <div className="min-h-0 flex-1 overflow-auto px-1.5 pb-2">
                {filtered.length === 0 ? (
                  <p className="px-2 py-6 text-center text-[11px] text-faint">{t("watch.panel.empty")}</p>
                ) : (
                  filtered.map((view) => (
                    <div
                      key={view.room.id}
                      onPointerEnter={(event) => {
                        window.clearTimeout(pickerTimer.current);
                        const rect = event.currentTarget.getBoundingClientRect();
                        setPicker({ roomId: view.room.id, x: rect.left, y: rect.top });
                      }}
                      onPointerLeave={() => {
                        window.clearTimeout(pickerTimer.current);
                        pickerTimer.current = window.setTimeout(() => setPicker(null), 160);
                      }}
                      onClick={() => void watchRoom(view.room.id)}
                      className="group relative flex cursor-pointer items-center gap-2 rounded-[6px] px-2 py-1.5 transition-colors hover:bg-raised"
                    >
                      {view.room.avatarUrl ? (
                        <img
                          src={view.room.avatarUrl}
                          alt=""
                          referrerPolicy="no-referrer"
                          className="h-6 w-6 shrink-0 rounded-full object-cover ring-1 ring-line"
                        />
                      ) : (
                        <span className="h-6 w-6 shrink-0 rounded-full bg-raised" />
                      )}
                      <div className="min-w-0 flex-1 leading-tight">
                        <div className="truncate text-[11.5px] text-ink">
                          {view.room.title || view.room.hostName}
                        </div>
                        <div className="truncate text-[10px] text-faint">{view.room.hostName}</div>
                      </div>
                      <span className="shrink-0 font-mono text-[10px] tabular-nums text-faint">›</span>
                    </div>
                  ))
                )}
              </div>

              <div className="shrink-0 space-y-1.5 border-t border-line p-2">
                <div className="flex gap-1.5">
                  <input
                    value={input}
                    onChange={(event) => setInput(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") void addAndWatch();
                    }}
                    placeholder={t("watch.inputPlaceholder")}
                    className="focus-ring h-8 min-w-0 flex-1 rounded-[6px] border border-line bg-raised px-2.5 text-[11.5px] text-ink placeholder:text-faint"
                  />
                  <Button
                    size="md"
                    variant="primary"
                    disabled={busy || input.trim().length === 0}
                    onClick={() => void addAndWatch()}
                  >
                    {t("watch.addAndWatch")}
                  </Button>
                </div>
              </div>
            </div>
          ) : null}
        </div>
      </div>

      {picker && candidates.some((view) => view.room.id === picker.roomId)
        ? createPortal(
            <div
              className="fixed z-[70] w-[208px] rounded-[8px] border border-line bg-surface p-2 shadow-pop animate-pop"
              style={{
                left: Math.max(8, picker.x - 204),
                top: Math.min(picker.y, window.innerHeight - 200),
              }}
              onPointerEnter={() => window.clearTimeout(pickerTimer.current)}
              onPointerLeave={() => {
                window.clearTimeout(pickerTimer.current);
                pickerTimer.current = window.setTimeout(() => setPicker(null), 160);
              }}
            >
              <div className="mb-1.5 px-0.5 text-[10px] uppercase tracking-[0.08em] text-faint">
                {t("watch.panel.place")}
              </div>
              <div className="grid grid-cols-2 gap-1.5">
                {Array.from({ length: count }).map((_, index) => (
                  <button
                    key={index}
                    type="button"
                    onClick={() => {
                      const roomId = picker.roomId;
                      setPicker(null);
                      void watchRoom(roomId, index);
                    }}
                    className="focus-ring flex flex-col items-center gap-1 rounded-[6px] border border-line bg-raised px-1.5 py-2 text-[10.5px] text-muted transition-colors hover:border-accent-line hover:bg-accent-soft hover:text-accent-text"
                  >
                    <MiniLayout tree={tree} target={index} />
                    <span>{t("watch.cellN", { n: index + 1 })}</span>
                  </button>
                ))}
                {count < MAX_SLOTS ? (
                  <button
                    type="button"
                    onClick={() => {
                      const roomId = picker.roomId;
                      setPicker(null);
                      void watchRoom(roomId);
                    }}
                    className="focus-ring flex flex-col items-center gap-1 rounded-[6px] border border-dashed border-line-strong bg-surface px-1.5 py-2 text-[10.5px] text-muted transition-colors hover:border-accent-line hover:bg-accent-soft hover:text-accent-text"
                  >
                    <MiniLayout tree={buildTree(count + 1)} target={count} />
                    <span>{t("watch.panel.placeNew")}</span>
                  </button>
                ) : null}
              </div>
            </div>,
            document.body,
          )
        : null}

      <aside
        onPointerEnter={() => hoverChrome("rail", true)}
        onPointerLeave={() => hoverChrome("rail", false)}
        className={cx(
          "flex w-11 shrink-0 flex-col border-l border-line bg-surface",
          fullscreen &&
            "absolute inset-y-0 right-0 z-30 bg-surface/95 backdrop-blur-md transition-transform duration-200",
          fullscreen && !hoverRail && "translate-x-full",
        )}
      >
        <button
          type="button"
          onClick={() => setPanelOpen((open) => !open)}
          title={t("watch.panel.title")}
          className="focus-ring flex h-full w-full flex-col items-center gap-1 py-1.5 text-muted hover:text-ink"
        >
          <span className="flex min-h-0 w-full flex-1 flex-col items-center gap-1 overflow-hidden px-1 pt-1">
            {candidates.map((view) =>
              view.room.avatarUrl ? (
                <img
                  key={view.room.id}
                  src={view.room.avatarUrl}
                  alt=""
                  referrerPolicy="no-referrer"
                  className="h-6 w-6 shrink-0 rounded-full object-cover ring-2 ring-surface"
                />
              ) : (
                <span key={view.room.id} className="h-6 w-6 shrink-0 rounded-full bg-raised ring-2 ring-surface" />
              ),
            )}
          </span>
          <span className="shrink-0 font-mono text-[10px] tabular-nums text-faint">{candidates.length}</span>
          <span className="shrink-0 rounded-[4px] border border-line px-1 text-[10px] leading-4 text-muted">
            {panelOpen ? "»" : "«"}
          </span>
        </button>
      </aside>

      {fullscreen ? (
        <>
          <div
            aria-hidden
            onPointerEnter={() => hoverChrome("top", true)}
            onPointerLeave={() => hoverChrome("top", false)}
            className={cx("absolute inset-x-0 top-0 z-40 h-1.5", hoverTop && "pointer-events-none")}
          />
          <div
            aria-hidden
            onPointerEnter={() => hoverChrome("rail", true)}
            onPointerLeave={() => hoverChrome("rail", false)}
            className={cx("absolute inset-y-0 right-0 z-40 w-1.5", hoverRail && "pointer-events-none")}
          />
        </>
      ) : null}

      {drag ? (
        <div
          className="pointer-events-none fixed z-[60] max-w-[220px] truncate rounded-[6px] border border-line bg-surface px-2 py-1 text-[11px] text-ink shadow-pop"
          style={{ left: drag.x + 14, top: drag.y + 12 }}
        >
          {t("watch.cellN", { n: drag.payload.index + 1 })}
        </div>
      ) : null}
    </div>
  );
}

type CellProps = {
  session: ViewSlotInfo;
  room?: RoomView["room"];
  index: number;
  count: number;
  compact: boolean;
  volume: number;
  danmaku: boolean;
  onVolume: (volume: number) => void;
  onToggleDanmaku: () => void;
  onQuality: (qn: number) => void;
  onLine: (line: string | null) => void;
  onDanmakuSettings: () => void;
  onClose: () => void;
  onRetry: () => void;
  onSwapNext: () => void;
  onDragStart: (event: ReactPointerEvent) => void;
};

function SlotCell({
  session,
  room,
  index,
  count,
  compact,
  volume,
  danmaku,
  onVolume,
  onToggleDanmaku,
  onQuality,
  onLine,
  onDanmakuSettings,
  onClose,
  onRetry,
  onSwapNext,
  onDragStart,
}: CellProps) {
  const { t } = useTranslation();
  const [fatal, setFatal] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [fill, setFill] = useState(false);
  const [lines, setLines] = useState<LineInfo[] | null>(null);
  const loadLines = useCallback(async () => {
    if (lines) return;
    try {
      setLines(await roomLines(session.roomId, session.requestedQn || undefined));
    } catch {
      setLines([]);
    }
  }, [lines, session.roomId, session.requestedQn]);
  const retryIn = useSecondsLeft(session.retryAt);
  const sounding = volume > 0;
  const degraded =
    session.requestedQn > 0 && session.actualQn > 0 && session.actualQn !== session.requestedQn;
  const actualQuality =
    (session.quality ?? "").split(" · ")[0].trim() ||
    t("watch.qn." + session.actualQn, { defaultValue: String(session.actualQn) });

  useEffect(() => {
    setFatal(null);
  }, [session.url]);

  return (
    <section
      className={[
        "group relative flex h-full w-full flex-col overflow-hidden bg-black transition-[border-color,box-shadow] duration-200",
        compact ? "rounded-none border-0" : "rounded-[8px] border",
        sounding ? "border-accent/50 shadow-[0_0_0_1px_rgba(255,180,58,0.16)]" : "border-white/10",
      ].join(" ")}
    >
      <header className="pointer-events-none absolute inset-x-0 top-0 z-20 flex h-8 items-center gap-1.5 bg-gradient-to-b from-black/85 via-black/45 to-transparent px-2 opacity-0 transition-opacity duration-200 group-hover:opacity-100 group-focus-within:opacity-100">
        <span
          onPointerDown={(event) => {
            if (count > 1) onDragStart(event);
          }}
          title={t("watch.dragHint")}
          className={[
            "pointer-events-auto flex h-6 w-5 shrink-0 touch-none items-center justify-center rounded-[3px] text-white/45",
            count > 1 ? "cursor-grab hover:bg-white/10 hover:text-white active:cursor-grabbing" : "",
          ].join(" ")}
        >
          <svg width="6" height="12" viewBox="0 0 6 12" fill="currentColor" aria-hidden>
            <circle cx="1.5" cy="2" r="1" />
            <circle cx="4.5" cy="2" r="1" />
            <circle cx="1.5" cy="6" r="1" />
            <circle cx="4.5" cy="6" r="1" />
            <circle cx="1.5" cy="10" r="1" />
            <circle cx="4.5" cy="10" r="1" />
          </svg>
        </span>
        <span className="pointer-events-auto shrink-0 rounded-[4px] bg-white/12 px-1 font-mono text-[9.5px] leading-4 text-white/70">
          {String(index + 1).padStart(2, "0")}
        </span>
        {room?.avatarUrl ? (
          <img
            src={room.avatarUrl}
            alt=""
            referrerPolicy="no-referrer"
            className="h-[18px] w-[18px] shrink-0 rounded-full object-cover ring-1 ring-white/20"
          />
        ) : (
          <span className="h-[18px] w-[18px] shrink-0 rounded-full bg-white/15" />
        )}
        <div className="pointer-events-auto min-w-0 flex-1 leading-tight">
          <div className="truncate text-[11.5px] font-medium text-white/90 [text-shadow:0_1px_2px_rgba(0,0,0,0.5)]">
            {room?.title ?? t("watch.unknownRoom")}
          </div>
        </div>

        {degraded ? (
          <span
            title={t("watch.degradedHint")}
            className="shrink-0 rounded-[4px] border border-warn/35 bg-warn/15 px-1 py-0.5 font-mono text-[9.5px] leading-4 text-warn"
          >
            ↓ {actualQuality}
          </span>
        ) : null}

        <span className="pointer-events-auto relative">
          <button
            type="button"
            onClick={() => setMenuOpen((open) => !open)}
            aria-label={t("watch.cellMenu")}
            className="focus-ring flex h-6 w-6 items-center justify-center rounded-[5px] text-white/50 transition-colors hover:bg-white/15 hover:text-white"
          >
            ⋯
          </button>
          {menuOpen ? (
            <>
              <button
                type="button"
                aria-label={t("watch.close")}
                onClick={() => setMenuOpen(false)}
                className="fixed inset-0 z-30 cursor-default"
              />
              <div className="absolute right-0 top-7 z-40 w-[168px] rounded-[6px] border border-white/12 bg-[#17171a]/98 p-1 text-[11px] text-white/80 shadow-2xl backdrop-blur-md">
                <button
                  type="button"
                  onClick={() => {
                    setFill((value) => !value);
                    setMenuOpen(false);
                  }}
                  className="flex w-full items-center justify-between rounded-[4px] px-2 py-1.5 text-left hover:bg-white/8"
                >
                  <span>{t("watch.fill")}</span>
                  <span className="font-mono text-[10px] text-white/40">{fill ? "ON" : "OFF"}</span>
                </button>
                {count > 1 ? (
                  <button
                    type="button"
                    onClick={() => {
                      onSwapNext();
                      setMenuOpen(false);
                    }}
                    className="w-full rounded-[4px] px-2 py-1.5 text-left hover:bg-white/8"
                  >
                    {t("watch.swapNext")}
                  </button>
                ) : null}
                <button
                  type="button"
                  onClick={() => {
                    setMenuOpen(false);
                    onClose();
                  }}
                  className="w-full rounded-[4px] px-2 py-1.5 text-left text-rec/90 hover:bg-rec/12"
                >
                  {t("watch.remove")}
                </button>
              </div>
            </>
          ) : null}
        </span>

        <button
          type="button"
          onClick={onClose}
          aria-label={t("watch.close")}
          className="pointer-events-auto focus-ring flex h-6 w-6 items-center justify-center rounded-[5px] text-white/50 transition-colors hover:bg-white/15 hover:text-white"
        >
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
            <path d="M1.5 1.5l7 7M8.5 1.5l-7 7" strokeLinecap="round" />
          </svg>
        </button>
      </header>

      <div className="relative min-h-0 flex-1 bg-black">
        {session.url && !fatal ? (
          <Player
            key={session.url}
            url={session.url}
            volume={volume}
            onVolumeChange={onVolume}
            statsCorner="bottom-left"
            fit={fill ? "cover" : "contain"}
            onReload={onRetry}
            onMenuOpen={() => void loadLines()}
            onOpenDanmakuSettings={onDanmakuSettings}
            quality={{
              value: session.requestedQn,
              options: QN_OPTIONS.map((qn) => ({
                value: qn,
                label: qn === 0 ? t("watch.qn.auto") : t("watch.qn." + qn),
              })),
              actual: degraded ? actualQuality : undefined,
              onChange: onQuality,
            }}
            lines={{
              value: session.preferredLine ?? null,
              options: [
                { value: null, label: t("watch.lineAuto") },
                ...(lines ?? []).map((line, lineIndex) => ({
                  value: line.name,
                  label: t("watch.lineN", { n: lineIndex + 1 }),
                  hint: line.name,
                })),
                ...(session.preferredLine && !(lines ?? []).some((line) => line.name === session.preferredLine)
                  ? [{ value: session.preferredLine, label: session.preferredLine, hint: session.preferredLine }]
                  : []),
              ],
              onChange: onLine,
            }}
            onFatal={(detail) => setFatal(detail)}
            danmakuEnabled={danmaku}
            onToggleDanmaku={onToggleDanmaku}
            overlay={
              <div className="pointer-events-none absolute inset-0">
                <DanmakuCanvas roomId={session.roomId} enabled={danmaku} />
              </div>
            }
          />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center">
            {session.status === "error" || fatal ? (
              <>
                <span className="text-[12px] text-rec">{t("watch.status.error")}</span>
                <span className="max-w-full break-all font-mono text-[10px] text-white/40">
                  {fatal ?? session.error}
                </span>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => {
                    setFatal(null);
                    onRetry();
                  }}
                >
                  {t("watch.retry")}
                </Button>
              </>
            ) : (
              <>
                <span className="h-4 w-4 animate-spin rounded-full border-2 border-white/20 border-t-accent" />
                <span className="text-[11px] text-white/60">
                  {session.status === "reconnecting"
                    ? retryIn && retryIn > 0
                      ? t("watch.status.retryIn", { n: retryIn })
                      : t("watch.status.reconnecting")
                    : t("watch.status.starting")}
                </span>
                {session.error ? (
                  <span className="max-w-full break-all font-mono text-[10px] text-white/35">{session.error}</span>
                ) : null}
              </>
            )}
          </div>
        )}
      </div>
    </section>
  );
}


type Rect = { x: number; y: number; w: number; h: number };

function miniRects(node: SplitNode, box: Rect, gap: number, out: Map<number, Rect>) {
  if (node.kind === "leaf") {
    out.set(node.index, box);
    return;
  }
  const horizontal = node.dir === "row";
  const avail = (horizontal ? box.w : box.h) - gap;
  const first = Math.max(0, avail * node.ratio);
  if (horizontal) {
    miniRects(node.a, { ...box, w: first }, gap, out);
    miniRects(node.b, { ...box, x: box.x + first + gap, w: Math.max(0, avail - first) }, gap, out);
  } else {
    miniRects(node.a, { ...box, h: first }, gap, out);
    miniRects(node.b, { ...box, y: box.y + first + gap, h: Math.max(0, avail - first) }, gap, out);
  }
}

/** 候选放置器里的小版式图：画出当前切法，并把目标格点亮。 */
function MiniLayout({ tree, target }: { tree: SplitNode; target: number }) {
  const rects = new Map<number, Rect>();
  miniRects(tree, { x: 0, y: 0, w: 100, h: 64 }, 2.5, rects);
  return (
    <svg viewBox="0 0 100 64" className="h-8 w-full" aria-hidden>
      {Array.from(rects.entries()).map(([index, rect]) => (
        <rect
          key={index}
          x={rect.x}
          y={rect.y}
          width={rect.w}
          height={rect.h}
          rx={3}
          fill={index === target ? "var(--c-accent)" : "var(--c-line-strong)"}
          opacity={index === target ? 0.9 : 0.55}
        />
      ))}
    </svg>
  );
}