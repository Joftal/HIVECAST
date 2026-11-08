import { useCallback, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Button, Switch, cx } from "../../components/ui/Controls";
import { SelectMenu } from "../../components/ui/SelectMenu";
import { LayoutGlyph, type LayoutKind } from "../../components/ui/LayoutGlyph";
import {
  describeError,
  isTauri,
  mediaReveal,
  onRoomsChanged,
  recordStart,
  recordStop,
  roomAdd,
  roomLines,
  roomMergeSessions,
  roomRefresh,
  roomRemove,
  roomSetAutoRecord,
  roomSetMonitor,
  roomSetLine,
  roomSetQuality,
  roomsList,
  sessionsList,
  viewOpen,
  type LineInfo,
  type PostResult,
  type RecordSession,
  type RoomView,
} from "../../lib/ipc";

const QUALITIES = [0, 40000, 10000, 400, 250, 150, 80];
const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"];
const PLATFORM_DOT: Record<string, string> = {
  bilibili: "#22a7f0",
  douyu: "#ff7a1a",
  huya: "#f5a623",
  douyin: "#ff2d6f",
};

/** 全表只有一套尺寸：行 76、缩略图 120x68，右侧详情固定 320。 */
const COLUMNS = "120px 66px 72px 84px minmax(0,1fr) 76px 138px 148px";
const ROW_H = 76;
const THUMB_W = 120;
const THUMB_H = 68;

type PostKind = "merge";

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

/** 上次录制开始时间统一用完整时间戳：2026-11-06 00:02:28。 */
function datetime(seconds: number): string {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  return (
    date.getFullYear() +
    "-" +
    pad(date.getMonth() + 1) +
    "-" +
    pad(date.getDate()) +
    " " +
    pad(date.getHours()) +
    ":" +
    pad(date.getMinutes()) +
    ":" +
    pad(date.getSeconds())
  );
}

type SortKey = "live" | "record";

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

/** 头像：没有 avatarUrl 时用平台色 + 首字母占位。 */
function Avatar({ room, size: px }: { room: RoomView["room"]; size: number }) {
  const src = room.avatarUrl;
  return src ? (
    <img
      src={src}
      alt=""
      referrerPolicy="no-referrer"
      className="shrink-0 rounded-full bg-raised object-cover"
      style={{ width: px, height: px }}
    />
  ) : (
    <span
      className="flex shrink-0 items-center justify-center rounded-full text-[11px] font-semibold text-white"
      style={{ width: px, height: px, backgroundColor: PLATFORM_DOT[room.platform] ?? "#8a8f98" }}
    >
      {(room.hostName || room.title || "?").slice(0, 1).toUpperCase()}
    </span>
  );
}

