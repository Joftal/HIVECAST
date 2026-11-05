import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Segmented, Select, Switch, cx } from "../../components/ui/Controls";
import {
  describeError,
  isTauri,
  mediaList,
  mediaReveal,
  onRoomsChanged,
  recordStart,
  recordStop,
  roomAdd,
  roomLines,
  roomMergeSessions,
  roomRemove,
  roomSetAutoRecord,
  roomSetLine,
  roomSetQuality,
  roomsList,
  sessionExportDanmaku,
  sessionRemux,
  sessionScreenshot,
  sessionsList,
  viewOpen,
  type LineInfo,
  type PostResult,
  type RecordSession,
  type RoomView,
} from "../../lib/ipc";
import { useUiPrefs, type Density } from "../../lib/uiPrefs";

const QUALITIES = [10000, 400, 250, 150, 80];
const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"];
const PLATFORM_DOT: Record<string, string> = {
  bilibili: "#22a7f0",
  douyu: "#ff7a1a",
  huya: "#f5a623",
  douyin: "#ff2d6f",
};

type PostKind = "mp4" | "shot" | "ass" | "merge";
type SortKey = "default" | "online" | "title";

/** 两种密度只差三个数：行高、列宽模板、缩略图尺寸。 */
type Metrics = { row: number; columns: string; thumbW: number; thumbH: number };
const METRICS: Record<Density, Metrics> = {
  comfortable: {
    row: 64,
    columns: "96px 64px 76px minmax(0,1fr) 92px 76px 66px 78px 86px 96px",
    thumbW: 96,
    thumbH: 54,
  },
  compact: {
    row: 54,
    columns: "78px 54px 64px minmax(0,1fr) 76px 64px 58px 68px 74px 84px",
    thumbW: 78,
    thumbH: 44,
  },
};

function pad(value: number): string {
  return value < 10 ? "0" + value : String(value);
}

function clock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0 ? h + ":" + pad(m) + ":" + pad(s) : pad(m) + ":" + pad(s);
}

function clockMs(ms: number): string {
  return clock(ms / 1000);
}

function size(bytes: number): string {
  if (!bytes) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return (unit === 0 ? String(Math.round(value)) : value.toFixed(1)) + " " + units[unit];
}

function hhmm(seconds: number): string {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  return pad(date.getHours()) + ":" + pad(date.getMinutes());
}

/** 最近录制列只放得下 5 个字：今天给时分，更早给月日。 */
function recent(seconds: number): string {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  return sameDay ? hhmm(seconds) : pad(date.getMonth() + 1) + "-" + pad(date.getDate());
}

