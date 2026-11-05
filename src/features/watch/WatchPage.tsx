import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import DanmakuCanvas from "./DanmakuCanvas";
import { useDanmakuPrefs } from "../../lib/danmakuPrefs";
import { useSecondsLeft } from "../../lib/countdown";
import Player from "./Player";
import SplitStage, { writeDrag } from "./SplitStage";
import {
  buildTree,
  leafCount,
  parseLayout,
  portraitTree,
  serializeLayout,
  type SavedLayout,
  type SplitNode,
  type Variant,
} from "./split";
import { Button } from "../../components/ui/Controls";
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
  viewRetry,
  viewSetLine,
  viewSetQuality,
  viewSlots,
  type LineInfo,
  type RoomView,
  type ViewSlotInfo,
} from "../../lib/ipc";

const QN_OPTIONS = [0, 10000, 400, 250, 150, 80] as const;
const MAX_SLOTS = 4;

export type WatchSeed = { ids: string[]; nonce: number };

type Props = {
  /** 从监控页多选进入时带过来的房间；nonce 变化才会重新应用。 */
  seed?: WatchSeed | null;
  /** 应用完立刻通知上层清空 seed，避免切回本页时又覆盖一次。 */
  onSeedApplied?: () => void;
};

export default function WatchPage({ seed, onSeedApplied }: Props) {
  const { t } = useTranslation();
  const [rooms, setRooms] = useState<RoomView[]>([]);
  const [slots, setSlots] = useState<ViewSlotInfo[]>([]);
  const [volumes, setVolumes] = useState<Record<number, number>>({});
  const [loudHint, setLoudHint] = useState(true);
  const [danmaku, setDanmaku] = useState<Record<number, boolean>>({});
  const [danmakuOpen, setDanmakuOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // 分屏：树管版式与比例，order 管「第几格放哪个房间」。order 与后端 slot 解耦。
  const [tree, setTree] = useState<SplitNode>(() => buildTree(1));
  const [variant, setVariant] = useState<Variant>("default");
  const [order, setOrder] = useState<(string | null)[]>([]);
  const [panelOpen, setPanelOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [input, setInput] = useState("");

  const [saved, setSaved] = useState<SavedLayout | null>(null);
  const seedRef = useRef(0);
  const latestLayout = useRef("");

  const load = useCallback(async () => {
    try {
      const [nextRooms, nextSlots] = await Promise.all([roomsList(), viewSlots()]);
      setRooms(nextRooms);
      setSlots(nextSlots);
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
      await load();
      if (disposed) return;
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
  }, [load]);

  // 分屏布局：只恢复比例与版式；房间以当前后端会话为准。
  useEffect(() => {
    if (!isTauri()) return;
    void layoutGet()
      .then((raw) => {
        const parsed = parseLayout(raw);
        if (parsed) setSaved(parsed);
      })
      .catch(() => undefined);
  }, []);

  const sessions = useMemo(() => [...slots].sort((a, b) => a.slot - b.slot), [slots]);
  const watchedIds = useMemo(() => new Set(sessions.map((session) => session.roomId)), [sessions]);
  const roomById = useMemo(() => {
    const map = new Map<string, RoomView>();
    for (const view of rooms) map.set(view.room.id, view);
    return map;
  }, [rooms]);

  const count = sessions.length;

  // 会话变化时对齐 order：保留仍在看的房间，新会话补空位，消失的清成 null 后裁掉。
  useEffect(() => {
    const ids = sessions.map((session) => session.roomId);
    setOrder((prev) => {
      const next = prev.filter((id) => id === null || ids.includes(id));
      while (next.length < ids.length) next.push(null);
      for (const id of ids) {
        if (next.includes(id)) continue;
        const at = next.indexOf(null);
        if (at >= 0) next[at] = id;
        else next.push(id);
      }
      return next.slice(0, ids.length);
    });
  }, [sessions]);

  // 存过的布局正好是同一路数时优先用它（含用户拖过的比例）；用完即弃，之后按路数走默认版式。
  useEffect(() => {
    if (!saved) return;
    if (saved.count !== count || leafCount(saved.tree) !== count) return;
    setTree(saved.tree);
    setSaved(null);
  }, [saved, count]);

  // 只有路数真正变化才重置版式；恢复布局那一轮交给上一个 effect。
  const prevCount = useRef<number | null>(null);
  useEffect(() => {
    const first = prevCount.current === null;
    const changed = prevCount.current !== count;
    prevCount.current = count;
    if (first || !changed) return;
    if (saved && saved.count === count && leafCount(saved.tree) === count) return;
    setTree(count === 0 ? buildTree(1) : buildTree(count));
    setVariant("default");
  }, [count, saved]);

  useEffect(() => {
    if (!isTauri() || count === 0) return;
    const payload = serializeLayout(count, tree, order);
    latestLayout.current = payload;
    const timer = window.setTimeout(() => {
      void layoutSet(payload).catch(() => undefined);
    }, 500);
    return () => window.clearTimeout(timer);
  }, [tree, order, count]);

  // 切 Tab 会卸载本页，防抖还没到点就先把布局落库。
  useEffect(() => {
    return () => {
      if (isTauri() && latestLayout.current) {
        void layoutSet(latestLayout.current).catch(() => undefined);
      }
    };
  }, []);

  useEffect(() => {
    if (!seed || seed.nonce === seedRef.current) return;
    seedRef.current = seed.nonce;
    void applySeed(seed.ids).finally(() => onSeedApplied?.());
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

  async function applySeed(ids: string[]) {
    setBusy(true);
    setError(null);
    try {
      const current = await viewSlots();
      await Promise.all(current.map((session) => viewClose(session.slot).catch(() => undefined)));
      const opened: string[] = [];
      for (const id of ids.slice(0, MAX_SLOTS)) {
        try {
          const info = await viewOpen(id);
          opened.push(info.roomId);
        } catch {
          // 单路失败不影响其它路，最后一并刷新状态
        }
      }
      setOrder(opened);
      setPanelOpen(false);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function watchRoom(roomId: string, replaceAt?: number) {
    setBusy(true);
    setError(null);
    try {
      if (replaceAt !== undefined) {
        const previous = order[replaceAt];
        if (previous && previous !== roomId) {
          const session = sessions.find((item) => item.roomId === previous);
          if (session) await viewClose(session.slot);
        }
      }
      await viewOpen(roomId);
      if (replaceAt !== undefined) {
        setOrder((prev) => prev.map((id, index) => (index === replaceAt ? roomId : id)));
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
      await viewOpen(view.room.id);
      setInput("");
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function closeSlot(slot: number) {
    try {
      await viewClose(slot);
      await load();
    } catch (err) {
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

  const swapCells = (from: number, to: number) => {
    setOrder((prev) => {
      const next = [...prev];
      const a = next[from];
      next[from] = next[to];
      next[to] = a;
      return next;
    });
  };

  const switchVariant = (next: Variant) => {
    setVariant(next);
    setTree(buildTree(count, next));
  };

  const usePortrait = () => {
    setVariant("default");
    setTree(portraitTree(count));
  };

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

  return (
    <div data-theme="dark" className="flex h-full min-h-0 bg-[#0b0c0e] text-ink">
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex h-11 shrink-0 items-center gap-2 border-b border-white/8 px-3">
          <span className="font-mono text-[11px] tabular-nums text-white/45">
            {count} <span className="text-white/25">/ 4</span>
          </span>

          {count >= 2 && count <= 3 ? (
            <div className="flex items-center rounded-[6px] border border-white/10 p-0.5">
              {(["default", "alt"] as Variant[]).map((option) => (
                <button
                  key={option}
                  type="button"
                  onClick={() => switchVariant(option)}
                  className={[
                    "focus-ring rounded-[4px] px-2 py-0.5 text-[11px] transition-colors",
                    variant === option ? "bg-white/12 text-white/90" : "text-white/45 hover:text-white/80",
                  ].join(" ")}
                >
                  {t(
                    count === 2
                      ? option === "default"
                        ? "watch.layout.twoH"
                        : "watch.layout.twoV"
                      : option === "default"
                        ? "watch.layout.threeBig"
                        : "watch.layout.threeCol",
                  )}
                </button>
              ))}
            </div>
          ) : null}

          {count >= 2 ? (
            <button
              type="button"
              onClick={usePortrait}
              className="focus-ring rounded-[6px] px-2 py-1 text-[11px] text-white/45 transition-colors hover:bg-white/8 hover:text-white/85"
            >
              {t("watch.portrait")}
            </button>
          ) : null}

          <div className="ml-auto flex items-center gap-1">
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
            <Button size="sm" variant="ghost" onClick={() => setPanelOpen((open) => !open)}>
              {t("watch.panel.title")}
            </Button>
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
          <div className="mx-3 mt-2 flex shrink-0 items-center gap-2 rounded-[6px] border border-white/12 bg-white/6 px-2.5 py-1.5 text-[11px] text-white/75">
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
              className="text-white/40 hover:text-white/80"
            >
              ✕
            </button>
          </div>
        ) : null}

        {danmakuOpen && count > 0 ? (
          <div className="mx-3 mt-2 grid shrink-0 gap-3 rounded-[8px] border border-white/10 bg-white/[0.03] p-3 sm:grid-cols-2 lg:grid-cols-4">
            {(
              [
                ["opacity", t("watch.danmakuOpacity"), 0.3, 1, 0.05, 2],
                ["fontScale", t("watch.danmakuFont"), 0.75, 1.6, 0.05, 2],
                ["speed", t("watch.danmakuSpeed"), 0.5, 2, 0.1, 1],
                ["area", t("watch.danmakuArea"), 0.3, 1, 0.05, 2],
              ] as const
            ).map(([key, label, min, max, step, digits]) => (
              <label key={key} className="flex flex-col gap-1">
                <span className="flex items-baseline justify-between text-[11px] text-white/55">
                  {label}
                  <span className="font-mono text-[10px] tabular-nums text-white/35">
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
                  <span key={index} className="h-6 w-10 rounded-[4px] border border-white/25 bg-white/5" />
                ))}
              </div>
              <div className="space-y-1">
                <p className="text-[13px] font-medium text-white/85">{t("watch.emptyTitle")}</p>
                <p className="max-w-[380px] text-[11.5px] leading-5 text-white/45">{t("watch.emptyBody")}</p>
              </div>
              {isTauri() ? (
                <Button variant="primary" onClick={() => setPanelOpen(true)}>
                  {t("watch.addRoom")}
                </Button>
              ) : null}
            </div>
          ) : (
            <SplitStage
              tree={tree}
              count={count}
              onTreeChange={(next) => setTree(next)}
              onSwap={swapCells}
              onDropRoom={(roomId, index) => void watchRoom(roomId, index)}
              renderCell={(index) => {
                const roomId = order[index] ?? null;
                const session = roomId ? sessions.find((item) => item.roomId === roomId) : undefined;
                const view = roomId ? roomById.get(roomId) : undefined;
                if (!session) {
                  return (
                    <div className="flex h-full items-center justify-center rounded-[8px] border border-dashed border-white/10 bg-white/[0.02] text-[11px] text-white/30">
                      {t("watch.stage.empty")}
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
                  />
                );
              }}
            />
          )}

          {panelOpen ? (
            <div className="absolute inset-y-0 right-0 z-30 flex w-[300px] flex-col border-l border-white/10 bg-[#141416]/95 shadow-2xl backdrop-blur-md">
              <div className="flex h-11 shrink-0 items-center gap-2 border-b border-white/8 px-3">
                <span className="text-[12px] font-medium text-white/85">{t("watch.panel.title")}</span>
                <span className="font-mono text-[10px] tabular-nums text-white/35">
                  {count} / 4
                </span>
                <button
                  type="button"
                  onClick={() => setPanelOpen(false)}
                  aria-label={t("watch.close")}
                  className="focus-ring ml-auto flex h-6 w-6 items-center justify-center rounded-[5px] text-white/40 hover:bg-white/10 hover:text-white/80"
                >
                  ✕
                </button>
              </div>

              <div className="shrink-0 px-2 pt-2">
                <input
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder={t("watch.panel.search")}
                  className="focus-ring h-8 w-full rounded-[6px] border border-white/10 bg-white/5 px-2.5 text-[11.5px] text-white/85 placeholder:text-white/30"
                />
              </div>
              <p className="shrink-0 px-3 pb-1 pt-1.5 text-[10px] leading-4 text-white/30">
                {t("watch.panel.dragHint")}
              </p>

              <div className="min-h-0 flex-1 overflow-auto px-1.5 pb-2">
                {filtered.length === 0 ? (
                  <p className="px-2 py-6 text-center text-[11px] text-white/30">{t("watch.panel.empty")}</p>
                ) : (
                  filtered.map((view) => (
                    <div
                      key={view.room.id}
                      draggable
                      onDragStart={(event) => writeDrag(event, { kind: "room", roomId: view.room.id })}
                      className="group flex cursor-grab items-center gap-2 rounded-[6px] px-2 py-1.5 transition-colors hover:bg-white/6 active:cursor-grabbing"
                    >
                      {view.room.avatarUrl ? (
                        <img
                          src={view.room.avatarUrl}
                          alt=""
                          referrerPolicy="no-referrer"
                          className="h-6 w-6 shrink-0 rounded-full object-cover ring-1 ring-white/15"
                        />
                      ) : (
                        <span className="h-6 w-6 shrink-0 rounded-full bg-white/12" />
                      )}
                      <div className="min-w-0 flex-1 leading-tight">
                        <div className="truncate text-[11.5px] text-white/85">
                          {view.room.title || view.room.hostName}
                        </div>
                        <div className="truncate text-[10px] text-white/40">{view.room.hostName}</div>
                      </div>
                      <button
                        type="button"
                        disabled={busy || count >= MAX_SLOTS}
                        onClick={() => void watchRoom(view.room.id)}
                        className="focus-ring shrink-0 rounded-[4px] px-1.5 py-0.5 text-[10.5px] text-white/45 transition-colors hover:bg-white/10 hover:text-white/90 disabled:opacity-35"
                      >
                        {count >= MAX_SLOTS ? t("watch.panel.full") : t("watch.panel.join")}
                      </button>
                    </div>
                  ))
                )}
              </div>

              <div className="shrink-0 space-y-1.5 border-t border-white/8 p-2">
                <div className="flex gap-1.5">
                  <input
                    value={input}
                    onChange={(event) => setInput(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") void addAndWatch();
                    }}
                    placeholder={t("watch.inputPlaceholder")}
                    className="focus-ring h-8 min-w-0 flex-1 rounded-[6px] border border-white/10 bg-white/5 px-2.5 text-[11.5px] text-white/85 placeholder:text-white/30"
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

      <aside className="flex w-11 shrink-0 flex-col items-center border-l border-white/8 bg-[#0d0e10] py-1.5">
        <button
          type="button"
          onClick={() => setPanelOpen((open) => !open)}
          title={t("watch.panel.title")}
          className="focus-ring flex w-full flex-col items-center gap-1 rounded-[6px] py-1 text-white/50 hover:text-white/85"
        >
          <span className="flex flex-col -space-y-1.5">
            {candidates.slice(0, 3).map((view) =>
              view.room.avatarUrl ? (
                <img
                  key={view.room.id}
                  src={view.room.avatarUrl}
                  alt=""
                  referrerPolicy="no-referrer"
                  className="h-[22px] w-[22px] rounded-full object-cover ring-2 ring-[#0d0e10]"
                />
              ) : (
                <span key={view.room.id} className="h-[22px] w-[22px] rounded-full bg-white/15 ring-2 ring-[#0d0e10]" />
              ),
            )}
          </span>
          <span className="font-mono text-[10px] tabular-nums text-white/40">{candidates.length}</span>
          <span className="rounded-[4px] border border-white/12 px-1 text-[10px] leading-4 text-white/45">
            {panelOpen ? "»" : "«"}
          </span>
        </button>
      </aside>
    </div>
  );
}

type CellProps = {
  session: ViewSlotInfo;
  room?: RoomView["room"];
  index: number;
  count: number;
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
};

function SlotCell({
  session,
  room,
  index,
  count,
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
        "group relative flex h-full w-full flex-col overflow-hidden rounded-[8px] border bg-black transition-[border-color,box-shadow] duration-200",
        sounding ? "border-accent/50 shadow-[0_0_0_1px_rgba(255,180,58,0.16)]" : "border-white/10",
      ].join(" ")}
    >
      <header className="pointer-events-none absolute inset-x-0 top-0 z-20 flex h-8 items-center gap-1.5 bg-gradient-to-b from-black/85 via-black/45 to-transparent px-2 opacity-0 transition-opacity duration-200 group-hover:opacity-100 group-focus-within:opacity-100">
        <span
          draggable={count > 1}
          onDragStart={(event) => writeDrag(event, { kind: "cell", index })}
          title={t("watch.dragHint")}
          className={[
            "pointer-events-auto flex h-5 w-4 shrink-0 items-center justify-center rounded-[3px] text-white/35",
            count > 1 ? "cursor-grab active:cursor-grabbing hover:text-white/80" : "",
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
