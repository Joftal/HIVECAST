import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Select, cx } from "../../components/ui/Controls";
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

const QUALITIES = [10000, 400, 250, 150, 80];
const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"];

/** 全表只有一套尺寸：行 76、缩略图 120x68，右侧详情固定 320。 */
const COLUMNS = "120px 68px 84px minmax(0,1fr) 78px 88px 92px 148px";
const ROW_H = 76;
const THUMB_W = 120;
const THUMB_H = 68;

type PostKind = "mp4" | "shot" | "ass" | "merge";

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
}

function Thumb({
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
    <div className="relative shrink-0 overflow-hidden rounded-[5px] bg-raised" style={{ width: w, height: h }}>
      {src ? (
        <img src={src} alt="" className="h-full w-full object-cover" referrerPolicy="no-referrer" />
      ) : (
        <div className="flex h-full w-full items-center justify-center text-[15px] font-semibold text-faint">
          {room.hostName.slice(0, 1) || "?"}
        </div>
      )}
      {recording ? (
        <span className="absolute left-1.5 top-1.5 flex items-center gap-1 rounded-[3px] bg-rec px-[4px] py-[2px] text-[9px] font-semibold leading-none text-white">
          <span className="h-[4px] w-[4px] rounded-full bg-white" />
          REC
        </span>
      ) : room.isLive ? (
        <span className="absolute left-1.5 top-1.5 flex items-center gap-1 rounded-[3px] bg-black/55 px-[4px] py-[2px] text-[9px] font-semibold leading-none text-white">
          <span className="h-[4px] w-[4px] rounded-full bg-live" />
          LIVE
        </span>
      ) : null}
      {footer ? (
        <div className="tnum absolute inset-x-0 bottom-0 truncate bg-gradient-to-t from-black/75 via-black/30 to-transparent px-1.5 pb-[3px] pt-2 text-[10.5px] font-medium leading-[13px] text-white/90">
          {footer}
        </div>
      ) : null}
    </div>
  );
}
export default function MonitorPage({ onOpenWatch }: { onOpenWatch?: () => void }) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "en-US" ? "en-US" : "zh-CN";

  const [views, setViews] = useState<RoomView[]>([]);
  const [sessions, setSessions] = useState<RecordSession[]>([]);
  const [library, setLibrary] = useState({ count: 0, bytes: 0 });
  const [query, setQuery] = useState("");
  const [platform, setPlatform] = useState("all");
  const [onlyRecording, setOnlyRecording] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [input, setInput] = useState("");
  const [adding, setAdding] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; error: boolean } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [menuId, setMenuId] = useState<string | null>(null);
  useEffect(() => {
    if (!addOpen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setAddOpen(false);
        setInput("");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [addOpen]);
  const [lines, setLines] = useState<LineInfo[] | null>(null);
  const [lineBusy, setLineBusy] = useState(false);
  const [postBusy, setPostBusy] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const [rooms, history, media] = await Promise.all([roomsList(), sessionsList(undefined, 200), mediaList(200)]);
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

  // 右栏跟着选中走：选中被移除或筛选后仍保留，只有第一次进来时自动选第一个。
  useEffect(() => {
    if (selectedId && views.some((view) => view.room.id === selectedId)) return;
    setSelectedId(views.length > 0 ? views[0].room.id : null);
  }, [views, selectedId]);

  useEffect(() => {
    setLines(null);
    setConfirmId(null);
    setNotice(null);
    setMenuId(null);
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
      return (view.room.title + " " + view.room.hostName + " " + view.room.roomKey).toLowerCase().includes(needle);
    });
    // 固定按主播名称排序（中文按拼音、数字按数值），不再提供排序入口。
    const label = (view: RoomView) => view.room.hostName || view.room.title || view.room.roomKey;
    return list.sort((a, b) => label(a).localeCompare(label(b), undefined, { numeric: true }));
  }, [views, query, platform, onlyRecording]);

  const selected = useMemo(() => views.find((view) => view.room.id === selectedId) ?? null, [views, selectedId]);
  const selectedSessions = useMemo(
    () => (selectedId ? sessions.filter((session) => session.roomId === selectedId).slice(0, 4) : []),
    [sessions, selectedId],
  );
  const recorded = useMemo(
    () => sessions.filter((session) => session.roomId === selectedId).reduce((sum, session) => sum + session.fileSize, 0),
    [sessions, selectedId],
  );
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
    setMenuId(null);
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

  const statusText = (view: RoomView) =>
    view.recording
      ? t("monitor.status.recording")
      : view.room.isLive
        ? t("monitor.statusLive")
        : t("monitor.statusOffline");

  const sessionStatus = (session: RecordSession) =>
    session.status === "recording"
      ? t("monitor.status.recording")
      : session.reason
        ? t("monitor.reason." + session.reason)
        : t("monitor.status." + session.status);

  const toggleRecording = (view: RoomView) => {
    const isRecording = Boolean(view.recording);
    void run(view.room.id, () => (isRecording ? recordStop(view.room.id) : recordStart(view.room.id)));
  };

  const renderRow = (view: RoomView) => {
    const room = view.room;
    const isRecording = Boolean(view.recording);
    const isSelected = selectedId === room.id;
    const busy = busyId === room.id;
    const elapsed = view.recording ? now / 1000 - view.recording.startedAt : 0;
    const last = lastByRoom.get(room.id);
    return (
      <div
        key={room.id}
        role="button"
        tabIndex={0}
        onClick={() => setSelectedId(room.id)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            setSelectedId(room.id);
          }
        }}
        style={{ gridTemplateColumns: COLUMNS, minHeight: ROW_H }}
        className={cx(
          "grid cursor-default items-center gap-x-3 border-b border-line px-5 text-left transition-colors",
          isSelected ? "bg-accent-soft" : "hover:bg-raised/70",
          busy ? "opacity-60" : "",
        )}
      >
        <Thumb
          room={room}
          w={THUMB_W}
          h={THUMB_H}
          recording={isRecording}
          elapsed={elapsed}
          lastAt={last ? last.startedAt : null}
          locale={locale}
        />
        <span
          className={cx(
            "truncate text-[12.5px]",
            isRecording ? "font-medium text-rec" : room.isLive ? "text-ink-soft" : "text-faint",
          )}
        >
          {statusText(view)}
        </span>
        <span className="truncate text-[12.5px] text-ink-soft">{t("platformName." + room.platform)}</span>
        <span className="min-w-0">
          <span className="block truncate text-[14px] font-medium leading-5 text-ink" title={room.title}>
            {room.title || room.roomKey}
          </span>
          <span className="mt-0.5 block truncate text-[12px] leading-4 text-muted">
            {(room.hostName || "—") +
              (room.isLive && room.startedAt ? " · " + t("monitor.detailStarted") + " " + hhmm(room.startedAt) : "")}
          </span>
        </span>
        <span className="truncate text-[12.5px] text-muted">
          {t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) })}
        </span>
        <span className="tnum text-right text-[12.5px] text-ink-soft">{isRecording ? clock(elapsed) : "—"}</span>
        <span className="tnum text-right text-[12.5px] text-faint">{last ? recent(last.startedAt) : "—"}</span>
        <span className="relative flex items-center justify-end gap-1.5">
          <Button
            size="sm"
            variant="primary"
            disabled={busy}
            onClick={() => void openWatch(room.id)}
            className="h-8 px-3 text-[12.5px]"
          >
            {t("monitor.watch")}
          </Button>
          <Button
            size="sm"
            disabled={busy || (!room.isLive && !isRecording)}
            title={isRecording ? t("monitor.stopRecord") : t("monitor.startRecord")}
            onClick={() => toggleRecording(view)}
            className={cx("h-8 px-2.5 text-[12.5px]", isRecording ? "border-rec/30 text-rec hover:bg-rec/10" : "")}
          >
            {isRecording ? t("monitor.stop") : t("monitor.rec")}
          </Button>
          <button
            type="button"
            aria-label={t("monitor.more")}
            title={t("monitor.more")}
            onClick={() => setMenuId(menuId === room.id ? null : room.id)}
            className="focus-ring flex h-8 w-7 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
          >
            <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
              <circle cx="3" cy="8" r="1.3" />
              <circle cx="8" cy="8" r="1.3" />
              <circle cx="13" cy="8" r="1.3" />
            </svg>
          </button>
          {menuId === room.id ? (
            <>
              <div className="fixed inset-0 z-30" onClick={() => setMenuId(null)} />
              <div className="absolute right-0 top-9 z-40 w-[176px] overflow-hidden rounded-[8px] border border-line bg-surface py-1 shadow-pop animate-pop">
                <button
                  type="button"
                  onClick={() => {
                    setMenuId(null);
                    void run(room.id, () => roomSetAutoRecord(room.id, !room.autoRecord));
                  }}
                  className="flex h-8 w-full items-center gap-2 px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised"
                >
                  <span className="flex h-3.5 w-3.5 shrink-0 items-center justify-center text-accent-text">
                    {room.autoRecord ? (
                      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden>
                        <path d="M2.5 6.4l2.3 2.3 4.7-4.9" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
                      </svg>
                    ) : null}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{t("monitor.autoRecord")}</span>
                  <span className="shrink-0 text-[11.5px] text-faint">
                    {room.autoRecord ? t("monitor.toggleOn") : t("monitor.toggleOff")}
                  </span>
                </button>
                <button
                  type="button"
                  disabled={!last}
                  onClick={() => {
                    setMenuId(null);
                    void reveal(room.id);
                  }}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised disabled:opacity-40"
                >
                  {t("monitor.reveal")}
                </button>
                <button
                  type="button"
                  onBlur={() => setConfirmId(null)}
                  onClick={() => void removeRoom(room)}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-rec transition-colors hover:bg-rec/10"
                >
                  {confirmId === room.id ? t("monitor.confirmRemove") : t("monitor.removeRoom")}
                </button>
              </div>
            </>
          ) : null}
        </span>
      </div>
    );
  };
  const renderPanel = (view: RoomView) => {
    const room = view.room;
    const busy = busyId === room.id;
    const last = lastByRoom.get(room.id);
    const isRecording = Boolean(view.recording);
    return (
      <aside className="flex w-[320px] shrink-0 flex-col overflow-y-auto border-l border-line bg-surface">
        <div className="p-4">
          <button
            type="button"
            onClick={() => void openWatch(room.id)}
            title={t("monitor.watch")}
            className="focus-ring block w-full cursor-pointer overflow-hidden rounded-[7px]"
          >
            <Thumb
              room={room}
              w={288}
              h={162}
              recording={isRecording}
              elapsed={view.recording ? now / 1000 - view.recording.startedAt : 0}
              lastAt={last ? last.startedAt : null}
              locale={locale}
            />
          </button>

          <div className="mt-3 flex items-center gap-1.5 text-[12.5px]">
            <span
              className={cx(
                "font-medium",
                isRecording ? "text-rec" : room.isLive ? "text-live" : "text-faint",
              )}
            >
              {statusText(view)}
            </span>
            {room.isLive && room.startedAt ? (
              <span className="tnum text-muted">· {t("monitor.detailStarted")} {hhmm(room.startedAt)}</span>
            ) : null}
          </div>
          <h2 className="mt-1.5 text-[15px] font-semibold leading-[22px] text-ink">{room.title || room.roomKey}</h2>
          <p className="mt-1 truncate text-[12.5px] text-muted">{room.hostName || "—"}</p>
          <p className="mt-1 truncate text-[11.5px] text-faint" title={room.canonicalUrl}>
            {room.canonicalUrl}
          </p>

          <div className="mt-3.5 grid grid-cols-2 gap-x-4 gap-y-3 rounded-[7px] bg-raised/60 px-3 py-2.5">
            {[
              { label: t("monitor.colArea"), value: room.areaName ?? "—" },
              { label: t("monitor.detailRecorded"), value: size(recorded) },
              { label: t("monitor.colQuality"), value: t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) }) },
              { label: t("monitor.colRecent"), value: last ? recent(last.startedAt) : "—" },
            ].map((item) => (
              <div key={item.label} className="min-w-0">
                <div className="text-[11.5px] text-faint">{item.label}</div>
                <div className="mt-0.5 truncate text-[12.5px] text-ink-soft">{item.value}</div>
              </div>
            ))}
          </div>

          <div className="mt-4 text-[11.5px] font-medium text-muted">{t("monitor.panelSettings")}</div>
          <div className="mt-2 space-y-2.5">
            <div className="flex items-center gap-2">
              <span className="w-[62px] shrink-0 text-[12.5px] text-muted">{t("monitor.quality")}</span>
              <Select
                value={String(room.recordQn)}
                size="md"
                disabled={busy}
                onChange={(next) => void run(room.id, () => roomSetQuality(room.id, Number(next), room.viewQn))}
                options={QUALITIES.map((qn) => ({ value: String(qn), label: t("monitor.qn." + qn) }))}
                className="flex-1"
              />
            </div>
            <div className="flex items-center gap-2">
              <span className="w-[62px] shrink-0 text-[12.5px] text-muted">{t("monitor.line")}</span>
              <Select
                value={room.recordLine ?? ""}
                size="md"
                disabled={lineBusy || busy}
                title={t("monitor.lineHint")}
                onChange={(next) => void run(room.id, () => roomSetLine(room.id, next || null))}
                options={lineOptions}
                className="flex-1"
              />
            </div>
            {lines === null ? (
              <button
                type="button"
                onClick={() => void loadLines(room.id, room.recordQn)}
                className="focus-ring inline-flex items-center gap-1 rounded-ctl px-1.5 py-0.5 text-[12px] text-accent-text transition-colors hover:bg-accent-soft"
              >
                {lineBusy ? t("common.loading") : t("monitor.lineLoad")}
              </button>
            ) : lines.length === 0 ? (
              <span className="block text-[11.5px] text-faint">{t("monitor.lineEmpty")}</span>
            ) : null}
          </div>

          <div className="mt-4 flex items-center gap-2 border-t border-line pt-3">
            <span className="text-[11.5px] font-medium text-muted">{t("monitor.sessions")}</span>
            {mergeable > 1 ? (
              <button
                type="button"
                onClick={() => void doPost(room.id, "merge")}
                disabled={postBusy === postKey(room.id, "merge")}
                title={t("monitor.post.mergeHint", { n: mergeable })}
                className="focus-ring ml-auto rounded-ctl px-1.5 py-0.5 text-[11.5px] text-accent-text transition-colors hover:bg-accent-soft disabled:opacity-40"
              >
                {t("monitor.post.merge")}
              </button>
            ) : null}
          </div>
          {selectedSessions.length === 0 ? (
            <p className="py-3 text-[12px] text-faint">{t("monitor.noHistory")}</p>
          ) : (
            <div className="mt-1">
              {selectedSessions.map((session) => {
                const ready = session.status !== "recording" && Boolean(session.filePath);
                return (
                  <div key={session.id} className="border-t border-line py-2 first:border-t-0">
                    <div className="flex items-center justify-between text-[12px]">
                      <span className="tnum text-ink-soft">
                        {recent(session.startedAt)} · {clockMs(session.durationMs)}
                      </span>
                      <span className="tnum text-muted">{size(session.fileSize)}</span>
                    </div>
                    <div className="mt-1 flex items-center gap-1">
                      <span className="min-w-0 flex-1 truncate text-[11.5px] text-muted">
                        {sessionStatus(session)}
                      </span>
                      {ready ? (
                        <span className="flex shrink-0 items-center gap-1">
                          <button
                            type="button"
                            disabled={postBusy === postKey(room.id, "mp4", session.id)}
                            title={t("monitor.post.mp4Hint")}
                            onClick={() => void doPost(room.id, "mp4", session.id)}
                            className="focus-ring h-6 rounded-ctl border border-line px-1.5 text-[11px] text-ink-soft transition-colors hover:bg-raised disabled:opacity-40"
                          >
                            MP4
                          </button>
                          <button
                            type="button"
                            disabled={postBusy === postKey(room.id, "shot", session.id)}
                            title={t("monitor.post.shotHint")}
                            onClick={() => void doPost(room.id, "shot", session.id)}
                            className="focus-ring h-6 rounded-ctl border border-line px-1.5 text-[11px] text-ink-soft transition-colors hover:bg-raised disabled:opacity-40"
                          >
                            {t("monitor.post.shot")}
                          </button>
                          {session.danmakuPath && session.danmakuCount > 0 ? (
                            <button
                              type="button"
                              disabled={postBusy === postKey(room.id, "ass", session.id)}
                              title={t("monitor.post.assHint")}
                              onClick={() => void doPost(room.id, "ass", session.id)}
                              className="focus-ring h-6 rounded-ctl border border-line px-1.5 text-[11px] text-ink-soft transition-colors hover:bg-raised disabled:opacity-40"
                            >
                              ASS
                            </button>
                          ) : null}
                        </span>
                      ) : null}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </aside>
    );
  };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-3 px-5 pb-3 pt-4">
        <div className="min-w-0">
          <h1 className="text-[21px] font-semibold leading-7 tracking-[-0.01em] text-ink">{t("monitor.title")}</h1>
          <p className="tnum mt-1 text-[12.5px] text-muted">
            {t("monitor.summary", { rooms: views.length, online, recording })}
          </p>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <div className="relative">
            <svg
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
              width="14"
              height="14"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
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
              className="focus-ring h-9 w-[228px] rounded-ctl border border-line bg-surface pl-8 pr-2.5 text-[13px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
            />
          </div>
          <Button size="lg" variant="primary" onClick={() => setAddOpen((open) => !open)}>
            <span className="text-[15px] leading-none">+</span>
            {t("monitor.addRoom")}
          </Button>
        </div>
      </div>

      {addOpen ? (
        <div
          className="fixed inset-0 z-50 flex items-start justify-center bg-black/30 px-6 pt-[88px] backdrop-blur-[2px] animate-fade-in"
          onMouseDown={() => {
            setAddOpen(false);
            setInput("");
          }}
        >
          <form
            onMouseDown={(event) => event.stopPropagation()}
            onSubmit={(event) => {
              event.preventDefault();
              void add();
            }}
            className="w-[560px] max-w-full rounded-panel border border-line bg-surface p-5 shadow-pop animate-pop"
          >
            <div className="flex items-start justify-between gap-3">
              <div>
                <h2 className="text-[15px] font-semibold text-ink">{t("monitor.addRoom")}</h2>
                <p className="mt-1 text-[12px] leading-relaxed text-muted">{t("monitor.inputPlaceholder")}</p>
              </div>
              <button
                type="button"
                aria-label={t("monitor.cancel")}
                onClick={() => {
                  setAddOpen(false);
                  setInput("");
                }}
                className="focus-ring -mr-1 -mt-1 flex h-7 w-7 items-center justify-center rounded-ctl text-faint hover:bg-raised hover:text-ink"
              >
                ✕
              </button>
            </div>
            <input
              autoFocus
              value={input}
              onChange={(event) => setInput(event.target.value)}
              placeholder="https://live.bilibili.com/6"
              spellCheck={false}
              className="focus-ring mt-4 h-10 w-full rounded-ctl border border-line bg-surface px-3 text-[13px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
            />
            <div className="mt-4 flex justify-end gap-2">
              <Button
                size="lg"
                variant="ghost"
                onClick={() => {
                  setAddOpen(false);
                  setInput("");
                }}
              >
                {t("monitor.cancel")}
              </Button>
              <Button size="lg" type="submit" variant="primary" disabled={adding || !input.trim()}>
                {adding ? t("monitor.adding") : t("monitor.add")}
              </Button>
            </div>
          </form>
        </div>
      ) : null}

      {error ? (
        <div className="flex shrink-0 items-center gap-3 border-b border-rec/20 bg-rec/10 px-5 py-2 text-[12.5px] text-rec">
          <span className="min-w-0 flex-1 truncate" title={error}>
            {error}
          </span>
          <button type="button" onClick={() => setError(null)} className="focus-ring rounded-ctl px-1 hover:bg-rec/10">
            ✕
          </button>
        </div>
      ) : null}
      {notice ? (
        <div
          className={cx(
            "flex shrink-0 items-center gap-3 border-b px-5 py-2 text-[12.5px]",
            notice.error ? "border-rec/20 bg-rec/10 text-rec" : "border-ok/20 bg-ok/10 text-ok",
          )}
        >
          <span className="min-w-0 flex-1 truncate" title={notice.text}>
            {notice.text}
          </span>
          <button type="button" onClick={() => setNotice(null)} className="focus-ring rounded-ctl px-1 hover:bg-raised">
            ✕
          </button>
        </div>
      ) : null}

      <div className="flex shrink-0 items-center gap-1 border-b border-line px-5 py-2.5">
        {platformTabs.map((item) => (
          <button
            key={item.key}
            type="button"
            onClick={() => setPlatform(item.key)}
            className={cx(
              "focus-ring rounded-ctl px-2.5 py-1.5 text-[12.5px] transition-colors",
              platform === item.key
                ? "bg-accent-soft font-medium text-accent-text"
                : "text-muted hover:bg-raised hover:text-ink-soft",
            )}
          >
            {item.label}
            <span className="tnum ml-1.5 text-[11.5px] opacity-60">{item.count}</span>
          </button>
        ))}
        <span className="mx-1.5 h-4 w-px bg-line" />
        <button
          type="button"
          onClick={() => setOnlyRecording((on) => !on)}
          className={cx(
            "focus-ring rounded-ctl px-2.5 py-1.5 text-[12.5px] transition-colors",
            onlyRecording
              ? "bg-accent-soft font-medium text-accent-text"
              : "text-muted hover:bg-raised hover:text-ink-soft",
          )}
        >
          {t("monitor.onlyRecording")}
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1 flex-col">
          {views.length > 0 ? (
            <div
              className="grid shrink-0 items-center gap-x-3 border-b border-line px-5 py-2.5 text-[11.5px] font-medium text-muted"
              style={{ gridTemplateColumns: COLUMNS }}
            >
              <span />
              <span>{t("monitor.colStatus")}</span>
              <span>{t("monitor.colPlatform")}</span>
              <span>{t("monitor.colRoom")}</span>
              <span>{t("monitor.colQuality")}</span>
              <span className="text-right">{t("monitor.colDuration")}</span>
              <span className="text-right">{t("monitor.colRecent")}</span>
              <span className="text-right">{t("monitor.colActions")}</span>
            </div>
          ) : null}

          <div className="min-h-0 flex-1 overflow-auto">
            {views.length === 0 ? (
              <div className="flex h-full flex-col items-center justify-center gap-3.5 px-8 pb-10 text-center">
                <div className="flex h-11 w-11 items-center justify-center rounded-[7px] border border-line text-faint">
                  <svg width="19" height="19" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                    <rect x="2.2" y="3.4" width="13.6" height="9.2" rx="2" />
                    <path d="M6.6 15.6h4.8" strokeLinecap="round" />
                  </svg>
                </div>
                <div>
                  <p className="text-[14px] font-medium text-ink">{t("monitor.emptyTitle")}</p>
                  <p className="mt-1.5 max-w-[340px] text-[12.5px] leading-5 text-muted">{t("monitor.emptyBody")}</p>
                </div>
                <Button size="md" variant="primary" onClick={() => setAddOpen(true)}>
                  {t("monitor.addRoom")}
                </Button>
              </div>
            ) : shown.length === 0 ? (
              <div className="flex h-full items-center justify-center px-8 text-center">
                <p className="text-[12.5px] text-muted">{t("monitor.noMatch")}</p>
              </div>
            ) : (
              shown.map(renderRow)
            )}
          </div>
        </div>

        {selected ? (
          renderPanel(selected)
        ) : (
          <aside className="flex w-[320px] shrink-0 items-center justify-center border-l border-line bg-surface px-8">
            <p className="text-center text-[12.5px] leading-5 text-faint">{t("monitor.detailEmpty")}</p>
          </aside>
        )}
      </div>

      <div className="flex h-9 shrink-0 items-center gap-2 border-t border-line px-5 text-[12px] text-muted">
        <span className="tnum">{t("monitor.summary", { rooms: views.length, online, recording })}</span>
        <span className="ml-auto tnum">{t("monitor.footerLibrary", { n: library.count, size: size(library.bytes) })}</span>
      </div>
    </div>
  );
}