function compactNumber(value: number, locale: string): string {
  if (!value) return "—";
  return new Intl.NumberFormat(locale === "en-US" ? "en-US" : "zh-CN", {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value);
}

function postKey(roomId: string, kind: PostKind, sessionId?: string): string {
  return roomId + ":" + kind + ":" + (sessionId ?? "");
}function Thumb({
  room,
  w,
  h,
  recording,
  elapsed,
  lastAt,
  locale,
}: {
  room: RoomView["room"];
  w: number;
  h: number;
  recording: boolean;
  elapsed: number;
  lastAt: number | null;
  locale: string;
}) {
  const src = room.coverUrl ?? room.avatarUrl ?? null;
  const footer = recording ? clock(elapsed) : room.isLive ? compactNumber(room.online, locale) : lastAt ? recent(lastAt) : "";
  return (
    <div className="relative shrink-0 overflow-hidden rounded-[4px] bg-raised" style={{ width: w, height: h }}>
      {src ? (
        <img src={src} alt="" className="h-full w-full object-cover" referrerPolicy="no-referrer" />
      ) : (
        <div className="flex h-full w-full items-center justify-center text-[13px] font-semibold text-faint">
          {room.hostName.slice(0, 1) || "?"}
        </div>
      )}
      {recording ? (
        <span className="absolute left-1 top-1 flex items-center gap-1 rounded-[3px] bg-rec px-[3px] py-[1.5px] text-[8px] font-semibold leading-none text-white">
          <span className="h-[3px] w-[3px] rounded-full bg-white" />
          REC
        </span>
      ) : room.isLive ? (
        <span className="absolute left-1 top-1 flex items-center gap-1 rounded-[3px] bg-black/55 px-[3px] py-[1.5px] text-[8px] font-semibold leading-none text-white">
          <span className="h-[3px] w-[3px] rounded-full bg-live" />
          LIVE
        </span>
      ) : null}
      {footer ? (
        <div className="tnum absolute inset-x-0 bottom-0 truncate bg-gradient-to-t from-black/75 via-black/30 to-transparent px-1 pb-[2px] pt-2 text-[9px] font-medium leading-[12px] text-white/90">
          {footer}
        </div>
      ) : null}
    </div>
  );
}

export default function MonitorPage({ onOpenWatch }: { onOpenWatch?: () => void }) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "en-US" ? "en-US" : "zh-CN";
  const density = useUiPrefs((state) => state.density);
  const setDensity = useUiPrefs((state) => state.setDensity);
  const metrics = METRICS[density];

  const [views, setViews] = useState<RoomView[]>([]);
  const [sessions, setSessions] = useState<RecordSession[]>([]);
  const [library, setLibrary] = useState({ count: 0, bytes: 0 });
  const [query, setQuery] = useState("");
  const [platform, setPlatform] = useState("all");
  const [onlyRecording, setOnlyRecording] = useState(false);
  const [sort, setSort] = useState<SortKey>("default");
  const [addOpen, setAddOpen] = useState(false);
  const [input, setInput] = useState("");
  const [adding, setAdding] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; error: boolean } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [lines, setLines] = useState<LineInfo[] | null>(null);
  const [lineBusy, setLineBusy] = useState(false);
  const [postBusy, setPostBusy] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const [rooms, history, media] = await Promise.all([
        roomsList(),
        sessionsList(undefined, 200),
        mediaList(200),
      ]);
      setViews(rooms);
      setSessions(history);
      setLibrary({
        count: media.filter((item) => item.hasFile).length,
        bytes: media.reduce((sum, item) => sum + (item.hasFile ? item.size : 0), 0),
      });
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void load();
    let unlisten: (() => void) | undefined;
    let alive = true;
    onRoomsChanged(() => {
      void load();
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      if (unlisten) unlisten();
    };
  }, [load]);

  const recordingActive = useMemo(() => views.some((view) => view.recording), [views]);
  useEffect(() => {
    if (!recordingActive) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [recordingActive]);

  useEffect(() => {
    setLines(null);
    setConfirmId(null);
    setNotice(null);
  }, [selectedId]);

  const online = views.filter((view) => view.room.isLive).length;
  const recording = views.filter((view) => view.recording).length;

  const counts = useMemo(() => {
    const map: Record<string, number> = { all: views.length };
    for (const view of views) map[view.room.platform] = (map[view.room.platform] ?? 0) + 1;
    return map;
  }, [views]);

  const lastByRoom = useMemo(() => {
    const map = new Map<string, RecordSession>();
    for (const session of sessions) {
      const prev = map.get(session.roomId);
      if (!prev || session.startedAt > prev.startedAt) map.set(session.roomId, session);
    }
    return map;
  }, [sessions]);

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const list = views.filter((view) => {
      if (platform !== "all" && view.room.platform !== platform) return false;
      if (onlyRecording && !view.recording) return false;
      if (!needle) return true;
      return (view.room.title + " " + view.room.hostName + " " + view.room.roomKey)
        .toLowerCase()
        .includes(needle);
    });
    const rank = (view: RoomView) => (view.recording ? 0 : view.room.isLive ? 1 : 2);
    return list.sort((a, b) => {
      if (sort === "online") return b.room.online - a.room.online;
      if (sort === "title") return (a.room.title || a.room.roomKey).localeCompare(b.room.title || b.room.roomKey);
      const delta = rank(a) - rank(b);
      return delta !== 0 ? delta : b.room.lastCheckedAt - a.room.lastCheckedAt;
    });
  }, [views, query, platform, onlyRecording, sort]);

  const selectedSessions = useMemo(
    () => (selectedId ? sessions.filter((session) => session.roomId === selectedId).slice(0, 6) : []),
    [sessions, selectedId],
  );
  const recorded = selectedSessions.reduce((sum, session) => sum + session.fileSize, 0);
  const mergeable = selectedSessions.filter((session) => session.status !== "recording" && session.filePath).length;

  const run = useCallback(
    async (id: string, action: () => Promise<unknown>) => {
      setBusyId(id);
      setError(null);
      try {
        await action();
        await load();
      } catch (err) {
        setError(describeError(err));
      } finally {
        setBusyId(null);
      }
    },
    [load],
  );

  const add = async () => {
    const value = input.trim();
    if (!value || adding) return;
    setAdding(true);
    setError(null);
    try {
      await roomAdd(value);
      setInput("");
      setAddOpen(false);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setAdding(false);
    }
  };

  const openWatch = async (roomId: string) => {
    setBusyId(roomId);
    setError(null);
    try {
      await viewOpen(roomId);
      onOpenWatch?.();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusyId(null);
    }
  };

  const reveal = async (roomId: string) => {
    const last = lastByRoom.get(roomId);
    if (!last) return;
    try {
      await mediaReveal(last.id);
    } catch (err) {
      setError(describeError(err));
    }
  };

  const removeRoom = async (room: RoomView["room"]) => {
    if (confirmId !== room.id) {
      setConfirmId(room.id);
      return;
    }
    setConfirmId(null);
    if (selectedId === room.id) setSelectedId(null);
    await run(room.id, () => roomRemove(room.id));
  };

  const loadLines = async (roomId: string, qn: number) => {
    setLineBusy(true);
    try {
      setLines(await roomLines(roomId, qn));
    } catch (err) {
      setLines([]);
      setError(describeError(err));
    } finally {
      setLineBusy(false);
    }
  };

  const doPost = async (roomId: string, kind: PostKind, sessionId?: string) => {
    setPostBusy(postKey(roomId, kind, sessionId));
    setNotice(null);
    try {
      let result: PostResult;
      if (kind === "merge") result = await roomMergeSessions(roomId);
      else if (!sessionId) throw new Error("missing session");
      else if (kind === "mp4") result = await sessionRemux(sessionId);
      else if (kind === "shot") result = await sessionScreenshot(sessionId);
      else result = await sessionExportDanmaku(sessionId);
      const name = result.path.split(/[\\/]/).pop() ?? result.path;
      setNotice({ text: size(result.size) + " · " + name, error: false });
      await load();
    } catch (err) {
      setNotice({ text: describeError(err), error: true });
    } finally {
      setPostBusy(null);
    }
  };

  const lineOptions = [{ value: "", label: t("monitor.lineAuto") }].concat(
    (lines ?? []).map((line) => ({ value: line.name, label: line.name + " · " + line.quality })),
  );

  const platformTabs = [
    { key: "all", label: t("monitor.filterAll"), count: counts.all ?? 0 },
    ...PLATFORMS.map((key) => ({ key, label: t("platformName." + key), count: counts[key] ?? 0 })),
  ].filter((item) => item.key === "all" || item.count > 0);

  const renderRow = (view: RoomView) => {
    const room = view.room;
    const isRecording = Boolean(view.recording);
    const isSelected = selectedId === room.id;
    const busy = busyId === room.id;
    const elapsed = view.recording ? now / 1000 - view.recording.startedAt : 0;
    const last = lastByRoom.get(room.id);
    const dot = PLATFORM_DOT[room.platform] ?? "var(--c-faint)";
    const toggle = () => setSelectedId(isSelected ? null : room.id);
    return (
      <div key={room.id}>
        <div
          role="button"
          tabIndex={0}
          onClick={toggle}
          onKeyDown={(event) => {
            if (event.key === "Enter" || event.key === " ") {
              event.preventDefault();
              toggle();
            }
          }}
          style={{ gridTemplateColumns: metrics.columns, minHeight: metrics.row }}
          className={cx(
            "grid cursor-default items-center gap-x-3 border-b border-line px-5 text-left transition-colors",
            isSelected ? "bg-accent-soft" : "hover:bg-raised/70",
            busy ? "opacity-60" : "",
          )}
        >
          <Thumb
            room={room}
            w={metrics.thumbW}
            h={metrics.thumbH}
            recording={isRecording}
            elapsed={elapsed}
            lastAt={last ? last.startedAt : null}
            locale={locale}
          />
          <span className="flex items-center gap-1.5 text-[11px]">
            <span
              className={cx(
                "h-[5px] w-[5px] shrink-0 rounded-full",
                isRecording ? "bg-rec" : room.isLive ? "bg-live" : "bg-faint",
              )}
            />
            <span className={cx("truncate", isRecording ? "text-rec" : room.isLive ? "text-ink-soft" : "text-faint")}>
              {isRecording ? t("monitor.status.recording") : room.isLive ? t("monitor.statusLive") : t("monitor.statusOffline")}
            </span>
          </span>
          <span className="flex min-w-0 items-center gap-1.5 text-[11px] text-ink-soft">
            <span className="h-[5px] w-[5px] shrink-0 rounded-full" style={{ background: dot }} />
            <span className="truncate">{t("platformName." + room.platform)}</span>
          </span>
          <span className="min-w-0">
            <span className="block truncate text-[12.5px] font-medium leading-5 text-ink" title={room.title}>
              {room.title || room.roomKey}
            </span>
            <span className="mt-[1px] block truncate text-[10.5px] leading-4 text-muted">
              {(room.hostName || "—") + (room.isLive && room.startedAt ? " · " + t("monitor.detailStarted") + " " + hhmm(room.startedAt) : "")}
            </span>
          </span>
          <span className="min-w-0 truncate text-[11px] text-muted">{room.areaName ?? "—"}</span>
          <span className="tnum text-right text-[11px] text-ink-soft">{compactNumber(room.online, locale)}</span>
          <span className="min-w-0 truncate text-[11px] text-muted">
            {t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) })}
          </span>
          <span className="tnum text-right text-[11px] text-ink-soft">{isRecording ? clock(elapsed) : "—"}</span>
          <span className="tnum text-right text-[11px] text-faint">{last ? recent(last.startedAt) : "—"}</span>
          <span className="flex justify-end">
            <button
              type="button"
              onClick={(event) => {
                event.stopPropagation();
                void run(room.id, () => (isRecording ? recordStop(room.id) : recordStart(room.id)));
              }}
              disabled={busy || (!room.isLive && !isRecording)}
              title={isRecording ? t("monitor.stopRecord") : t("monitor.startRecord")}
              className={cx(
                "focus-ring h-6 rounded-[4px] border px-2 text-[10.5px] font-medium transition-colors",
                isRecording
                  ? "border-rec/30 text-rec hover:bg-rec/10"
                  : "border-line text-ink-soft hover:border-line-strong hover:bg-raised",
                busy || (!room.isLive && !isRecording) ? "pointer-events-none opacity-40" : "",
              )}
            >
              {isRecording ? t("monitor.stop") : t("monitor.rec")}
            </button>
          </span>
        </div>
        {isSelected ? renderExpanded(view) : null}
      </div>
    );
  };

  const renderExpanded = (view: RoomView) => {
    const room = view.room;
    const busy = busyId === room.id;
    const last = lastByRoom.get(room.id);
    const confirm = confirmId === room.id;
    return (
      <div className="border-b border-line bg-surface px-5 py-3.5">
        <div className="flex items-start gap-4">
          <Thumb
            room={room}
            w={164}
            h={92}
            recording={Boolean(view.recording)}
            elapsed={view.recording ? now / 1000 - view.recording.startedAt : 0}
            lastAt={last ? last.startedAt : null}
            locale={locale}
          />
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-2">
              <span className="shrink-0 text-[12.5px] font-medium text-ink">{room.hostName || "—"}</span>
              <span className="min-w-0 truncate text-[10.5px] text-faint">{room.canonicalUrl}</span>
            </div>
            <div className="mt-2.5 grid grid-cols-[repeat(4,minmax(0,96px))] gap-x-7">
              {[
                { label: t("monitor.detailStarted"), value: room.startedAt ? hhmm(room.startedAt) : "—" },
                { label: t("monitor.detailViewers"), value: compactNumber(room.online, locale) },
                { label: t("monitor.detailArea"), value: room.areaName ?? "—" },
                { label: t("monitor.detailRecorded"), value: size(recorded) },
              ].map((item) => (
                <div key={item.label}>
                  <div className="text-[10px] text-faint">{item.label}</div>
                  <div className="tnum mt-1 truncate text-[11.5px] text-ink-soft">{item.value}</div>
                </div>
              ))}
            </div>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <span className="text-[10.5px] text-faint">{t("monitor.quality")}</span>
              <Select
                value={String(room.recordQn)}
                disabled={busy}
                onChange={(next) => void run(room.id, () => roomSetQuality(room.id, Number(next), room.viewQn))}
                options={QUALITIES.map((qn) => ({ value: String(qn), label: t("monitor.qn." + qn) }))}
                className="w-[92px]"
              />
              <span className="ml-1 text-[10.5px] text-faint">{t("monitor.line")}</span>
              <Select
                value={room.recordLine ?? ""}
                disabled={lineBusy || busy}
                title={t("monitor.lineHint")}
                onChange={(next) => void run(room.id, () => roomSetLine(room.id, next || null))}
                options={lineOptions}
                className="w-[168px]"
              />
              {lines === null ? (
                <button
                  type="button"
                  onClick={() => void loadLines(room.id, room.recordQn)}
                  className="focus-ring rounded-[4px] px-1.5 py-0.5 text-[10.5px] text-accent-text transition-colors hover:bg-accent-soft"
                >
                  {lineBusy ? t("common.loading") : t("monitor.lineLoad")}
                </button>
              ) : lines.length === 0 ? (
                <span className="text-[10px] text-faint">{t("monitor.lineEmpty")}</span>
              ) : null}
              <span className="ml-3 flex items-center gap-2">
                <span className="text-[10.5px] text-muted">{t("monitor.autoRecord")}</span>
                <Switch
                  checked={room.autoRecord}
                  label={t("monitor.autoRecord")}
                  onChange={(next) => void run(room.id, () => roomSetAutoRecord(room.id, next))}
                />
              </span>
            </div>
            <div className="mt-3 border-t border-line pt-2">
              <div className="flex items-center gap-3">
                <span className="text-[10.5px] text-faint">{t("monitor.sessions")}</span>
                {mergeable > 1 ? (
                  <button
                    type="button"
                    onClick={() => void doPost(room.id, "merge")}
                    disabled={postBusy === postKey(room.id, "merge")}
                    className="focus-ring rounded-[3px] px-1 text-[10.5px] text-accent-text hover:underline disabled:opacity-40"
                  >
                    {t("monitor.post.merge")}
                  </button>
                ) : null}
              </div>
              {selectedSessions.length === 0 ? (
                <p className="py-1.5 text-[10.5px] text-faint">{t("monitor.noHistory")}</p>
              ) : (
                selectedSessions.slice(0, 2).map((session) => (
                  <div key={session.id} className="group/row flex items-center gap-3 py-1 text-[10.5px] text-muted">
                    <span className="tnum w-12 shrink-0">{recent(session.startedAt)}</span>
                    <span className="tnum w-12 shrink-0">{clockMs(session.durationMs)}</span>
                    <span className="tnum w-16 shrink-0">{size(session.fileSize)}</span>
                    <span className="min-w-0 flex-1 truncate">
                      {session.status === "recording"
                        ? t("monitor.status.recording")
                        : session.reason
                          ? t("monitor.reason." + session.reason)
                          : t("monitor.status." + session.status)}
                    </span>
                    {session.status !== "recording" && session.filePath ? (
                      <span className="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover/row:opacity-100">
                        <button
                          type="button"
                          disabled={postBusy === postKey(room.id, "mp4", session.id)}
                          onClick={() => void doPost(room.id, "mp4", session.id)}
                          className="focus-ring rounded-[3px] px-1 text-[10px] text-accent-text hover:bg-accent-soft disabled:opacity-40"
                        >
                          {t("monitor.post.mp4")}
                        </button>
                        <button
                          type="button"
                          disabled={postBusy === postKey(room.id, "shot", session.id)}
                          onClick={() => void doPost(room.id, "shot", session.id)}
                          className="focus-ring rounded-[3px] px-1 text-[10px] text-accent-text hover:bg-accent-soft disabled:opacity-40"
                        >
                          {t("monitor.post.shot")}
                        </button>
                        {session.danmakuPath && session.danmakuCount > 0 ? (
                          <button
                            type="button"
                            disabled={postBusy === postKey(room.id, "ass", session.id)}
                            onClick={() => void doPost(room.id, "ass", session.id)}
                            className="focus-ring rounded-[3px] px-1 text-[10px] text-accent-text hover:bg-accent-soft disabled:opacity-40"
                          >
                            {t("monitor.post.ass")}
                          </button>
                        ) : null}
                      </span>
                    ) : null}
                  </div>
                ))
              )}
            </div>
          </div>
          <div className="flex w-[104px] shrink-0 flex-col gap-1.5">
            <Button size="sm" variant="primary" disabled={busy} onClick={() => void openWatch(room.id)}>
              {t("monitor.watch")}
            </Button>
            <Button size="sm" disabled={!last || busy} onClick={() => void reveal(room.id)}>
              {t("monitor.reveal")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="text-muted hover:text-rec"
              onBlur={() => setConfirmId(null)}
              onClick={() => void removeRoom(room)}
            >
              {confirm ? t("monitor.confirmRemove") : t("monitor.removeRoom")}
            </Button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-start gap-3 px-5 pb-3 pt-4">
        <div className="min-w-0">
          <h1 className="text-[17px] font-semibold leading-6 tracking-[-0.01em] text-ink">{t("monitor.title")}</h1>
          <p className="tnum mt-1 text-[10.5px] text-muted">
            {t("monitor.summary", { rooms: views.length, online, recording })}
          </p>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <div className="relative">
            <svg
              className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-faint"
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.4"
              aria-hidden
            >
              <circle cx="7" cy="7" r="4.4" />
              <path d="m10.4 10.4 3 3" strokeLinecap="round" />
            </svg>
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("monitor.searchPlaceholder")}
              spellCheck={false}
              className="focus-ring h-8 w-[196px] rounded-[5px] border border-line bg-surface pl-7 pr-2 text-[11.5px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
            />
          </div>
          <Select
            value={sort}
            onChange={(next) => setSort(next as SortKey)}
            title={t("monitor.sortLabel")}
            options={[
              { value: "default", label: t("monitor.sortDefault") },
              { value: "online", label: t("monitor.sortOnline") },
              { value: "title", label: t("monitor.sortTitle") },
            ]}
            className="w-[104px]"
          />
          <Button variant="primary" onClick={() => setAddOpen((open) => !open)}>
            <span className="text-[13px] leading-none">+</span>
            {t("monitor.addRoom")}
          </Button>
        </div>
      </div>

      {addOpen ? (
        <form
          className="flex shrink-0 items-center gap-2 border-y border-line bg-raised/50 px-5 py-2"
          onSubmit={(event) => {
            event.preventDefault();
            void add();
          }}
        >
          <input
            autoFocus
            value={input}
            onChange={(event) => setInput(event.target.value)}
            placeholder={t("monitor.inputPlaceholder")}
            spellCheck={false}
            className="focus-ring h-8 w-[520px] max-w-[62%] rounded-[5px] border border-line bg-surface px-2.5 text-[12px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
          />
          <Button type="submit" variant="primary" disabled={adding || !input.trim()}>
            {adding ? t("monitor.adding") : t("monitor.add")}
          </Button>
          <Button
            variant="ghost"
            onClick={() => {
              setAddOpen(false);
              setInput("");
            }}
          >
            {t("monitor.cancel")}
          </Button>
        </form>
      ) : null}

      {error ? (
        <div className="flex shrink-0 items-center gap-3 border-b border-rec/20 bg-rec/10 px-5 py-1.5 text-[11px] text-rec">
          <span className="min-w-0 flex-1 truncate" title={error}>{error}</span>
          <button type="button" onClick={() => setError(null)} className="focus-ring rounded-[3px] px-1 hover:bg-rec/10">✕</button>
        </div>
      ) : null}
      {notice ? (
        <div
          className={cx(
            "flex shrink-0 items-center gap-3 border-b px-5 py-1.5 text-[11px]",
            notice.error ? "border-rec/20 bg-rec/10 text-rec" : "border-ok/20 bg-ok/10 text-ok",
          )}
        >
          <span className="min-w-0 flex-1 truncate" title={notice.text}>{notice.text}</span>
          <button type="button" onClick={() => setNotice(null)} className="focus-ring rounded-[3px] px-1 hover:bg-raised">✕</button>
        </div>
      ) : null}

      <div className="flex shrink-0 items-center gap-1 border-b border-line px-5 py-2">
        {platformTabs.map((item) => (
          <button
            key={item.key}
            type="button"
            onClick={() => setPlatform(item.key)}
            className={cx(
              "focus-ring rounded-[4px] px-2 py-1 text-[11px] transition-colors",
              platform === item.key
                ? "bg-accent-soft font-medium text-accent-text"
                : "text-muted hover:bg-raised hover:text-ink-soft",
            )}
          >
            {item.label}
            <span className="tnum ml-1 text-[10px] opacity-60">{item.count}</span>
          </button>
        ))}
        <span className="mx-1 h-3.5 w-px bg-line" />
        <button
          type="button"
          onClick={() => setOnlyRecording((on) => !on)}
          className={cx(
            "focus-ring rounded-[4px] px-2 py-1 text-[11px] transition-colors",
            onlyRecording
              ? "bg-accent-soft font-medium text-accent-text"
              : "text-muted hover:bg-raised hover:text-ink-soft",
          )}
        >
          {t("monitor.onlyRecording")}
        </button>
        <div className="ml-auto flex items-center gap-2">
          <span className="text-[10px] text-faint">{t("monitor.density")}</span>
          <Segmented
            value={density}
            onChange={(next) => setDensity(next)}
            items={[
              { key: "comfortable", label: t("monitor.densityComfortable") },
              { key: "compact", label: t("monitor.densityCompact") },
            ]}
          />
        </div>
      </div>

      {views.length > 0 ? (
        <div
          className="grid shrink-0 items-center gap-x-3 border-b border-line px-5 py-2 text-[10.5px] text-faint"
          style={{ gridTemplateColumns: metrics.columns }}
        >
          <span />
          <span>{t("monitor.colStatus")}</span>
          <span>{t("monitor.colPlatform")}</span>
          <span>{t("monitor.colRoom")}</span>
          <span>{t("monitor.colArea")}</span>
          <span className="text-right">{t("monitor.colOnline")}</span>
          <span>{t("monitor.colQuality")}</span>
          <span className="text-right">{t("monitor.colDuration")}</span>
          <span className="text-right">{t("monitor.colRecent")}</span>
          <span className="text-right">{t("monitor.colActions")}</span>
        </div>
      ) : null}

      <div className="min-h-0 flex-1 overflow-auto">
        {views.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-3 px-8 pb-10 text-center">
            <div className="flex h-10 w-10 items-center justify-center rounded-[6px] border border-line text-faint">
              <svg width="17" height="17" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                <rect x="2.2" y="3.4" width="13.6" height="9.2" rx="2" />
                <path d="M6.6 15.6h4.8" strokeLinecap="round" />
              </svg>
            </div>
            <div>
              <p className="text-[12.5px] font-medium text-ink">{t("monitor.emptyTitle")}</p>
              <p className="mt-1 max-w-[320px] text-[11px] leading-5 text-muted">{t("monitor.emptyBody")}</p>
            </div>
            <Button size="sm" variant="primary" onClick={() => setAddOpen(true)}>
              {t("monitor.addRoom")}
            </Button>
          </div>
        ) : shown.length === 0 ? (
          <div className="flex h-full items-center justify-center px-8 text-center">
            <p className="text-[11.5px] text-muted">{t("monitor.noMatch")}</p>
          </div>
        ) : (
          shown.map(renderRow)
        )}
      </div>

      <div className="flex h-8 shrink-0 items-center gap-2 border-t border-line px-5 text-[10.5px] text-muted">
        <span className="tnum">{t("monitor.summary", { rooms: views.length, online, recording })}</span>
        <span className="ml-auto tnum">{t("monitor.footerLibrary", { n: library.count, size: size(library.bytes) })}</span>
      </div>
    </div>
  );
}
