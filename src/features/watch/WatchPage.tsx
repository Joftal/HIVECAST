import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import DanmakuCanvas from "./DanmakuCanvas";
import { useDanmakuPrefs } from "../../lib/danmakuPrefs";
import { useSecondsLeft } from "../../lib/countdown";
import Player from "./Player";
import { Button, Segmented } from "../../components/ui/Controls";
import {
  describeError,
  isTauri,
  onRoomsChanged,
  onViewChanged,
  roomAdd,
  roomsList,
  viewClose,
  viewOpen,
  viewRetry,
  viewSetLine,
  viewSetQuality,
  viewSlots,
  roomLines,
  type LineInfo,
  type RoomView,
  type ViewSlotInfo,
} from "../../lib/ipc";

const QN_OPTIONS = [0, 10000, 400, 250, 150, 80] as const;

type Layout = "single" | "twoH" | "twoV" | "threeBig" | "threeCol" | "quad" | "main";

const LAYOUT_CLASS: Record<Layout, string> = {
  single: "grid-cols-1",
  twoH: "grid-cols-2",
  twoV: "grid-rows-2",
  threeBig: "grid-cols-[minmax(0,1.65fr)_minmax(0,1fr)] grid-rows-2",
  threeCol: "grid-cols-3",
  quad: "grid-cols-2 grid-rows-2",
  main: "grid-cols-[minmax(0,1.7fr)_minmax(0,1fr)] grid-rows-3",
};

const LAYOUT_LABEL: Record<Layout, string> = {
  single: "watch.layout.single",
  twoH: "watch.layout.twoH",
  twoV: "watch.layout.twoV",
  threeBig: "watch.layout.threeBig",
  threeCol: "watch.layout.threeCol",
  quad: "watch.layout.quad",
  main: "watch.layout.main",
};

// 布局随路数自适应；同一路数下用户可手动切换，切路数时回到默认。
const LAYOUTS_BY_COUNT: Record<number, Layout[]> = {
  1: ["single"],
  2: ["twoH", "twoV"],
  3: ["threeBig", "threeCol"],
  4: ["quad", "main"],
};

function spanClass(layout: Layout, index: number): string {
  if (layout === "threeBig" && index === 0) return "row-span-2";
  if (layout === "main" && index === 0) return "row-span-3";
  return "";
}

