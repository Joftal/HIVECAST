import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Button, Switch, cx } from "../../components/ui/Controls";
import { Checkbox } from "../../components/arc/checkbox/checkbox";
import { FloatingButtonGroup } from "../../components/arc/floating-button-group/floating-button-group";
import { useNotify } from "../../lib/notify";
import { DropdownMenu } from "../../components/arc/dropdown-menu/dropdown-menu";
import { HoldToConfirm } from "../../components/arc/hold-to-confirm/hold-to-confirm";
import { SelectMenu } from "../../components/ui/SelectMenu";
import PageEmpty from "../../components/ui/PageEmpty";
import EmptyArt from "../../components/ui/EmptyArt";
import InfoTip from "../../components/ui/InfoTip";
import TagBadge from "../../components/ui/TagBadge";
import { PageChip, PageSearch, PageShell, PageToolbar } from "../../components/ui/PageShell";
import { LayoutGlyph, type LayoutKind } from "../../components/ui/LayoutGlyph";
import AddRoomDialog from "./AddRoomDialog";
import ImportFollowsDialog from "./ImportFollowsDialog";
import RoomRules from "./RoomRules";
import InlineNote from "./InlineNote";
import InlineTags from "./InlineTags";
import { parseTags } from "./roomRuleInput";
import {
  isTauri,
  mediaReveal,
  monitorStatus,
  onRoomsChanged,
  openUrl,
  recordStart,
  recordStop,
  recordingUsage,
  roomStreamOptions,
  roomRefresh,
  roomRemove,
  roomSetAutoRecord,
  roomSetMonitor,
  roomSetLine,
  roomSetQuality,
  roomsList,
  sessionSummary,
  viewOpen,
  type LineInfo,
  type MonitorStatus,
  type QualityInfo,
  type RecordingUsage,
  type RoomView,
  type SessionSummary,
} from "../../lib/ipc";
import { codecLabel, recordingCodecSupported, videoCodecSupported } from "../../lib/codecSupport";

const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"];
const PLATFORM_DOT: Record<string, string> = {
  bilibili: "#22a7f0",
  douyu: "#ff7a1a",
  huya: "#f5a623",
  douyin: "#ff2d6f",
};

/** 全表只有一套尺寸：行 76、缩略图 120x68，右侧详情固定 320。*/
const COLUMNS = "120px 58px 80px 58px minmax(0,1fr) 104px 104px 74px 120px 140px";
// 搜索时把平台中文名与常见别名一起算进去，输入「B站」也能命中。
const PLATFORM_ALIASES: Record<string, string> = {
  bilibili: "b站 bilibili 哔哩哔哩",
  douyu: "斗鱼 douyu",
  huya: "虎牙 huya",
  douyin: "抖音 douyin",
};
// 行内动作组比 Arc 默认 sm 再紧一档：英文 Watch/Record 也能放进 148px 操作列。
const ROW_ACTION_GROUP_STYLE = {
  "--fbg-h": "28px",
  "--fbg-pad": "2px",
  "--fbg-gap": "2px",
  "--fbg-pad-x": "8px",
  "--fbg-outer": "8px",
  "--fbg-radius": "6px",
} as CSSProperties;
const ROW_H = 76;
const THUMB_W = 120;
const THUMB_H = 68;

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

/* 录制时钟共享一个 1s tick：只有挂载中的 LiveDuration 重渲染，页面本身不 tick。*/
let liveTick = Date.now();
let liveTimer: number | null = null;
const liveListeners = new Set<() => void>();
function subscribeLiveTick(listener: () => void) {
  liveTick = Date.now();
  liveListeners.add(listener);
  if (liveTimer === null) {
    liveTimer = window.setInterval(() => {
      liveTick = Date.now();
      liveListeners.forEach((notify) => notify());
    }, 1000);
  }
  return () => {
    liveListeners.delete(listener);
    if (liveListeners.size === 0 && liveTimer !== null) {
      window.clearInterval(liveTimer);
      liveTimer = null;
    }
  };
}
function getLiveTick() {
  return liveTick;
}
function LiveDuration({ startedAt }: { startedAt: number }) {
  const now = useSyncExternalStore(subscribeLiveTick, getLiveTick, getLiveTick);
  return <>{clock(now / 1000 - startedAt)}</>;
}

