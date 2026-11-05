import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import DanmakuCanvas from "./DanmakuCanvas";
import Player from "./Player";
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
  viewSetQuality,
  viewSlots,
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
  // 弹幕按槽位独立开关：关掉一路不影响其它路（与音量同理）。
  const [danmaku, setDanmaku] = useState<Record<number, boolean>>({});
  const [layoutChoice, setLayoutChoice] = useState<Layout | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
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

  const retrySlot = async (slot: number) => {
    try {
      await viewRetry(slot);
    } catch (err) {
      setError(describeError(err));
    }
  };

  const candidates = rooms.filter((view) => !watchedIds.has(view.room.id));

  return (
    <div className="flex h-full min-h-0 flex-col gap-3">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-[15px] font-semibold text-zinc-100">{t("watch.title")}</h1>
          <p className="mt-0.5 text-[12px] text-zinc-500">{t("watch.subtitle")}</p>
        </div>

        <div className="flex items-center gap-2">
          {count > 1 && layouts.length > 1 && (
            <div className="flex overflow-hidden rounded-lg border hairline p-0.5">
              {layouts.map((option) => (
                <button
                  key={option}
                  type="button"
                  onClick={() => setLayoutChoice(option)}
                  className={[
                    "focus-ring rounded-md px-2 py-1 text-[11px] transition-colors",
                    layout === option
                      ? "bg-ink-700 text-zinc-100"
                      : "text-zinc-500 hover:text-zinc-300",
                  ].join(" ")}
                >
                  {t(LAYOUT_LABEL[option])}
                </button>
              ))}
            </div>
          )}

          {sounding > 0 && (
            <button
              type="button"
              onClick={muteAll}
              className="focus-ring rounded-lg border hairline px-2.5 py-1.5 text-[11px] text-zinc-300 transition-colors hover:bg-ink-700"
            >
              {t("watch.muteAll")}
            </button>
          )}

          {isTauri() && (
            <button
              type="button"
              onClick={() => setPickerOpen((open) => !open)}
              className="focus-ring rounded-lg border border-hive-500/40 bg-hive-500/10 px-3 py-1.5 text-[11px] text-hive-300 transition-colors hover:bg-hive-500/20"
            >
              + {t("watch.addRoom")}
            </button>
          )}
        </div>
      </div>

      {error && (
        <div className="flex items-center gap-2 rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-200">
          <span className="flex-1 break-all">{error}</span>
          <button type="button" onClick={() => setError(null)} className="text-red-300/70 hover:text-red-200">
            ✕
          </button>
        </div>
      )}

      {sounding >= 3 && (
        <p className="text-[11px] text-amber-300/80">{t("watch.multiSound")}</p>
      )}

      {pickerOpen && isTauri() && (
        <div className="panel space-y-3 p-3">
          <div className="flex gap-2">
            <input
              value={input}
              onChange={(event) => setInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void addAndWatch();
              }}
              placeholder={t("watch.inputPlaceholder")}
              className="focus-ring min-w-0 flex-1 rounded-lg border hairline bg-ink-800/70 px-3 py-2 text-[12px] text-zinc-200 placeholder:text-zinc-600"
            />
            <button
              type="button"
              disabled={busy || input.trim().length === 0}
              onClick={() => void addAndWatch()}
              className="focus-ring rounded-lg bg-hive-500 px-3 py-2 text-[11px] font-medium text-ink-900 disabled:opacity-40"
            >
              {t("watch.addAndWatch")}
            </button>
          </div>

          {candidates.length > 0 ? (
            <div className="flex flex-wrap gap-2">
              {candidates.map((view) => (
                <button
                  key={view.room.id}
                  type="button"
                  disabled={busy || !view.room.isLive}
                  onClick={() => void watchRoom(view.room.id)}
                  className="focus-ring flex max-w-[260px] items-center gap-2 rounded-lg border hairline bg-ink-800/50 px-2.5 py-1.5 text-left text-[11px] text-zinc-300 transition-colors hover:bg-ink-700 disabled:opacity-40"
                >
                  <span
                    className={[
                      "h-1.5 w-1.5 shrink-0 rounded-full",
                      view.room.isLive ? "bg-emerald-400" : "bg-zinc-600",
                    ].join(" ")}
                  />
                  <span className="truncate">{view.room.title || view.room.hostName}</span>
                  <span className="shrink-0 text-[10px] text-zinc-500">
                    {view.room.isLive ? t("watch.watchNow") : t("monitor.offline")}
                  </span>
                </button>
              ))}
            </div>
          ) : (
            <p className="text-[11px] text-zinc-500">{t("watch.noCandidates")}</p>
          )}
        </div>
      )}

      {count === 0 ? (
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 rounded-2xl border border-dashed border-white/10 bg-black/20 px-6 text-center">
          <span className="font-mono text-3xl text-zinc-700">0 / 4</span>
          <p className="text-[13px] text-zinc-400">{t("watch.emptyTitle")}</p>
          <p className="max-w-[420px] text-[11px] leading-5 text-zinc-600">{t("watch.emptyBody")}</p>
          {isTauri() && (
            <button
              type="button"
              onClick={() => setPickerOpen(true)}
              className="focus-ring rounded-full border border-hive-500/40 bg-hive-500/10 px-4 py-1.5 text-[11px] text-hive-300 hover:bg-hive-500/20"
            >
              {t("watch.addRoom")}
            </button>
          )}
        </div>
      ) : (
        <div className={["grid min-h-0 flex-1 gap-3", LAYOUT_CLASS[layout]].join(" ")}>
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
                onClose={() => void closeSlot(session.slot)}
                onRetry={() => void retrySlot(session.slot)}
              />
            );
          })}
        </div>
      )}

      <p className="shrink-0 text-[11px] leading-5 text-zinc-600">{t("watch.hint")}</p>
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
  onClose,
  onRetry,
}: CellProps) {
  const { t } = useTranslation();
  const [fatal, setFatal] = useState<string | null>(null);
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
        "panel relative flex min-h-0 flex-col overflow-hidden transition-shadow",
        span,
        sounding ? "ring-1 ring-hive-500/60" : "",
      ].join(" ")}
    >
      <header className="flex shrink-0 items-center gap-2 px-2.5 py-1.5">
        <span className="font-mono text-[10px] text-zinc-600">0{session.slot + 1}</span>
        {room?.avatarUrl ? (
          <img
            src={room.avatarUrl}
            alt=""
            referrerPolicy="no-referrer"
            className="h-5 w-5 shrink-0 rounded-full object-cover"
          />
        ) : (
          <span className="h-5 w-5 shrink-0 rounded-full bg-ink-700" />
        )}
        <div className="min-w-0 flex-1 leading-tight">
          <div className="truncate text-[11.5px] text-zinc-200">{room?.title ?? t("watch.unknownRoom")}</div>
          <div className="truncate text-[10px] text-zinc-500">
            {room?.hostName}
            {session.quality ? " · " + session.quality : ""}
            {sounding ? " · " + t("watch.audioOn") : ""}
          </div>
        </div>

        <select
          value={session.requestedQn}
          onChange={(event) => onQuality(Number(event.target.value))}
          aria-label={t("watch.quality")}
          className="focus-ring rounded-md border hairline bg-ink-800/80 px-1 py-0.5 text-[10px] text-zinc-300"
        >
          {QN_OPTIONS.map((qn) => (
            <option key={qn} value={qn}>
              {qn === 0 ? t("watch.qn.auto") : t(`watch.qn.${qn}`)}
            </option>
          ))}
        </select>

        {degraded && (
          <span
            title={t("watch.degradedHint")}
            className="shrink-0 rounded border border-amber-500/40 bg-amber-500/10 px-1 py-0.5 font-mono text-[10px] text-amber-300/90"
          >
            ↓ {actualQuality}
          </span>
        )}

        <button
          type="button"
          onClick={onClose}
          aria-label={t("watch.close")}
          className="focus-ring rounded-md px-1.5 py-0.5 text-[11px] text-zinc-500 hover:bg-white/10 hover:text-zinc-200"
        >
          ✕
        </button>
      </header>

      <div className="relative min-h-0 flex-1 bg-black">
        {session.url && !fatal ? (
          <Player
            key={session.url}
            url={session.url}
            volume={volume}
            onVolumeChange={onVolume}
            onFatal={(detail) => setFatal(detail)}
            danmakuEnabled={danmaku}
            onToggleDanmaku={onToggleDanmaku}
            overlay={<DanmakuCanvas roomId={session.roomId} enabled={danmaku} />}
          />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center">
            {session.status === "error" || fatal ? (
              <>
                <span className="text-[12px] text-red-300">{t("watch.status.error")}</span>
                <span className="max-w-full break-all font-mono text-[10px] text-zinc-500">
                  {fatal ?? session.error}
                </span>
                <button
                  type="button"
                  onClick={() => {
                    setFatal(null);
                    onRetry();
                  }}
                  className="focus-ring rounded-full border hairline bg-ink-700 px-3 py-1 text-[11px] text-zinc-200 hover:bg-ink-600"
                >
                  {t("watch.retry")}
                </button>
              </>
            ) : (
              <>
                <span className="h-4 w-4 animate-spin rounded-full border-2 border-zinc-600 border-t-hive-400" />
                <span className="text-[11px] text-zinc-400">
                  {session.status === "reconnecting"
                    ? t("watch.status.reconnecting")
                    : t("watch.status.starting")}
                </span>
                {session.error && (
                  <span className="max-w-full break-all font-mono text-[10px] text-zinc-600">
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