export default function WatchPage() {
  const { t } = useTranslation();
  const [rooms, setRooms] = useState<RoomView[]>([]);
  const [slots, setSlots] = useState<ViewSlotInfo[]>([]);
  const [volumes, setVolumes] = useState<Record<number, number>>({});
  // 「多路发声」提示只弹一次；降到 3 路以下后复位，下次重新提醒。
  const [loudHint, setLoudHint] = useState(true);
  // 弹幕按槽位独立开关：关掉一路不影响其它路（与音量同理）。
  const [danmaku, setDanmaku] = useState<Record<number, boolean>>({});
  const [layoutChoice, setLayoutChoice] = useState<Layout | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [danmakuOpen, setDanmakuOpen] = useState(false);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  const sessions = useMemo(
    () => [...slots].sort((a, b) => a.slot - b.slot),
    [slots],
  );
  const watchedIds = useMemo(() => new Set(sessions.map((s) => s.roomId)), [sessions]);
  const roomById = useMemo(() => {
    const map = new Map<string, RoomView>();
    for (const view of rooms) map.set(view.room.id, view);
    return map;
  }, [rooms]);

  const count = sessions.length;
  const layouts = LAYOUTS_BY_COUNT[count] ?? ["single"];
  const layout: Layout = layoutChoice && layouts.includes(layoutChoice) ? layoutChoice : layouts[0];

  useEffect(() => {
    setLayoutChoice(null);
  }, [count]);

  const sounding = sessions.filter((session) => (volumes[session.slot] ?? 0) > 0).length;

  useEffect(() => {
    if (sounding < 3) setLoudHint(true);
  }, [sounding]);

  const setVolume = (slot: number, volume: number) => {
    setVolumes((prev) => ({ ...prev, [slot]: volume }));
  };

  const toggleDanmaku = (slot: number) => {
    setDanmaku((prev) => ({ ...prev, [slot]: !(prev[slot] ?? true) }));
  };

  const muteAll = () => {
    const next: Record<number, number> = {};
    for (const session of sessions) next[session.slot] = 0;
    setVolumes(next);
  };

  const watchRoom = async (roomId: string) => {
    setBusy(true);
    setError(null);
    try {
      await viewOpen(roomId);
      setPickerOpen(false);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  };

  const addAndWatch = async () => {
    const value = input.trim();
    if (!value) return;
    setBusy(true);
    setError(null);
    try {
      const view = await roomAdd(value);
      await viewOpen(view.room.id);
      setInput("");
      setPickerOpen(false);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  };

  const closeSlot = async (slot: number) => {
    try {
      await viewClose(slot);
      await load();
    } catch (err) {
      setError(describeError(err));
    }
  };

  const changeQuality = async (slot: number, qn: number) => {
    try {
      await viewSetQuality(slot, qn);
    } catch (err) {
      setError(describeError(err));
    }
  };

  const changeLine = async (slot: number, line: string | null) => {
    try {
      await viewSetLine(slot, line);
    } catch (err) {
      setError(describeError(err));
    }
  };

  const retrySlot = async (slot: number) => {
    try {
      await viewRetry(slot);
    } catch (err) {
      setError(describeError(err));
    }
  };

  const candidates = rooms.filter((view) => !watchedIds.has(view.room.id));
  const prefs = useDanmakuPrefs();

  return (
    <div data-theme="dark" className="flex h-full min-h-0 flex-col gap-2.5 bg-canvas p-3 text-ink">
      <div className="flex shrink-0 flex-wrap items-center gap-2">
        <span className="font-mono text-[11px] tabular-nums text-muted">
          {count} <span className="text-faint">/ 4</span>
        </span>

        {count > 1 && layouts.length > 1 && (
          <Segmented
            value={layout}
            onChange={(next) => setLayoutChoice(next)}
            items={layouts.map((option) => ({ key: option, label: t(LAYOUT_LABEL[option]) }))}
          />
        )}

        <div className="ml-auto flex items-center gap-1.5">
          {count > 0 && (
            <Button size="sm" variant="ghost" onClick={() => setDanmakuOpen((open) => !open)}>
              {t("watch.danmakuSettings")}
            </Button>
          )}
          {sounding > 0 && (
            <Button size="sm" variant="ghost" onClick={muteAll}>
              {t("watch.muteAll")}
            </Button>
          )}
          {isTauri() && (
            <Button size="sm" variant="primary" onClick={() => setPickerOpen((open) => !open)}>
              {t("watch.addRoom")}
            </Button>
          )}
        </div>
      </div>

      {error && (
        <div className="flex shrink-0 items-center gap-2 rounded-ctl border border-rec/30 bg-rec/10 px-2.5 py-1.5 text-[11px] text-rec">
          <span className="flex-1 break-all">{error}</span>
          <button type="button" onClick={() => setError(null)} className="text-rec/70 hover:text-rec">
            ✕
          </button>
        </div>
      )}

      {sounding >= 3 && loudHint && (
        <div className="flex shrink-0 items-center gap-2 rounded-ctl border border-accent-line bg-accent-soft px-2.5 py-1.5 text-[11px] text-accent-text">
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
            className="text-accent-text/70 hover:text-accent-text"
          >
            ✕
          </button>
        </div>
      )}

      {danmakuOpen && count > 0 && (
        <div className="panel grid shrink-0 gap-3 p-3 sm:grid-cols-2 lg:grid-cols-4">
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
      )}

      {pickerOpen && isTauri() && (
        <div className="panel shrink-0 space-y-3 p-3">
          <div className="flex gap-2">
            <input
              value={input}
              onChange={(event) => setInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void addAndWatch();
              }}
              placeholder={t("watch.inputPlaceholder")}
              className="focus-ring h-8 min-w-0 flex-1 rounded-ctl border border-line bg-raised px-2.5 text-[12px] text-ink placeholder:text-faint"
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

          {candidates.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {candidates.map((view) => (
                <button
                  key={view.room.id}
                  type="button"
                  disabled={busy || !view.room.isLive}
                  onClick={() => void watchRoom(view.room.id)}
                  className="focus-ring flex max-w-[260px] items-center gap-2 rounded-ctl border border-line bg-raised px-2.5 py-1.5 text-left text-[11px] text-ink-soft transition-colors hover:border-line-strong disabled:opacity-40"
                >
                  <span
                    className={["h-1.5 w-1.5 shrink-0 rounded-full", view.room.isLive ? "bg-live" : "bg-faint"].join(" ")}
                  />
                  <span className="truncate">{view.room.title || view.room.hostName}</span>
                  <span className="shrink-0 text-[10px] text-faint">
                    {view.room.isLive ? t("watch.watchNow") : t("monitor.offline")}
                  </span>
                </button>
              ))}
            </div>
          ) : (
            <p className="text-[11px] text-faint">{t("watch.noCandidates")}</p>
          )}
        </div>
      )}

      {count === 0 ? (
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-4 rounded-panel border border-dashed border-line-strong bg-surface/40 px-6 text-center">
          <div className="grid grid-cols-2 gap-1.5 opacity-60">
            {[0, 1, 2, 3].map((index) => (
              <span
                key={index}
                className="h-6 w-10 rounded-[4px] border border-line-strong bg-raised"
              />
            ))}
          </div>
          <div className="space-y-1">
            <p className="text-[13px] font-medium text-ink">{t("watch.emptyTitle")}</p>
            <p className="max-w-[360px] text-[11.5px] leading-5 text-muted">{t("watch.emptyBody")}</p>
          </div>
          {isTauri() && (
            <Button variant="primary" onClick={() => setPickerOpen(true)}>
              {t("watch.addRoom")}
            </Button>
          )}
        </div>
      ) : (
        <div className={["grid min-h-0 flex-1 gap-2.5", LAYOUT_CLASS[layout]].join(" ")}>
          {sessions.map((session, index) => {
            const view = roomById.get(session.roomId);
            const volume = volumes[session.slot] ?? 0;
            return (
              <SlotCell
                key={session.roomId}
                session={session}
                room={view?.room}
                volume={volume}
                danmaku={danmaku[session.slot] ?? true}
                span={spanClass(layout, index)}
                onVolume={(value) => setVolume(session.slot, value)}
                onToggleDanmaku={() => toggleDanmaku(session.slot)}
                onQuality={(qn) => void changeQuality(session.slot, qn)}
                onLine={(line) => void changeLine(session.slot, line)}
                onDanmakuSettings={() => setDanmakuOpen(true)}
                onClose={() => void closeSlot(session.slot)}
                onRetry={() => void retrySlot(session.slot)}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

type CellProps = {
  session: ViewSlotInfo;
  room?: RoomView["room"];
  volume: number;
  danmaku: boolean;
  span: string;
  onVolume: (volume: number) => void;
  onToggleDanmaku: () => void;
  onQuality: (qn: number) => void;
  onLine: (line: string | null) => void;
  onDanmakuSettings: () => void;
  onClose: () => void;
  onRetry: () => void;
};

function SlotCell({
  session,
  room,
  volume,
  danmaku,
  span,
  onVolume,
  onToggleDanmaku,
  onQuality,
  onLine,
  onDanmakuSettings,
  onClose,
  onRetry,
}: CellProps) {
  const { t } = useTranslation();
  const [fatal, setFatal] = useState<string | null>(null);
  // 线路列表按需拉取：点开下拉时才解析线路，免得 4 路同屏一上来就打平台接口。
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
  // 平台不一定给到请求的画质（未登录 B 站会把原画降到超清），要如实告诉用户。
  const degraded =
    session.requestedQn > 0 && session.actualQn > 0 && session.actualQn !== session.requestedQn;
  // 降级时用平台自己的档位名（如斗鱼「蓝光4M」），避免各平台 qn 语义不一致造成误读。
  const actualQuality =
    (session.quality ?? "").split(" · ")[0].trim() ||
    t(`watch.qn.${session.actualQn}`, { defaultValue: String(session.actualQn) });

  useEffect(() => {
    setFatal(null);
  }, [session.url]);

  return (
    <section
      className={[
        "group relative flex min-h-0 flex-col overflow-hidden rounded-card border bg-black transition-[border-color,box-shadow] duration-200",
        sounding ? "border-accent/45 shadow-[0_0_0_1px_rgba(255,180,58,0.18)]" : "border-line",
        span,
      ].join(" ")}
    >
      <header className="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-center gap-1.5 bg-gradient-to-b from-black/80 via-black/45 to-transparent px-2 pb-6 pt-1.5">
        <span className="pointer-events-auto shrink-0 rounded-[5px] bg-white/12 px-1 font-mono text-[9.5px] leading-4 text-white/75">
          0{session.slot + 1}
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
          <div className="truncate text-[11.5px] font-medium text-white/92 [text-shadow:0_1px_2px_rgba(0,0,0,0.5)]">
            {room?.title ?? t("watch.unknownRoom")}
          </div>
          <div className="truncate text-[10px] text-white/55">
            {room?.hostName}
            {sounding ? " · " + t("watch.audioOn") : ""}
          </div>
        </div>

        <div className="pointer-events-auto flex items-center gap-1">
          {degraded && (
            <span
              title={t("watch.degradedHint")}
              className="shrink-0 rounded-[5px] border border-warn/35 bg-warn/15 px-1 py-0.5 font-mono text-[9.5px] leading-4 text-warn"
            >
              ↓ {actualQuality}
            </span>
          )}

          <button
            type="button"
            onClick={onClose}
            aria-label={t("watch.close")}
            className="focus-ring flex h-6 w-6 items-center justify-center rounded-[6px] text-white/50 transition-colors hover:bg-white/15 hover:text-white"
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
              <path d="M1.5 1.5l7 7M8.5 1.5l-7 7" strokeLinecap="round" />
            </svg>
          </button>
        </div>
      </header>

      <div className="relative min-h-0 flex-1 bg-black">
        {session.url && !fatal ? (
          <Player
            key={session.url}
            url={session.url}
            volume={volume}
            onVolumeChange={onVolume}
            statsCorner="bottom-left"
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
                ...(lines ?? []).map((line, index) => ({
                  value: line.name,
                  label: t("watch.lineN", { n: index + 1 }),
                  hint: line.name,
                })),
                // 已选线路不在本次解析结果里（线路下线/平台换 CDN）时也要能显示出来。
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
              <div className="pointer-events-none absolute inset-x-0 bottom-0 top-7">
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
                {session.error && (
                  <span className="max-w-full break-all font-mono text-[10px] text-white/35">
                    {session.error}
                  </span>
                )}
              </>
            )}
          </div>
        )}
      </div>
    </section>
  );
}