/** 当前录制占用：独立轮询，避免每秒触发整页/整表重渲染。*/
function RecordingUsageRows({ sessionId }: { sessionId: string }) {
  const { t } = useTranslation();
  const [usage, setUsage] = useState<RecordingUsage | null>(null);
  const [estimatedSecs, setEstimatedSecs] = useState<number | null>(null);

  useEffect(() => {
    setUsage(null);
    setEstimatedSecs(null);
    if (!isTauri()) return;
    let alive = true;
    let previous: { bytes: number; at: number } | null = null;
    const load = () => {
      void recordingUsage(sessionId)
        .then((next) => {
          if (!alive) return;
          const now = Date.now();
          if (previous && next.bytes > previous.bytes) {
            const elapsed = Math.max(1, (now - previous.at) / 1000);
            const rate = (next.bytes - previous.bytes) / elapsed;
            if (rate > 0 && next.diskFree > 0) setEstimatedSecs(next.diskFree / rate);
          }
          previous = { bytes: next.bytes, at: now };
          setUsage(next);
        })
        .catch(() => undefined);
    };
    load();
    const timer = window.setInterval(load, 3000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [sessionId]);

  return (
    <>
      <div className="flex items-center gap-2 border-t border-rec/15 px-3 py-2 text-[11px]">
        <span className="shrink-0 text-faint">{t("monitor.written")}</span>
        <span className="tnum text-ink-soft">{usage ? size(usage.bytes) : "—"}</span>
        <span className="ml-auto shrink-0 text-faint">{t("monitor.diskFree")}</span>
        <span className="tnum text-ink-soft">{usage ? size(usage.diskFree) : "—"}</span>
      </div>
      {estimatedSecs !== null ? (
        <div className="flex items-center gap-2 border-t border-rec/15 px-3 py-2 text-[11px]">
          <span className="shrink-0 text-faint">{t("monitor.estimatedTime")}</span>
          <span className="tnum ml-auto text-ink-soft">{clock(estimatedSecs)}</span>
        </div>
      ) : null}
    </>
  );
}

/** 监控排期：独立轮询，避免 1s 一次的状态更新带动整个监控表。*/
function MonitorDiagnostics({
  roomId,
  roomLabel,
  lastCheckedAt,
  error,
  onCopy,
}: {
  roomId: string;
  roomLabel: string;
  lastCheckedAt: number | null;
  error: string | null;
  onCopy: (text: string) => void;
}) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<MonitorStatus | null>(null);

  useEffect(() => {
    setStatus(null);
    if (!isTauri()) return;
    let alive = true;
    const load = () => {
      void monitorStatus(roomId)
        .then((next) => {
          if (alive) setStatus(next);
        })
        .catch(() => undefined);
    };
    load();
    const timer = window.setInterval(load, 1000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [roomId]);

  const nextCheck = status
    ? status.inflight
      ? t("monitor.checking")
      : t("monitor.inSeconds", { n: Math.max(0, status.nextCheckInSecs) })
    : "—";
  const outcome = status?.lastOutcome
    ? t("monitor.outcome." + status.lastOutcome, { defaultValue: status.lastOutcome })
    : "—";
  const diagnosis = [
    roomLabel,
    `${t("monitor.lastChecked")}: ${lastCheckedAt ? new Date(lastCheckedAt * 1000).toLocaleString() : "—"}`,
    `${t("monitor.nextCheck")}: ${nextCheck}`,
    `${t("monitor.failures")}: ${status?.errorCount ?? 0}`,
    `${t("monitor.lastError")}: ${error ?? "—"}`,
  ].join("\n");

  return (
    <>
      <div className="flex items-center gap-3 border-t border-line px-3 py-2">
        <span className="shrink-0 text-[11.5px] text-faint">{t("monitor.nextCheck")}</span>
        <span className="min-w-0 flex-1 truncate text-right text-[12px] text-ink-soft">{nextCheck}</span>
      </div>
      <div className="flex items-center gap-3 border-t border-line px-3 py-2">
        <span className="shrink-0 text-[11.5px] text-faint">{t("monitor.failures")}</span>
        <span className="min-w-0 flex-1 truncate text-right text-[12px] text-ink-soft">
          {status ? status.errorCount : "—"}
          {status?.backoff ? <span className="text-warn"> · {t("monitor.backoff")}</span> : null}
        </span>
      </div>
      <div className="flex items-center gap-3 border-t border-line px-3 py-2">
        <span className="shrink-0 text-[11.5px] text-faint">{t("monitor.lastOutcome")}</span>
        <span className="min-w-0 flex-1 truncate text-right text-[12px] text-ink-soft">{outcome}</span>
      </div>
      {error ? (
        <div className="flex items-start gap-2 border-t border-line px-3 py-2">
          <span className="shrink-0 pt-0.5 text-[11.5px] text-faint">{t("monitor.lastError")}</span>
          <span
            className="line-clamp-2 min-w-0 flex-1 break-words text-right text-[11.5px] leading-4 text-warn"
            title={error}
          >
            {error}
          </span>
          <button
            type="button"
            onClick={() => onCopy(error)}
            aria-label={t("common.copy")}
            title={t("common.copy")}
            className="focus-ring flex h-6 w-6 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
          >
            <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
              <rect x="5.2" y="5.2" width="8.3" height="8.3" rx="1.4" />
              <path d="M10.8 5.2V3.8a1.4 1.4 0 0 0-1.4-1.4H3.8a1.4 1.4 0 0 0-1.4 1.4v5.6a1.4 1.4 0 0 0 1.4 1.4h1.4" />
            </svg>
          </button>
        </div>
      ) : null}
      <div className="flex justify-end border-t border-line px-3 py-2">
        <button
          type="button"
          onClick={() => onCopy(diagnosis)}
          className="focus-ring rounded-ctl px-1.5 py-0.5 text-[11.5px] text-accent-text transition-colors hover:bg-accent-soft"
        >
          {t("monitor.copyDiagnostics")}
        </button>
      </div>
    </>
  );
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

/** 上次录制开始时间统一用完整时间戳：2026-11-06 00:02:28。*/
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

/** 最近录制列只放得下 5 个字：今天给时分，更早给月日。*/
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

/** 最近检查时间用相对时间；Intl 自带「刚刚 / 12 秒前」等本地化。*/
function timeAgo(seconds: number, locale: string): string {
  if (!seconds) return "—";
  const formatter = new Intl.RelativeTimeFormat(locale === "en-US" ? "en-US" : "zh-CN", {
    numeric: "auto",
  });
  const diff = Math.round(seconds - Date.now() / 1000);
  const abs = Math.abs(diff);
  if (abs < 60) return formatter.format(diff, "second");
  if (abs < 3600) return formatter.format(Math.round(diff / 60), "minute");
  if (abs < 86400) return formatter.format(Math.round(diff / 3600), "hour");
  return formatter.format(Math.round(diff / 86400), "day");
}

/** 逗号分隔的本地标签；列表筛选与抽屉共用同一套解析。*/
function compactNumber(value: number, locale: string): string {
  if (!value) return "—";
  return new Intl.NumberFormat(locale === "en-US" ? "en-US" : "zh-CN", {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(value);
}

/** 头像：没有 avatarUrl 时用平台色 + 首字母占位。*/
function Avatar({ room, size: px }: { room: RoomView["room"]; size: number }) {
  const src = room.avatarUrl;
  return src ? (
    <img
      src={src}
      alt=""
      referrerPolicy="no-referrer"
      loading="lazy"
      decoding="async"
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
        <img
          src={src}
          alt=""
          className="h-full w-full object-cover"
          referrerPolicy="no-referrer"
          loading="lazy"
          decoding="async"
        />
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
/** 可排序表头：一次升序、再点降序、第三次取消。*/
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
  onOpenMedia,
  onOpenTasks,
}: {
  onOpenWatch?: (roomIds?: string[], kind?: LayoutKind) => void;
  onOpenMedia?: (roomId: string) => void;
  onOpenTasks?: (roomId: string) => void;
}) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "en-US" ? "en-US" : "zh-CN";

  const [views, setViews] = useState<RoomView[]>([]);
  const [drawerSummary, setDrawerSummary] = useState<SessionSummary | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const reloadTimer = useRef<number | null>(null);
  const loadInFlight = useRef(false);
  const loadQueued = useRef(false);

  const [queryInput, setQueryInput] = useState("");
  const [query, setQuery] = useState("");
  const [platform, setPlatform] = useState("all");
  const [tag, setTag] = useState("all");
  const [sort, setSort] = useState<{ key: SortKey; dir: "asc" | "desc" } | null>(null);
  const [onlyRecording, setOnlyRecording] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<"watch" | "record" | null>(null);
  const notify = useNotify();
  // rooms://changed 可能连续触发；后台加载失败只提示一次。
  const loadErrorShown = useRef(false);
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
  // 同一个抽屉既展示录制详情，也承载行菜单的「直播间设置」。
  const [detailMode, setDetailMode] = useState<"detail" | "settings">("detail");
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selectedId;
  // 分屏观看多选：勾在缩略图右上角，最多 4 个在播房间。
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
  useEffect(() => {
    if (!selectedId) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setSelectedId(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedId]);

  const [lines, setLines] = useState<LineInfo[] | null>(null);
  const [qualities, setQualities] = useState<QualityInfo[] | null>(null);
  const [optionsKey, setOptionsKey] = useState<{ roomId: string; qn: number } | null>(null);
  const [lineBusy, setLineBusy] = useState(false);
  const [removeDone, setRemoveDone] = useState(false);

  // 详情抽屉不再展示录制列表，只为「已录」汇总取一次 totalBytes。
  const loadDrawerSummary = useCallback(async (roomId: string) => {
    if (!isTauri()) {
      setDrawerSummary(null);
      return;
    }
    try {
      const summary = await sessionSummary(roomId);
      if (selectedIdRef.current === roomId) setDrawerSummary(summary);
    } catch (err) {
      if (selectedIdRef.current === roomId) notify.error(err);
    }
  }, [notify]);

  // 单飞：同一时刻只有一条 rooms_list 在路上；期间的刷新请求合并成一次尾随刷新。
  const load = useCallback(async () => {
    if (!isTauri()) {
      setLoaded(true);
      return;
    }
    if (loadInFlight.current) {
      loadQueued.current = true;
      return;
    }
    loadInFlight.current = true;
    try {
      do {
        loadQueued.current = false;
        try {
          setViews(await roomsList());
          loadErrorShown.current = false;
          setLoadFailed(false);
        } catch (err) {
          setLoadFailed(true);
          if (!loadErrorShown.current) {
            loadErrorShown.current = true;
            notify.error(err);
          }
        }
      } while (loadQueued.current);
    } finally {
      loadInFlight.current = false;
      setLoaded(true);
    }
  }, [notify]);

  const retryLoad = () => {
    setLoadFailed(false);
    setLoaded(false);
    void load();
  };

  // rooms://changed 会成串到达：200ms 合并；load 自身负责单飞与尾随刷新。
  const requestReload = useCallback(() => {
    if (reloadTimer.current) window.clearTimeout(reloadTimer.current);
    reloadTimer.current = window.setTimeout(() => {
      reloadTimer.current = null;
      void load();
    }, 200);
  }, [load]);

  useEffect(() => {
    if (!isTauri()) {
      setLoaded(true);
      return;
    }
    void load();
    let unlisten: (() => void) | undefined;
    let alive = true;
    onRoomsChanged(() => {
      requestReload();
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      if (reloadTimer.current) window.clearTimeout(reloadTimer.current);
      if (unlisten) unlisten();
    };
  }, [load, requestReload]);

  useEffect(() => {
    setLines(null);
    setQualities(null);
    setOptionsKey(null);
    setRemoveDone(false);
    setDrawerSummary(null);
    if (!selectedId || !isTauri()) return;
    void loadDrawerSummary(selectedId);
  }, [selectedId, loadDrawerSummary]);

  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(queryInput.trim()), 120);
    return () => window.clearTimeout(timer);
  }, [queryInput]);

  const changeQuery = (next: string) => {
    setQueryInput(next);
    if (!next) setQuery("");
  };

  const collator = useMemo(
    () => new Intl.Collator(locale, { numeric: true }),
    [locale],
  );

  const counts = useMemo(() => {
    const map: Record<string, number> = { all: views.length };
    for (const view of views) map[view.room.platform] = (map[view.room.platform] ?? 0) + 1;
    return map;
  }, [views]);
  const recordingCount = useMemo(() => views.filter((view) => view.recording).length, [views]);

  // 标签筛选：按出现次数倒序，同次数按名称；选项里保留计数。
  const tagOptions = useMemo(() => {
    const countsByTag = new Map<string, number>();
    for (const view of views) {
      for (const value of parseTags(view.room.tags)) {
        countsByTag.set(value, (countsByTag.get(value) ?? 0) + 1);
      }
    }
    return [...countsByTag.entries()]
      .sort((a, b) => b[1] - a[1] || collator.compare(a[0], b[0]))
      .map(([value, count]) => ({ value, label: value, count }));
  }, [views, collator]);

  const lastByRoom = useMemo(() => {
    const map = new Map<string, { id: string; startedAt: number; durationMs: number }>();
    for (const view of views) {
      const room = view.room;
      if (room.lastSessionId !== null && room.lastSessionStartedAt !== null) {
        map.set(room.id, {
          id: room.lastSessionId,
          startedAt: room.lastSessionStartedAt,
          durationMs: room.lastSessionDurationMs,
        });
      }
    }
    return map;
  }, [views]);

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const list = views.filter((view) => {
      if (platform !== "all" && view.room.platform !== platform) return false;
      if (tag !== "all" && !parseTags(view.room.tags).includes(tag)) return false;
      if (onlyRecording && !view.recording) return false;
      if (!needle) return true;
      return [
        view.room.title,
        view.room.hostName,
        view.room.roomKey,
        view.room.note,
        view.room.tags,
        view.room.areaName ?? "",
        view.room.canonicalUrl,
        t("platformName." + view.room.platform, { defaultValue: view.room.platform }),
        PLATFORM_ALIASES[view.room.platform] ?? "",
      ]
        .join(" ")
        .toLowerCase()
        .includes(needle);
    });
    // 默认按主播名称（中文按拼音、数字按数值）；点表头再叠加状态排序。
    const label = (view: RoomView) => view.room.hostName || view.room.title || view.room.roomKey;
    const byName = (a: RoomView, b: RoomView) => collator.compare(label(a), label(b));
    if (!sort) return list.sort(byName);
    const rank = (view: RoomView) =>
      sort.key === "live" ? (view.room.isLive ? 1 : 0) : view.recording ? 1 : 0;
    return list.sort((a, b) => {
      const diff = rank(b) - rank(a);
      return (sort.dir === "asc" ? diff : -diff) || byName(a, b);
    });
  }, [views, query, platform, tag, onlyRecording, sort, collator, t]);

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

  /** 点一次升序（在线/录制中优先），再点降序，第三次取消。*/
  const toggleSort = (key: SortKey) =>
    setSort((prev) =>
      prev?.key !== key ? { key, dir: "asc" } : prev.dir === "asc" ? { key, dir: "desc" } : null,
    );

  const selected = useMemo(() => views.find((view) => view.room.id === selectedId) ?? null, [views, selectedId]);
  const recordedLabel =
    drawerSummary && drawerSummary.total > 0
      ? `${size(drawerSummary.totalBytes)} · ${t("monitor.sessionCount", { n: drawerSummary.total })}`
      : "—";

  const run = useCallback(
    async (id: string, action: () => Promise<unknown>, kind: "record" | null = null) => {
      setBusyId(id);
      setBusyAction(kind);
      try {
        await action();
        await load();
        if (selectedId === id) await loadDrawerSummary(id);
        return true;
      } catch (err) {
        notify.error(err);
        return false;
      } finally {
        setBusyId(null);
        setBusyAction(null);
      }
    },
    [load, loadDrawerSummary, notify, selectedId],
  );

  const openWatch = async (roomId: string) => {
    setBusyId(roomId);
    setBusyAction("watch");
    try {
      await viewOpen(roomId);
      onOpenWatch?.();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusyId(null);
      setBusyAction(null);
    }
  };

  const revealSession = async (sessionId: string) => {
    try {
      await mediaReveal(sessionId);
    } catch (err) {
      notify.error(err);
    }
  };

  const reveal = async (roomId: string) => {
    const last = lastByRoom.get(roomId);
    if (last) await revealSession(last.id);
  };

  const refreshRoom = (roomId: string) =>
    void run(roomId, async () => {
      await roomRefresh(roomId);
      notify.success(t("monitor.refreshed"));
    });

  const copyText = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      notify.success(t("common.copied"));
    } catch (err) {
      notify.error(err);
    }
  };

  const openRoomUrl = async (url: string) => {
    try {
      await openUrl(url);
    } catch (err) {
      notify.error(err);
    }
  };

  const removeRoom = async (room: RoomView["room"]) => {
    const ok = await run(room.id, () => roomRemove(room.id));
    // 移除只隐藏房间，录像历史仍按主播留在媒体库，明确告知免得以为被删了。
    if (ok) notify.info(t("monitor.removedKept"));
    return ok;
  };

  const loadOptions = async (roomId: string, qn: number, force = false) => {
    if (!force && optionsKey?.roomId === roomId && optionsKey.qn === qn && qualities) return;
    setLineBusy(true);
    try {
      const next = await roomStreamOptions(roomId, qn);
      if (selectedIdRef.current !== roomId) return;
      setQualities(next.qualities);
      setLines(next.lines);
      setOptionsKey({ roomId, qn });
    } catch (err) {
      setQualities(null);
      setLines([]);
      notify.error(err);
    } finally {
      setLineBusy(false);
    }
  };

  const qualityOptionsFor = (roomId: string, qn: number, mode: "record" | "view") => {
    const auto = { value: "0", label: t("monitor.qn.0") };
    if (!qualities || optionsKey?.roomId !== roomId) {
      // 能力清单未到时只给「自动」和当前档位，不猜平台专有 qn。
      if (qn === 0) return [auto];
      return [auto, { value: String(qn), label: t("monitor.qn." + qn, { defaultValue: String(qn) }) }];
    }
    const options = qualities.map((quality) => {
      const supported =
        mode === "view" ? videoCodecSupported(quality.codec) : recordingCodecSupported(quality.codec);
      return {
        value: String(quality.qn),
        label: quality.label,
        hint: !quality.available
          ? (quality.hint ?? t("watch.qualityUnavailable"))
          : !supported
            ? mode === "view"
              ? t("watch.qualityCodecUnsupported", { codec: codecLabel(quality.codec) })
              : t("monitor.recordingCodecUnsupported", { codec: codecLabel(quality.codec) })
            : (quality.hint ?? undefined),
        disabled: !quality.available || !supported,
      };
    });
    if (qn !== 0 && !options.some((option) => option.value === String(qn))) {
      options.unshift({
        value: String(qn),
        label: t("monitor.qn." + qn, { defaultValue: String(qn) }),
        hint: undefined,
        disabled: false,
      });
    }
    return [auto, ...options];
  };

  const lineOptions = [{ value: "", label: t("monitor.lineAuto") }].concat(
    (lines ?? []).map((line) => ({ value: line.name, label: line.name, hint: line.quality })),
  );

  const platformOptions = PLATFORMS.map((key) => ({
    key,
    label: t("platformName." + key),
    count: counts[key] ?? 0,
  })).filter((item) => item.count > 0);

  const platformLabel = platform === "all" ? t("monitor.filterPlatform") : t("platformName." + platform);
  const tagLabel = tag === "all" ? t("monitor.filterTag") : tag;

  const statusText = (view: RoomView) =>
    view.recording
      ? t("monitor.status.recording")
      : view.room.monitorPaused
        ? t("monitor.statusPaused")
        : view.room.isLive
          ? t("monitor.statusLive")
          : t("monitor.statusOffline");

  const toggleRecording = (view: RoomView) => {
    const isRecording = Boolean(view.recording);
    void run(
      view.room.id,
      () => (isRecording ? recordStop(view.room.id) : recordStart(view.room.id)),
      "record",
    );
  };

  // 抽屉打开时的快捷键：W 观看、R 开录/停录、F 刷新；输入框里不抢键。
  useEffect(() => {
    if (!selectedId) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable='true']")) return;
      const view = views.find((item) => item.room.id === selectedId);
      if (!view) return;
      if (event.key === "w" || event.key === "W") {
        event.preventDefault();
        if (view.room.isLive) void openWatch(view.room.id);
      } else if (event.key === "r" || event.key === "R") {
        event.preventDefault();
        if (view.room.isLive || view.recording) toggleRecording(view);
      } else if (event.key === "f" || event.key === "F") {
        event.preventDefault();
        refreshRoom(view.room.id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedId, views, openWatch, refreshRoom]);

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
      notify.warning(t("monitor.maxSelection"));
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
    const watchBusy = busy && busyAction === "watch";
    const recordBusy = busy && busyAction === "record";
    const last = lastByRoom.get(room.id);
    const startedAt = view.recording ? view.recording.startedAt : last?.startedAt;
    // 录制中优先用活动会话的实际画质：升级/降档只更新 session，
    // room.last_session_quality 要等本场结束才回写，直接用它会一直显示开录时的档位。
    const shownQuality = view.recording?.quality || room.lastSessionQuality;
    const durationMs = last?.durationMs ?? 0;
    return (
      <div
        key={room.id}
        style={{ gridTemplateColumns: COLUMNS, minHeight: ROW_H }}
        className={cx(
          "monitor-row grid cursor-default items-center gap-x-3 border-b border-line px-5 text-left transition-colors",
          isPicked ? "bg-accent-soft" : isSelected ? "bg-raised/60" : "hover:bg-raised/70",
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
            <div className="absolute right-0 top-0">
              <Checkbox
                checked={isPicked}
                onCheckedChange={() => togglePick(view)}
                aria-label={t("monitor.selectHint")}
                tone="overlay"
              />
            </div>
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
        <span className="min-w-0">
          <InlineNote room={room} />
        </span>
        <span className="min-w-0">
          <InlineTags room={room} />
        </span>
        <button
          type="button"
          onClick={() => {
            setDetailMode("settings");
            setSelectedId(room.id);
          }}
          title={`${t("monitor.actualQuality")}: ${shownQuality ?? "—"}\n${t("monitor.targetQuality")}: ${t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) })}`}
          className="focus-ring truncate rounded-ctl text-left text-[12.5px] text-muted transition-colors hover:text-ink"
        >
          {shownQuality ?? t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) })}
        </button>
        <span className="tnum text-right text-[12.5px] leading-4 text-ink-soft">
          {startedAt ? (
            <>
              <span className="block text-[11px] text-muted">{datetime(startedAt)}</span>
              <span className={cx("block", isRecording ? "text-rec" : "")}>
                {view.recording ? (
                  <LiveDuration startedAt={view.recording.startedAt} />
                ) : (
                  clockMs(durationMs)
                )}
              </span>
            </>
          ) : (
            "—"
          )}
        </span>
        <span className="relative flex items-center justify-end gap-1.5">
          <FloatingButtonGroup
            label={t("monitor.rowActions")}
            variant="muted"
            size="sm"
            style={ROW_ACTION_GROUP_STYLE}
            items={[
              {
                id: "watch",
                label: room.isLive ? t("monitor.watch") : t("monitor.watchOfflineShort"),
                reserveLabels: [t("monitor.watch"), t("monitor.watchOfflineShort")],
                tone: "accent",
                disabled: !room.isLive,
                loading: watchBusy,
                onSelect: () => {
                  if (busy) return;
                  void openWatch(room.id);
                },
              },
              {
                id: "record",
                label: isRecording ? t("monitor.stop") : t("monitor.rec"),
                reserveLabels: [t("monitor.rec"), t("monitor.stop")],
                tone: isRecording ? "danger" : "default",
                disabled: !room.isLive && !isRecording,
                loading: recordBusy,
                onSelect: () => {
                  if (busy) return;
                  toggleRecording(view);
                },
              },
            ]}
          />
          <DropdownMenu
            label={t("monitor.more")}
            ariaLabel={t("monitor.more")}
            icon={
              <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
                <circle cx="3" cy="8" r="1.3" />
                <circle cx="8" cy="8" r="1.3" />
                <circle cx="13" cy="8" r="1.3" />
              </svg>
            }
            iconOnly
            variant="bare"
            size="sm"
            align="end"
            onOpenChange={(open) => {
              if (open) setRemoveDone(false);
            }}
            footer={
              <HoldToConfirm
                label={t("monitor.holdRemove")}
                confirmedLabel={t("monitor.removeDone")}
                duration={1400}
                tone="danger"
                confirmed={removeDone}
                disabled={busy}
                className="w-full"
                onConfirm={() => {
                  setRemoveDone(true);
                  void removeRoom(room).then((ok) => {
                    if (!ok) setRemoveDone(false);
                  });
                }}
              />
            }
            items={[
              {
                key: "detail",
                label: t("monitor.recordDetail"),
                onSelect: () => {
                  setDetailMode("detail");
                  setSelectedId(room.id);
                },
              },
              {
                key: "refresh",
                label: t("monitor.refreshInfo"),
                onSelect: () => refreshRoom(room.id),
              },
              {
                key: "settings",
                label: t("monitor.roomSettings"),
                onSelect: () => {
                  setDetailMode("settings");
                  setSelectedId(room.id);
                },
              },
              {
                key: "pause",
                label: room.monitorPaused ? t("monitor.resumeMonitor") : t("monitor.pauseMonitor"),
                separatorBefore: true,
                onSelect: () => void run(room.id, () => roomSetMonitor(room.id, !room.monitorPaused)),
              },
              {
                key: "auto-record",
                label: room.autoRecord ? t("monitor.autoRecordOff") : t("monitor.autoRecordOn"),
                disabled: room.monitorPaused,
                onSelect: () =>
                  void run(room.id, async () => {
                    await roomSetAutoRecord(room.id, !room.autoRecord);
                    notify.success(
                      room.autoRecord ? t("monitor.autoRecordDisabled") : t("monitor.autoRecordEnabled"),
                    );
                  }),
              },
              {
                key: "reveal",
                label: t("monitor.reveal"),
                disabled: !last,
                onSelect: () => void reveal(room.id),
              },
            ]}
          />
        </span>
      </div>
    );
  };
  const renderPanel = (view: RoomView) => {
    const room = view.room;
    const busy = busyId === room.id;
    const last = lastByRoom.get(room.id);
    const recording = view.recording;
    const isRecording = Boolean(recording);
    const recordingFile = recording?.filePath.split(/[\\/]/).pop() ?? "";
    const lastLabel = last
      ? `${datetime(last.startedAt)} · ${last.durationMs ? clockMs(last.durationMs) : "—"}`
      : "—";
    const lastResultLabel = room.lastSessionStatus
      ? [
          t("monitor.status." + room.lastSessionStatus, { defaultValue: room.lastSessionStatus }),
          room.lastSessionReason
            ? t("monitor.reason." + room.lastSessionReason, { defaultValue: room.lastSessionReason })
            : null,
          room.lastSessionFileSize > 0 ? size(room.lastSessionFileSize) : null,
        ]
          .filter(Boolean)
          .join(" · ")
      : "—";
    const roomTags = parseTags(room.tags);
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
              <h2 className="text-[14px] font-semibold text-ink">
                {detailMode === "settings" ? t("monitor.roomSettings") : t("monitor.recordDetail")}
              </h2>
              <p className="mt-0.5 truncate text-[11.5px] leading-4 text-muted">
                {t("platformName." + room.platform)} · {room.roomKey}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-1">
              <button
                type="button"
                onClick={() => setSelectedId(null)}
                aria-label={t("monitor.closeDetail")}
                className="focus-ring -mr-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
              >
                <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
                  <path d="M2.5 2.5 9.5 9.5M9.5 2.5 2.5 9.5" strokeLinecap="round" />
                </svg>
              </button>
            </div>
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-5 py-4">
            <button
              type="button"
              onClick={() => void openWatch(room.id)}
              disabled={!room.isLive}
              aria-label={room.isLive ? t("monitor.watch") : t("monitor.watchOffline")}
              title={room.isLive ? t("monitor.watch") : t("monitor.watchOffline")}
              className={cx(
                "focus-ring group relative block w-full cursor-pointer overflow-hidden rounded-[8px] p-0 text-left disabled:cursor-not-allowed disabled:opacity-60",
                room.isLive &&
                  "[&_img]:transition-transform [&_img]:duration-300 [&_img]:ease-out group-hover:[&_img]:scale-[1.02]",
              )}
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
              {room.isLive ? (
                <span className="pointer-events-none absolute inset-0 flex items-center justify-center bg-black/0 opacity-0 transition-opacity duration-150 group-hover:bg-black/25 group-hover:opacity-100 group-focus-visible:bg-black/25 group-focus-visible:opacity-100">
                  <span className="flex h-11 w-11 items-center justify-center rounded-full bg-black/55 text-white shadow-pop ring-1 ring-white/25 backdrop-blur-sm">
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
                      <path d="M5.6 3.7a.75.75 0 0 1 1.14-.64l5.9 4.3a.75.75 0 0 1 0 1.28l-5.9 4.3a.75.75 0 0 1-1.14-.64V3.7Z" />
                    </svg>
                  </span>
                </span>
              ) : null}
            </button>

            <div className="mt-3 flex items-center gap-1.5 text-[12px]">
              <span
                className={cx(
                  "h-1.5 w-1.5 shrink-0 rounded-full",
                  isRecording ? "bg-rec" : room.isLive ? "bg-live" : "bg-faint",
                )}
              />
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
            <h3 className="mt-1.5 text-[15.5px] font-semibold leading-[22px] text-ink">{room.title || room.roomKey}</h3>
            <div className="mt-2 flex items-center gap-2">
              <Avatar room={room} size={20} />
              <span className="min-w-0 flex-1 truncate text-[12.5px] text-muted">{room.hostName || "—"}</span>
            </div>
            <div className="mt-1.5 flex items-center gap-2">
              <p className="min-w-0 flex-1 truncate text-[11.5px] text-faint" title={room.canonicalUrl}>
                {room.canonicalUrl}
              </p>
              <button
                type="button"
                onClick={() => void copyText(room.canonicalUrl)}
                aria-label={t("common.copy")}
                title={t("common.copy")}
                className="focus-ring flex h-6 w-6 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
              >
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                  <rect x="5.2" y="5.2" width="8.3" height="8.3" rx="1.4" />
                  <path d="M10.8 5.2V3.8a1.4 1.4 0 0 0-1.4-1.4H3.8a1.4 1.4 0 0 0-1.4 1.4v5.6a1.4 1.4 0 0 0 1.4 1.4h1.4" />
                </svg>
              </button>
            </div>
            {roomTags.length > 0 ? (
              <div className="mt-2 flex flex-wrap gap-1.5">
                {roomTags.map((tag) => (
                  <TagBadge key={tag} tag={tag} className="text-[10.5px] leading-4" />
                ))}
              </div>
            ) : null}
            {room.note ? (
              <p className="mt-2 whitespace-pre-wrap rounded-[6px] bg-raised/60 px-2 py-1.5 text-[11.5px] leading-4 text-muted">
                {room.note}
              </p>
            ) : null}

            <div className="mt-3 grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto_auto] gap-2">
              <Button
                size="md"
                variant="primary"
                className="w-full"
                disabled={busy || !room.isLive}
                title={room.isLive ? t("monitor.watch") : t("monitor.watchOffline")}
                loading={busy && busyAction === "watch"}
                onClick={() => void openWatch(room.id)}
              >
                {room.isLive ? t("monitor.watch") : t("monitor.watchOfflineShort")}
              </Button>
              <Button
                size="md"
                variant={isRecording ? "recording" : "secondary"}
                className="w-full"
                disabled={busy || (!room.isLive && !isRecording)}
                loading={busy && busyAction === "record"}
                onClick={() => toggleRecording(view)}
              >
                {isRecording ? t("monitor.stopRecord") : t("monitor.startRecord")}
              </Button>
              <Button
                size="md"
                variant="secondary"
                disabled={busy}
                onClick={() => refreshRoom(room.id)}
              >
                {t("common.refresh")}
              </Button>
              <DropdownMenu
                label={t("monitor.more")}
                ariaLabel={t("monitor.more")}
                icon={
                  <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
                    <circle cx="3" cy="8" r="1.3" />
                    <circle cx="8" cy="8" r="1.3" />
                    <circle cx="13" cy="8" r="1.3" />
                  </svg>
                }
                iconOnly
                variant="bare"
                size="sm"
                align="end"
                items={[
                  {
                    key: "browser",
                    label: t("monitor.openBrowser"),
                    onSelect: () => void openRoomUrl(room.canonicalUrl),
                  },
                  {
                    key: "media",
                    label: t("monitor.openMedia"),
                    disabled: !onOpenMedia,
                    separatorBefore: true,
                    onSelect: () => onOpenMedia?.(room.id),
                  },
                  {
                    key: "tasks",
                    label: t("monitor.openTasks"),
                    disabled: !onOpenTasks,
                    onSelect: () => onOpenTasks?.(room.id),
                  },
                ]}
              />
            </div>

            {recording ? (
              <>
                <h3 className="mt-5 flex items-center gap-1 text-[11.5px] font-medium text-muted">
                  {t("monitor.currentRecording")}
                  <InfoTip content={t("monitor.tip.currentRecording")} />
                </h3>
                <div className="sticky top-0 z-10 mt-2 animate-fade-rise overflow-hidden rounded-[8px] border border-rec/20 bg-surface/95 backdrop-blur">
                  <div className="flex items-center gap-3 px-3 py-2.5">
                    <span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-rec" />
                    <div className="min-w-0 flex-1">
                      <div className="tnum text-[13px] font-medium text-ink">
                        <LiveDuration startedAt={recording.startedAt} />
                      </div>
                      <div className="mt-0.5 truncate text-[11px] text-faint">
                        {recording.targetQn < 40000 &&
                        recording.targetQuality &&
                        recording.targetQuality !== recording.quality
                          ? t("monitor.qualityTargetActual", {
                              target: recording.targetQuality,
                              actual: recording.quality || "—",
                            })
                          : recording.quality || "—"}{" "}
                        · {recording.line || t("monitor.lineAuto")}
                      </div>
                    </div>
                  </div>
                  <div className="flex items-center gap-2 border-t border-rec/15 px-3 py-2">
                    <span className="shrink-0 text-[11.5px] text-faint">{t("monitor.recordingFile")}</span>
                    <span
                      className="min-w-0 flex-1 truncate text-right text-[11.5px] text-ink-soft"
                      title={recording.filePath}
                    >
                      {recordingFile || "—"}
                    </span>
                    <button
                      type="button"
                      onClick={() => void revealSession(recording.sessionId)}
                      aria-label={t("monitor.reveal")}
                      title={t("monitor.reveal")}
                      className="focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
                    >
                      <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                        <path
                          d="M1.8 4.6a1 1 0 0 1 1-1h3l1.4 1.5h5a1 1 0 0 1 1 1v5.7a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1V4.6Z"
                          strokeLinejoin="round"
                        />
                      </svg>
                    </button>
                  </div>
                  <RecordingUsageRows sessionId={recording.sessionId} />
                </div>
              </>
            ) : null}

            <h3 className="mt-5 flex items-center gap-1 text-[11.5px] font-medium text-muted">
              {t("monitor.monitoring")}
              <InfoTip content={t("monitor.tip.monitoring")} />
            </h3>
            <div className="mt-2 overflow-hidden rounded-[8px] border border-line">
              <div className="flex items-center gap-3 px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] text-ink-soft">{t("monitor.autoRecord")}</div>
                  <p className="mt-0.5 text-[11px] leading-4 text-faint">{t("monitor.addAutoRecordHint")}</p>
                </div>
                <Switch
                  checked={room.autoRecord}
                  disabled={busy || room.monitorPaused}
                  onChange={(next) =>
                    void run(room.id, async () => {
                      await roomSetAutoRecord(room.id, next);
                      notify.success(
                        next ? t("monitor.autoRecordEnabled") : t("monitor.autoRecordDisabled"),
                      );
                    })
                  }
                  label={t("monitor.autoRecord")}
                />
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] text-ink-soft">{t("monitor.addMonitor")}</div>
                  <p className="mt-0.5 text-[11px] leading-4 text-faint">
                    {room.monitorPaused ? t("monitor.statusPaused") : t("monitor.addMonitorHint")}
                  </p>
                </div>
                <Switch
                  checked={!room.monitorPaused}
                  disabled={busy}
                  onChange={(next) => void run(room.id, () => roomSetMonitor(room.id, !next))}
                  label={t("monitor.addMonitor")}
                />
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="shrink-0 text-[11.5px] text-faint">{t("monitor.lastChecked")}</span>
                <span
                  className="min-w-0 flex-1 truncate text-right text-[12px] text-ink-soft"
                  title={room.lastCheckedAt ? datetime(room.lastCheckedAt) : undefined}
                >
                  {room.lastCheckedAt ? timeAgo(room.lastCheckedAt, locale) : "—"}
                </span>
              </div>
              <MonitorDiagnostics
                roomId={room.id}
                roomLabel={`${room.hostName || room.roomKey} · ${room.title || room.roomKey}`}
                lastCheckedAt={room.lastCheckedAt}
                error={view.error}
                onCopy={(text) => void copyText(text)}
              />
            </div>

            <h3 className="mt-5 text-[11.5px] font-medium text-muted">{t("monitor.roomInfo")}</h3>
            <div className="mt-2 overflow-hidden rounded-[8px] border border-line">
              {[
                { label: t("monitor.colArea"), value: room.areaName ?? "—" },
                { label: t("monitor.statOnline"), value: room.online ? compactNumber(room.online, locale) : "—" },
                { label: t("monitor.colQuality"), value: t("monitor.qn." + room.recordQn, { defaultValue: String(room.recordQn) }) },
                { label: t("monitor.actualQuality"), value: room.lastSessionQuality ?? "—" },
                { label: t("monitor.detailRecorded"), value: recordedLabel },
              ].map((item, index) => (
                <div
                  key={item.label}
                  className={cx("flex items-center gap-3 px-3 py-2", index > 0 && "border-t border-line")}
                >
                  <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{item.label}</span>
                  <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">{item.value}</span>
                </div>
              ))}
              <div className="flex items-center gap-2 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.lastRecorded")}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">{lastLabel}</span>
                {last ? (
                  <button
                    type="button"
                    onClick={() => void revealSession(last.id)}
                    aria-label={t("monitor.reveal")}
                    title={t("monitor.reveal")}
                    className="focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
                  >
                    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                      <path
                        d="M1.8 4.6a1 1 0 0 1 1-1h3l1.4 1.5h5a1 1 0 0 1 1 1v5.7a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1V4.6Z"
                        strokeLinejoin="round"
                      />
                    </svg>
                  </button>
                ) : null}
              </div>
            </div>

            <h3 className="mt-5 flex items-center gap-1 text-[11.5px] font-medium text-muted">
              {t("monitor.recordingStats")}
              <InfoTip content={t("monitor.tip.recordingStats")} />
            </h3>
            <div className="mt-2 overflow-hidden rounded-[8px] border border-line">
              <div className="flex items-center gap-3 px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.totalDuration")}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">
                  {drawerSummary ? clockMs(drawerSummary.totalDurationMs) : "—"}
                </span>
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.averageDuration")}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">
                  {drawerSummary && drawerSummary.total > 0
                    ? clockMs(Math.round(drawerSummary.totalDurationMs / drawerSummary.total))
                    : "—"}
                </span>
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.successRate")}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">
                  {drawerSummary && drawerSummary.total > 0
                    ? `${Math.round(((drawerSummary.total - drawerSummary.failedCount) / drawerSummary.total) * 100)}%`
                    : "—"}
                </span>
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.recent7d")}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">
                  {drawerSummary
                    ? `${t("monitor.sessionCount", { n: drawerSummary.recentCount })} · ${clockMs(drawerSummary.recentDurationMs)} · ${size(drawerSummary.recentBytes)}`
                    : "—"}
                </span>
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.lastResult")}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[12.5px] text-ink-soft">{lastResultLabel}</span>
              </div>
            </div>

            <h3 className="mt-5 flex items-center gap-1 text-[11.5px] font-medium text-muted">
              {t("monitor.panelSettings")}
              <InfoTip content={t("monitor.tip.panelSettings")} />
            </h3>
            <div className="mt-2 overflow-hidden rounded-[8px] border border-line">
              <div className="flex items-center gap-3 px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.quality")}</span>
                <SelectMenu
                  value={String(room.recordQn)}
                  size="md"
                  disabled={busy}
                  onChange={(next) => void run(room.id, () => roomSetQuality(room.id, Number(next), room.viewQn))}
                  onOpenChange={(open) => {
                    if (open) void loadOptions(room.id, room.recordQn, true);
                  }}
                  options={qualityOptionsFor(room.id, room.recordQn, "record")}
                  className="min-w-0 flex-1 justify-between"
                />
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.viewQuality")}</span>
                <SelectMenu
                  value={String(room.viewQn)}
                  size="md"
                  disabled={busy}
                  onChange={(next) => void run(room.id, () => roomSetQuality(room.id, room.recordQn, Number(next)))}
                  onOpenChange={(open) => {
                    if (open) void loadOptions(room.id, room.recordQn, true);
                  }}
                  options={qualityOptionsFor(room.id, room.viewQn, "view")}
                  className="min-w-0 flex-1 justify-between"
                />
              </div>
              <div className="flex items-center gap-3 border-t border-line px-3 py-2">
                <span className="w-[72px] shrink-0 text-[11.5px] text-faint">{t("monitor.line")}</span>
                <SelectMenu
                  value={room.recordLine ?? ""}
                  size="md"
                  disabled={lineBusy || busy}
                  onChange={(next) => void run(room.id, () => roomSetLine(room.id, next || null))}
                  onOpenChange={(open) => {
                    if (open) void loadOptions(room.id, room.recordQn, true);
                  }}
                  options={lineOptions}
                  className="min-w-0 flex-1 justify-between"
                />
                <button
                  type="button"
                  onClick={() => void loadOptions(room.id, room.recordQn, true)}
                  disabled={lineBusy || busy}
                  aria-label={t("monitor.lineLoad")}
                  title={lineBusy ? t("common.loading") : t("monitor.lineLoad")}
                  className="focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink disabled:opacity-40"
                >
                  {lineBusy ? (
                    <span className="h-3 w-3 animate-spin rounded-full border-[1.5px] border-line-strong border-t-accent" />
                  ) : (
                    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
                      <path d="M13.2 8a5.2 5.2 0 1 1-1.5-3.7" strokeLinecap="round" />
                      <path d="M13.5 2.8v3.2h-3.2" strokeLinecap="round" strokeLinejoin="round" />
                    </svg>
                  )}
                </button>
              </div>
            </div>
            {lines?.length === 0 ? (
              <span className="mt-2 block text-[11.5px] text-faint">{t("monitor.lineEmpty")}</span>
            ) : null}

            <RoomRules key={room.id} room={room} busy={busy} />
          </div>
        </aside>
      </div>,
      document.body,
    );
  };
  return (
    <PageShell>
      <AddRoomDialog
        open={addOpen}
        onOpenChange={setAddOpen}
        onAdded={() => load()}
      />
      <ImportFollowsDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        rooms={views}
        onImported={() => load()}
      />

      <PageToolbar className="pt-4">
        <PageChip
          active={platform === "all" && tag === "all" && !onlyRecording}
          onClick={() => {
            setPlatform("all");
            setTag("all");
            setOnlyRecording(false);
          }}
        >
          {t("monitor.filterAll")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{counts.all ?? 0}</span>
        </PageChip>
        <SelectMenu
          variant="chip"
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
        <SelectMenu
          variant="chip"
          align="start"
          active={tag !== "all"}
          value={tag}
          onChange={setTag}
          placeholder={tagLabel}
          disabled={tagOptions.length === 0}
          options={[
            { value: "all", label: t("monitor.allTags"), hint: String(views.length) },
            ...tagOptions.map((item) => ({
              value: item.value,
              label: item.label,
              hint: String(item.count),
            })),
          ]}
        />
        <PageChip active={onlyRecording} onClick={() => setOnlyRecording((on) => !on)}>
          {t("monitor.onlyRecording")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{recordingCount}</span>
        </PageChip>
        <div className="ml-auto flex items-center gap-2">
          <PageSearch
            value={queryInput}
            onChange={changeQuery}
            placeholder={t("monitor.searchPlaceholder")}
            clearLabel={t("common.clear")}
          />
          <Button
            size="lg"
            variant="secondary"
            onClick={() => setImportOpen(true)}
          >
            {t("monitor.importFollow.entry")}
          </Button>
          <Button
            size="lg"
            variant="primary"
            onClick={() => setAddOpen(true)}
          >
            <span className="text-[15px] leading-none">+</span>
            {t("monitor.addRoom")}
          </Button>
        </div>
      </PageToolbar>

      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1 flex-col">
          {loaded && views.length > 0 ? (
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
              <span>{t("monitor.colNote")}</span>
              <span>{t("monitor.colTags")}</span>
              <span>{t("monitor.colQuality")}</span>
              <span className="text-right">{t("monitor.colDuration")}</span>
              <span className="text-right">{t("monitor.colActions")}</span>
            </div>
          ) : null}

          <div className={cx("min-h-0 flex-1 overflow-auto", picked.size > 0 ? "pb-20" : "")}>
            {!loaded ? (
              <div className="flex h-full items-center justify-center gap-2 text-[12px] text-faint">
                <span className="h-3.5 w-3.5 animate-spin rounded-full border-[1.5px] border-line-strong border-t-accent" />
                {t("common.loading")}
              </div>
            ) : loadFailed && views.length === 0 ? (
              <PageEmpty
                className="h-full"
                title={t("monitor.loadFailedTitle")}
                description={t("monitor.loadFailedBody")}
                icon={<EmptyArt kind="monitor" />}
                action={
                  <Button size="sm" variant="secondary" onClick={retryLoad}>
                    {t("common.retry")}
                  </Button>
                }
              />
            ) : views.length === 0 ? (
              <PageEmpty
                className="h-full"
                title={t("monitor.emptyTitle")}
                description={t("monitor.emptyBody")}
                icon={<EmptyArt kind="monitor" />}
              />
            ) : shown.length === 0 ? (
              <PageEmpty
                className="h-full"
                title={t("monitor.noMatch")}
                description={t("monitor.noMatchBody")}
                icon={<EmptyArt kind="search" />}
              />
            ) : (
              shown.map(renderRow)
            )}
          </div>
        </div>

      </div>

      {picked.size > 0 ? (
        <div className="absolute bottom-6 left-1/2 z-40 -translate-x-1/2">
          <FloatingButtonGroup
            label={t("monitor.selectionActions")}
            variant="floating"
            size="sm"
            leading={
              <>
                {t("monitor.selectedCount", { n: picked.size })}
                <span className="ml-1.5 font-normal text-faint">· {t("monitor.splitMode")}</span>
              </>
            }
            items={[
              {
                id: "clear",
                label: t("monitor.clearSelection"),
                onSelect: () => setPicked(new Set()),
              },
              { type: "separator", id: "selection-sep" },
              ...splitModes.map((mode) => ({
                id: mode.kind,
                label: mode.label,
                icon: <LayoutGlyph kind={mode.kind} />,
                onSelect: () => onOpenWatch?.(pickedIds, mode.kind),
              })),
            ]}
          />
        </div>
      ) : null}

      {selected ? renderPanel(selected) : null}
    </PageShell>
  );
}