function Thumb({
  room,
  w,
  h,
  recording,
  lastAt,
  locale,
  fluid,
}: {
  room: RoomView["room"];
  w: number;
  h: number;
  recording: boolean;
  lastAt: number | null;
  locale: string;
  fluid?: boolean;
}) {
  const src = room.coverUrl ?? room.avatarUrl ?? null;
  const footer = room.isLive ? compactNumber(room.online, locale) : lastAt ? recent(lastAt) : "";
  return (
    <div
      className={cx("relative shrink-0 overflow-hidden rounded-[5px] bg-raised", fluid ? "h-full w-full" : "")}
      style={fluid ? undefined : { width: w, height: h }}
    >
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
/** 可排序表头：一次升序、再点降序、第三次取消。 */
function SortHead({
  label,
  active,
  dir,
  onClick,
}: {
  label: string;
  active: boolean;
  dir: "asc" | "desc";
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cx(
        "group focus-ring -mx-1 inline-flex items-center gap-1 rounded-ctl px-1 py-0.5 text-left transition-colors hover:text-ink-soft",
        active ? "text-ink" : "",
      )}
    >
      {label}
      <svg
        width="8"
        height="8"
        viewBox="0 0 12 12"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        aria-hidden
        className={cx(
          "transition-opacity",
          active ? "opacity-90" : "opacity-0 group-hover:opacity-40",
          dir === "desc" ? "rotate-180" : "",
        )}
      >
        <path d="M2.5 7.5 6 4 9.5 7.5" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </button>
  );
}

export default function MonitorPage({
  onOpenWatch,
}: {
  onOpenWatch?: (roomIds?: string[], kind?: LayoutKind) => void;
}) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "en-US" ? "en-US" : "zh-CN";

  const [views, setViews] = useState<RoomView[]>([]);
  const [sessions, setSessions] = useState<RecordSession[]>([]);

  const [query, setQuery] = useState("");
  const [platform, setPlatform] = useState("all");
  const [sort, setSort] = useState<{ key: SortKey; dir: "asc" | "desc" } | null>(null);
  const [onlyRecording, setOnlyRecording] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [input, setInput] = useState("");
  // 添加房间时的两个开关：不监控就没有自动录制，所以关掉监控会顺手关掉它。
  const [addMonitor, setAddMonitor] = useState(true);
  const [addAutoRecord, setAddAutoRecord] = useState(true);
  const [adding, setAdding] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; error: boolean } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  // 房间下播后把它从多选里摘掉，避免带着离线房间进分屏。
  useEffect(() => {
    setPicked((prev) => {
      const live = new Set(views.filter((view) => view.room.isLive).map((view) => view.room.id));
      const next = new Set([...prev].filter((id) => live.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [views]);

  // 详情是「录制详情」抽屉，默认不展开；点行不再自动打开。
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // 分屏观看多选：勾在缩略图右上角，最多 4 个在播房间。
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
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
  useEffect(() => {
    if (!selectedId) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setSelectedId(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedId]);

  const [lines, setLines] = useState<LineInfo[] | null>(null);
  const [lineBusy, setLineBusy] = useState(false);
  const [postBusy, setPostBusy] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const [rooms, history] = await Promise.all([roomsList(), sessionsList(undefined, 200)]);
      setViews(rooms);
      setSessions(history);
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
    setMenuId(null);
  }, [selectedId]);

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
    // 默认按主播名称（中文按拼音、数字按数值）；点表头再叠加状态排序。
    const label = (view: RoomView) => view.room.hostName || view.room.title || view.room.roomKey;
    const byName = (a: RoomView, b: RoomView) => label(a).localeCompare(label(b), undefined, { numeric: true });
    if (!sort) return list.sort(byName);
    const rank = (view: RoomView) =>
      sort.key === "live" ? (view.room.isLive ? 1 : 0) : view.recording ? 1 : 0;
    return list.sort((a, b) => {
      const diff = rank(b) - rank(a);
      return (sort.dir === "asc" ? diff : -diff) || byName(a, b);
    });
  }, [views, query, platform, onlyRecording, sort]);

  // 分屏入口：顺序沿用当前表格顺序（筛选 + 排序），余下的按房间列表补齐。
  const pickedIds = useMemo(() => {
    const ordered = shown.map((view) => view.room.id).filter((id) => picked.has(id));
    for (const view of views) {
      if (picked.has(view.room.id) && !ordered.includes(view.room.id)) ordered.push(view.room.id);
    }
    return ordered.slice(0, 4);
  }, [shown, views, picked]);

  // 按选中数量给出可选版式：1 单画面、2 左右/上下、3 上大下二/左大右二、4 田字。
  const splitModes: { kind: LayoutKind; label: string }[] =
    pickedIds.length <= 1
      ? [{ kind: "single", label: t("monitor.modeSingle") }]
      : pickedIds.length === 2
        ? [
            { kind: "row", label: t("monitor.modeRow") },
            { kind: "col", label: t("monitor.modeCol") },
          ]
        : pickedIds.length === 3
          ? [
              { kind: "top", label: t("monitor.modeTop") },
              { kind: "left", label: t("monitor.modeLeft") },
            ]
          : [{ kind: "quad", label: t("monitor.modeQuad") }];

  /** 点一次升序（在线/录制中优先），再点降序，第三次取消。 */
  const toggleSort = (key: SortKey) =>
    setSort((prev) =>
      prev?.key !== key ? { key, dir: "asc" } : prev.dir === "asc" ? { key, dir: "desc" } : null,
    );

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
      await roomAdd(value, addMonitor, addAutoRecord);
      setInput("");
      setAddMonitor(true);
      setAddAutoRecord(true);
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
    // 移除只隐藏房间，录像历史仍按主播留在媒体库，明确告知免得以为被删了。
    setNotice({ text: t("monitor.removedKept"), error: false });
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
      const result: PostResult = await roomMergeSessions(roomId);
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

  const platformOptions = PLATFORMS.map((key) => ({
    key,
    label: t("platformName." + key),
    count: counts[key] ?? 0,
  })).filter((item) => item.count > 0);

  const platformLabel = platform === "all" ? t("monitor.filterPlatform") : t("platformName." + platform);

  const statusText = (view: RoomView) =>
    view.recording
      ? t("monitor.status.recording")
      : view.room.monitorPaused
        ? t("monitor.statusPaused")
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

  const togglePick = (view: RoomView) => {
    const room = view.room;
    if (!room.isLive) return;
    if (picked.has(room.id)) {
      const next = new Set(picked);
      next.delete(room.id);
      setPicked(next);
      return;
    }
    if (picked.size >= 4) {
      setNotice({ text: t("monitor.maxSelection"), error: false });
      return;
    }
    setPicked(new Set(picked).add(room.id));
  };

  const renderRow = (view: RoomView) => {
    const room = view.room;
    const isRecording = Boolean(view.recording);
    const isSelected = selectedId === room.id;
    const isPicked = picked.has(room.id);
    const busy = busyId === room.id;
    const elapsed = view.recording ? now / 1000 - view.recording.startedAt : 0;
    const last = lastByRoom.get(room.id);
    const startedAt = view.recording ? view.recording.startedAt : last?.startedAt;
    const durationMs = view.recording ? now - view.recording.startedAt * 1000 : (last?.durationMs ?? 0);
    return (
      <div
        key={room.id}
        style={{ gridTemplateColumns: COLUMNS, minHeight: ROW_H }}
        className={cx(
          "grid cursor-default items-center gap-x-3 border-b border-line px-5 text-left transition-colors",
          isPicked ? "bg-accent-soft" : isSelected ? "bg-raised/60" : "hover:bg-raised/70",
          busy ? "opacity-60" : "",
        )}
      >
        <div className="relative" style={{ width: THUMB_W, height: THUMB_H }}>
          <Thumb
            room={room}
            w={THUMB_W}
            h={THUMB_H}
            recording={isRecording}
            lastAt={last ? last.startedAt : null}
            locale={locale}
          />
          {room.isLive ? (
            <button
              type="button"
              onClick={() => togglePick(view)}
              aria-label={t("monitor.selectHint")}
              aria-pressed={isPicked}
              className={cx(
                "focus-ring absolute right-1 top-1 flex h-[18px] w-[18px] items-center justify-center rounded-[4px] border transition-colors",
                isPicked
                  ? "border-accent bg-accent text-accent-ink"
                  : "border-white/75 bg-black/40 text-transparent hover:border-white hover:bg-black/65",
              )}
            >
              <svg width="10" height="10" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden>
                <path d="M2.2 6.3 4.8 8.9 9.8 3.4" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </button>
          ) : null}
        </div>
        <span className={cx("truncate text-[12.5px]", room.isLive && !room.monitorPaused ? "text-ink-soft" : "text-faint")}>
          {room.monitorPaused
            ? t("monitor.statusPaused")
            : room.isLive
              ? t("monitor.statusLive")
              : t("monitor.statusOffline")}
        </span>
        <span
          className={cx("truncate text-[12.5px]", isRecording ? "font-medium text-rec" : "text-faint")}
          title={isRecording ? statusText(view) : undefined}
        >
          {isRecording ? t("monitor.statusRecording") : t("monitor.statusIdle")}
        </span>
        <span className="truncate text-[12.5px] text-ink-soft">{t("platformName." + room.platform)}</span>
        <span className="flex min-w-0 items-center gap-2">
          <Avatar room={room} size={26} />
          <span className="min-w-0">
            <span className="block truncate text-[13.5px] font-medium leading-5 text-ink" title={room.hostName || room.title}>
              {room.hostName || room.title || room.roomKey}
            </span>
            <span className="mt-0.5 block truncate text-[11.5px] leading-4 text-muted" title={room.title}>
              {room.title || room.roomKey}
            </span>
          </span>
        </span>
        <span className="truncate text-[12.5px] text-muted">
          {t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) })}
        </span>
        <span className="tnum text-right text-[12.5px] leading-4 text-ink-soft">
          {startedAt ? (
            <>
              <span className="block text-[11px] text-muted">{datetime(startedAt)}</span>
              <span className={cx("block", isRecording ? "text-rec" : "")}>
                {isRecording ? clock(elapsed) : clockMs(durationMs)}
              </span>
            </>
          ) : (
            "—"
          )}
        </span>
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
              <div className="absolute right-0 top-9 z-40 w-[196px] overflow-hidden rounded-[8px] border border-line bg-surface py-1 shadow-pop animate-pop">
                <button
                  type="button"
                  onClick={() => {
                    setMenuId(null);
                    setSelectedId(room.id);
                  }}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised"
                >
                  {t("monitor.recordDetail")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setMenuId(null);
                    void run(room.id, () => roomRefresh(room.id));
                  }}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised"
                >
                  {t("monitor.refreshInfo")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setMenuId(null);
                    setNotice({ text: t("monitor.roomSettingsSoon"), error: false });
                  }}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised"
                >
                  {t("monitor.roomSettings")}
                </button>
                <div className="my-1 border-t border-line" />
                <button
                  type="button"
                  onClick={() => {
                    setMenuId(null);
                    void run(room.id, () => roomSetMonitor(room.id, !room.monitorPaused));
                  }}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised"
                >
                  {room.monitorPaused ? t("monitor.resumeMonitor") : t("monitor.pauseMonitor")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setMenuId(null);
                    void run(room.id, () => roomSetAutoRecord(room.id, !room.autoRecord));
                  }}
                  className="flex h-8 w-full items-center px-3 text-left text-[12.5px] text-ink-soft transition-colors hover:bg-raised"
                >
                  {room.autoRecord ? t("monitor.autoRecordOff") : t("monitor.autoRecordOn")}
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
                <div className="my-1 border-t border-line" />
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
    return createPortal(
      <div className="fixed inset-0 z-50 flex justify-end">
        <button
          type="button"
          aria-label={t("monitor.closeDetail")}
          onClick={() => setSelectedId(null)}
          className="absolute inset-0 animate-fade-in bg-black/35 backdrop-blur-[2px]"
        />
        <aside className="relative flex h-full w-[min(440px,94vw)] animate-sheet flex-col border-l border-line bg-surface shadow-2xl">
          <div className="flex items-start justify-between gap-3 border-b border-line px-5 py-4">
            <div className="min-w-0">
              <h2 className="text-[14px] font-semibold text-ink">{t("monitor.recordDetail")}</h2>
              <p className="mt-0.5 truncate text-[11px] leading-4 text-muted">{room.hostName || room.roomKey}</p>
            </div>
            <button
              type="button"
              onClick={() => setSelectedId(null)}
              aria-label={t("monitor.closeDetail")}
              className="focus-ring -mr-1 flex h-7 w-7 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
                <path d="M2.5 2.5 9.5 9.5M9.5 2.5 2.5 9.5" strokeLinecap="round" />
              </svg>
            </button>
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-5 py-4">
            <button
              type="button"
              onClick={() => void openWatch(room.id)}
              title={t("monitor.watch")}
              className="focus-ring block w-full cursor-pointer overflow-hidden rounded-[7px]"
            >
              <div className="aspect-video w-full">
                <Thumb
                  room={room}
                  w={368}
                  h={207}
                  fluid
                  recording={isRecording}
                  lastAt={last ? last.startedAt : null}
                  locale={locale}
                />
              </div>
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
                { label: t("monitor.lastRecorded"), value: last ? datetime(last.startedAt) : "—" },
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
                <SelectMenu
                  value={String(room.recordQn)}
                  size="md"
                  disabled={busy}
                  onChange={(next) => void run(room.id, () => roomSetQuality(room.id, Number(next), room.viewQn))}
                  options={QUALITIES.map((qn) => ({ value: String(qn), label: t("monitor.qn." + qn) }))}
                  className="flex-1 justify-between"
                />
              </div>
              <div className="flex items-center gap-2">
                <span className="w-[62px] shrink-0 text-[12.5px] text-muted">{t("monitor.line")}</span>
                <SelectMenu
                  value={room.recordLine ?? ""}
                  size="md"
                  disabled={lineBusy || busy}
                  onChange={(next) => void run(room.id, () => roomSetLine(room.id, next || null))}
                  options={lineOptions}
                  className="flex-1 justify-between"
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
                  return (
                    <div key={session.id} className="border-t border-line py-2 first:border-t-0">
                      <div className="flex items-center justify-between text-[12px]">
                        <span className="tnum text-ink-soft">
                          {recent(session.startedAt)} ·{" "}
                          {session.status === "recording"
                            ? clock(now / 1000 - session.startedAt)
                            : clockMs(session.durationMs)}
                        </span>
                        <span className="tnum text-muted">{size(session.fileSize)}</span>
                      </div>
                      <div className="mt-1 truncate text-[11.5px] text-muted">{sessionStatus(session)}</div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
          <div className="flex shrink-0 items-center justify-end gap-2 border-t border-line px-5 py-3">
            <Button variant="secondary" onClick={() => setSelectedId(null)}>
              {t("monitor.close")}
            </Button>
          </div>
        </aside>
      </div>,
      document.body,
    );
  };
  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-3 px-5 pb-3 pt-4">
        <div className="min-w-0">
          <h1 className="text-[21px] font-semibold leading-7 tracking-[-0.01em] text-ink">{t("monitor.title")}</h1>

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
          <Button
            size="lg"
            variant="primary"
            onClick={() => {
              setAddOpen((open) => !open);
              setAddMonitor(true);
              setAddAutoRecord(true);
            }}
          >
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
            <div className="mt-3 divide-y divide-line overflow-hidden rounded-[8px] border border-line">
              <div className="flex items-center gap-3 px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] text-ink">{t("monitor.addMonitor")}</div>
                  <p className="mt-0.5 text-[11px] leading-4 text-faint">
                    {t("monitor.addMonitorHint")}
                  </p>
                </div>
                <Switch
                  checked={addMonitor}
                  onChange={(next) => {
                    setAddMonitor(next);
                    // 不监控就不会自动录，避免留一个看不见的开关。
                    setAddAutoRecord(next);
                  }}
                  label={t("monitor.addMonitor")}
                />
              </div>
              <div className="flex items-center gap-3 px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] text-ink">{t("monitor.addAutoRecord")}</div>
                  <p className="mt-0.5 text-[11px] leading-4 text-faint">
                    {addMonitor
                      ? t("monitor.addAutoRecordHint")
                      : t("monitor.addAutoRecordDisabled")}
                  </p>
                </div>
                <Switch
                  checked={addAutoRecord}
                  disabled={!addMonitor}
                  onChange={setAddAutoRecord}
                  label={t("monitor.addAutoRecord")}
                />
              </div>
            </div>
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
        <button
          type="button"
          onClick={() => {
            setPlatform("all");
          }}
          className={cx(
            "focus-ring rounded-ctl px-2.5 py-1.5 text-[12.5px] transition-colors",
            platform === "all"
              ? "bg-accent-soft font-medium text-accent-text"
              : "text-muted hover:bg-raised hover:text-ink-soft",
          )}
        >
          {t("monitor.filterAll")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{counts.all ?? 0}</span>
        </button>
        <SelectMenu
          variant="bare"
          align="start"
          active={platform !== "all"}
          value={platform}
          onChange={setPlatform}
          placeholder={platformLabel}
          disabled={platformOptions.length === 0}
          options={platformOptions.map((item) => ({
            value: item.key,
            label: item.label,
            hint: String(item.count),
          }))}
        />
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
              <SortHead
                label={t("monitor.colLive")}
                active={sort?.key === "live"}
                dir={sort?.dir ?? "asc"}
                onClick={() => toggleSort("live")}
              />
              <SortHead
                label={t("monitor.colRecord")}
                active={sort?.key === "record"}
                dir={sort?.dir ?? "asc"}
                onClick={() => toggleSort("record")}
              />
              <span>{t("monitor.colPlatform")}</span>
              <span>{t("monitor.colHost")}</span>
              <span>{t("monitor.colQuality")}</span>
              <span className="text-right">{t("monitor.colDuration")}</span>
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

      </div>

      {picked.size > 0 ? (
        <div className="absolute bottom-6 left-1/2 z-40 flex -translate-x-1/2 items-center gap-2 rounded-[10px] border border-line bg-surface/95 py-1.5 pl-3.5 pr-2 shadow-pop backdrop-blur">
          <span className="whitespace-nowrap text-[12px] text-ink">
            {t("monitor.selectedCount", { n: picked.size })}
          </span>
          <Button size="sm" variant="ghost" onClick={() => setPicked(new Set())}>
            {t("monitor.clearSelection")}
          </Button>
          <span className="h-4 w-px shrink-0 bg-line" />
          <span className="whitespace-nowrap text-[11px] text-faint">{t("monitor.splitMode")}</span>
          {splitModes.map((mode) => (
            <Button
              key={mode.kind}
              size="sm"
              variant="secondary"
              onClick={() => onOpenWatch?.(pickedIds, mode.kind)}
            >
              <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
                <LayoutGlyph kind={mode.kind} />
                {mode.label}
              </span>
            </Button>
          ))}
        </div>
      ) : null}

      {selected ? renderPanel(selected) : null}
    </div>
  );
}