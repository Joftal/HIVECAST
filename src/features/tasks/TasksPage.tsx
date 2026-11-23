import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent } from "../../components/arc/dialog/dialog";
import { Button, Chip, Switch, cx } from "../../components/ui/Controls";
import EmptyArt from "../../components/ui/EmptyArt";
import PageEmpty from "../../components/ui/PageEmpty";
import { PageBody, PageChip, PageHeader, PageShell, PageToolbar } from "../../components/ui/PageShell";
import {
  isTauri,
  postCancel,
  postRetry,
  finalizeRetry,
  taskCenter,
  taskEventSearch,
  taskEventsExport,
  taskRunEvents,
  type Incident,
  type PlatformHealth,
  type RunRoom,
  type RunView,
  type TaskCenter,
  type TaskEvent,
  type TaskEventHit,
  type TaskEventSearch,
  type TaskView,
} from "../../lib/ipc";
import { useNotify } from "../../lib/notify";

const HISTORY_PAGE = 100;
const HISTORY_MAX = 1_000;
const EVENT_PAGE = 200;
const HISTORY_VIEWPORT = 420;
const HISTORY_ROW = 56;
const HISTORY_OVERSCAN = 6;
const TIMELINE_WINDOW = 200;
const ACTIVE_RUN_STATES = new Set(["recording", "running", "finalizing", "queued"]);

/** 合并两段按时间正序的事件，按 id 去重；后传入的覆盖旧状态。 */
function mergeEvents(current: TaskEvent[], incoming: TaskEvent[]): TaskEvent[] {
  const map = new Map<number, TaskEvent>();
  for (const event of current) map.set(event.id, event);
  for (const event of incoming) map.set(event.id, event);
  return [...map.values()].sort((a, b) => a.at - b.at || a.id - b.id);
}

const EMPTY_CENTER: TaskCenter = {
  generatedAt: 0,
  summary: { rooms: 0, live: 0, recording: 0, queued: 0, running: 0, failed: 0, incidents: 0 },
  platforms: [],
  system: { ffmpegOk: true, diskFreeBytes: null, diskLow: false, mediaRoot: "" },
  running: [],
  queued: [],
  history: [],
  historyHasMore: false,
  incidents: [],
};

const STAGE_KEYS: Record<string, string> = {
  record: "tasks.stageRecord",
  finalize: "tasks.stageFinalize",
  post: "tasks.stagePost",
  commit: "tasks.stageCommit",
  merging: "tasks.stageMerging",
  splitting: "tasks.stageSplitting",
  danmaku: "tasks.stageDanmaku",
  thumbnail: "tasks.stageThumbnail",
  exporting: "tasks.stageExporting",
  postprocessing: "tasks.stagePostprocessing",
  starting: "tasks.stageStarting",
  recording: "tasks.stageRecording",
  finalizing: "tasks.stageFinalizing",
  queued: "tasks.stageQueued",
  done: "tasks.stageDone",
  failed: "tasks.stageFailed",
  cancelled: "tasks.stageCancelled",
  interrupted: "tasks.stageInterrupted",
  pending: "tasks.stagePending",
  skipped: "tasks.stageSkipped",
  partial: "tasks.stagePartial",
};

const STATE_KEYS: Record<string, string> = {
  running: "tasks.stateRunning",
  active: "tasks.stateRunning",
  queued: "tasks.stateQueued",
  pending: "tasks.statePending",
  skipped: "tasks.stateSkipped",
  done: "tasks.stateDone",
  failed: "tasks.stateFailed",
  cancelled: "tasks.stateCancelled",
  interrupted: "tasks.stateInterrupted",
  recording: "tasks.stateRecording",
  finalizing: "tasks.stateFinalizing",
  partial: "tasks.statePartial",
};

const KIND_KEYS: Record<string, string> = {
  record: "tasks.kindRecord",
  post: "tasks.kindPost",
  remux: "tasks.kindRemux",
  merge: "tasks.kindMerge",
  danmaku: "tasks.kindDanmaku",
  finalize: "tasks.kindFinalize",
};

const INCIDENT_KEYS: Record<string, string> = {
  task_failed: "tasks.incidentTaskFailed",
  record_failed: "tasks.incidentRecordFailed",
  monitor_backoff: "tasks.incidentMonitorBackoff",
  login_expired: "tasks.incidentLoginExpired",
  ffmpeg_missing: "tasks.incidentFfmpegMissing",
  disk_low: "tasks.incidentDiskLow",
};

type Filter = "all" | "running" | "queued" | "incidents";

function formatTime(seconds: number | null | undefined): string {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "—";
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return h > 0 ? `${h}:${mm}:${String(s).padStart(2, "0")}` : `${mm}:${String(s).padStart(2, "0")}`;
}

function formatClock(seconds: number): string {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value >= 100 || index === 0 ? Math.round(value) : value.toFixed(1)} ${units[index]}`;
}

function formatElapsed(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "00:00";
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function stageLabel(t: (key: string) => string, stage: string | null | undefined): string {
  if (!stage) return "—";
  return t(STAGE_KEYS[stage] ?? "tasks.stageUnknown");
}

function stateLabel(t: (key: string) => string, state: string): string {
  return t(STATE_KEYS[state] ?? "tasks.stateUnknown");
}

function stateTone(state: string): "neutral" | "accent" | "live" | "rec" | "warn" {
  switch (state) {
    case "recording":
    case "finalizing":
    case "running":
      return "accent";
    case "queued":
    case "partial":
      return "warn";
    case "done":
      return "live";
    case "failed":
      return "rec";
    case "interrupted":
      return "warn";
    default:
      return "neutral";
  }
}

function platformName(t: (key: string) => string, platform: string | null | undefined): string {
  return platform ? t("platformName." + platform) : "—";
}

function activeTask(run: RunView): TaskView | undefined {
  return run.tasks.find((task) => task.state === "running");
}

function queuedTasks(run: RunView): TaskView[] {
  return run.tasks.filter((task) => task.state === "queued");
}

function queuedTask(run: RunView): TaskView | undefined {
  return queuedTasks(run)[0];
}

function progressOf(run: RunView): number | null {
  const values = run.tasks
    .filter((task) => task.state === "running")
    .map((task) => Math.round(Number(task.percent)))
    .filter((value) => Number.isFinite(value) && value > 0);
  if (values.length === 0) return null;
  return Math.max(0, Math.min(100, Math.max(...values)));
}

function canCancel(run: RunView): TaskView | undefined {
  return run.tasks.find(
    (task) =>
      (task.state === "running" || task.state === "queued") &&
      !!task.sessionId &&
      (task.kind === "post" || task.kind === "remux" || task.kind === "merge"),
  );
}

function failedTask(run: RunView): TaskView | undefined {
  return run.tasks.find(
    (task) =>
      (task.state === "failed" || task.state === "interrupted") &&
      !!task.sessionId &&
      (task.kind === "post" || task.kind === "remux"),
  );
}

/// 收尾失败可能已经重试过：先找最新一条收尾任务，只有它仍失败才允许重试。
function failedFinalizeTask(run: RunView): TaskView | undefined {
  if (!run.finalizeRetryReady) return undefined;
  const latest = run.tasks
    .filter((task) => task.kind === "finalize" && !!task.sessionId)
    .sort((a, b) => a.startedAt - b.startedAt)
    .at(-1);
  return latest && (latest.state === "failed" || latest.state === "interrupted")
    ? latest
    : undefined;
}

function clipText(t: TFunction, task: TaskView): string | null {
  if (!task.clipIndex || task.clipCount <= 1) return null;
  return t("tasks.clipOf", { index: task.clipIndex, count: task.clipCount });
}

function Avatar({ room }: { room: RunRoom | null }) {
  const [failed, setFailed] = useState(false);
  const name = room?.hostName?.trim() || room?.title?.trim() || "?";
  const initial = Array.from(name)[0] ?? "?";
  if (room?.avatarUrl && !failed) {
    return (
      <img
        src={room.avatarUrl}
        alt=""
        className="h-9 w-9 shrink-0 rounded-full border border-line object-cover"
        onError={() => setFailed(true)}
      />
    );
  }
  return (
    <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full border border-line bg-raised text-[13px] font-semibold text-muted">
      {initial}
    </span>
  );
}

function StageRail({ run }: { run: RunView }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-1.5">
      {run.stages.map((stage, index) => {
        const node = {
          done: "border-live/40 bg-live text-white",
          active: "border-accent bg-accent text-white",
          failed: "border-rec bg-rec text-white",
          partial: "border-warn/40 bg-warn text-white",
          cancelled: "border-line bg-raised text-faint",
          skipped: "border-line bg-surface text-faint",
          pending: "border-line bg-surface text-faint",
        }[stage.state] ?? "border-line bg-surface text-faint";
        return (
          <div key={stage.key} className={cx("flex min-w-0 items-center gap-1.5", index > 0 && "flex-1")}>
            {index > 0 ? (
              <span
                className={cx(
                  "h-px min-w-3 flex-1",
                  run.stages[index - 1]?.state === "done" ? "bg-live/35" : "bg-line",
                )}
              />
            ) : null}
            <span
              className={cx(
                "flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-full border text-[10px] font-semibold",
                node,
              )}
              aria-hidden
            >
              {stage.state === "done"
                ? "✓"
                : stage.state === "failed"
                  ? "!"
                  : stage.state === "partial"
                    ? "~"
                    : stage.state === "active"
                      ? "•"
                      : ""}
            </span>
            <span
              className={cx(
                "shrink-0 text-[11px]",
                stage.state === "active"
                  ? "font-medium text-accent-text"
                  : stage.state === "failed"
                    ? "text-rec"
                    : stage.state === "partial"
                      ? "text-warn"
                      : stage.state === "done"
                      ? "text-ink-soft"
                      : "text-faint",
              )}
            >
              {stageLabel(t, stage.key)}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function PlatformCard({
  health,
  active,
  onSelect,
}: {
  health: PlatformHealth;
  active: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation();
  const levelClass =
    health.level === "error"
      ? "text-rec"
      : health.level === "warn"
        ? "text-warn"
        : health.level === "idle"
          ? "text-faint"
          : "text-live";
  const authText =
    health.auth === "ok"
      ? t("tasks.authOk")
      : health.auth === "expired"
        ? t("tasks.authExpired")
        : t("tasks.authNone");
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={active}
      className={cx(
        "focus-ring min-w-0 rounded-panel border bg-surface px-3.5 py-3 text-left transition-colors",
        active ? "border-accent-line bg-accent-soft/45" : "border-line hover:border-line-strong",
      )}
    >
      <div className="flex items-center gap-2">
        <span className={cx("h-1.5 w-1.5 shrink-0 rounded-full", health.level === "error" ? "bg-rec" : health.level === "warn" ? "bg-warn" : health.level === "idle" ? "bg-faint" : "bg-live")} />
        <span className="truncate text-[12.5px] font-semibold text-ink">{platformName(t, health.platform)}</span>
        <span className={cx("ml-auto shrink-0 text-[10.5px]", levelClass)}>
          {health.checking ? t("tasks.checking") : health.backoff ? t("tasks.backoff") : authText}
        </span>
      </div>
      <div className="mt-1.5 truncate text-[10.5px] text-muted">
        {t("tasks.platformStats", { rooms: health.rooms, live: health.live, recording: health.recording })}
      </div>
      <div className="mt-1 flex items-center gap-2 text-[10px] text-faint">
        <span className="truncate">{t("tasks.batchFollow", { count: health.batchRooms })}</span>
        {health.paused > 0 ? <span>· {t("tasks.paused", { count: health.paused })}</span> : null}
        {health.nextCheckInSecs != null ? (
          <span className="ml-auto shrink-0 tabular-nums">{formatElapsed(health.nextCheckInSecs)}</span>
        ) : null}
      </div>
    </button>
  );
}

function RunCard({
  run,
  now,
  busy,
  onCancel,
  onOpen,
}: {
  run: RunView;
  now: number;
  busy: boolean;
  onCancel: (task: TaskView) => void;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const task = activeTask(run);
  const queued = queuedTask(run);
  const percent = progressOf(run);
  const wait = queued ? Math.max(0, now - (queued.queuedAt ?? queued.startedAt)) : 0;
  const detail =
    task?.message ||
    task?.detail ||
    (run.stage === "starting" ? t("tasks.actionStarting") : null) ||
    (run.stage === "finalizing" ? t("tasks.actionFinalizing") : null) ||
    (queued ? t("tasks.waitingSlot", { time: formatElapsed(wait) }) : null) ||
    t("tasks.actionWaiting");
  const clip = run.tasks.map((item) => clipText(t, item)).find(Boolean) ?? null;
  const cancelTask = canCancel(run);
  return (
    <article className="rounded-panel border border-line bg-surface p-3.5 shadow-resting">
      <div className="flex items-start gap-3">
        <Avatar room={run.room} />
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-1.5 text-[10.5px] text-faint">
            <span className="shrink-0">{platformName(t, run.room?.platform)}</span>
            <span>·</span>
            <span className="truncate">{run.room?.roomKey ?? "—"}</span>
            {run.quality ? (
              <>
                <span>·</span>
                <span className="truncate">{run.quality}</span>
              </>
            ) : null}
          </div>
          <h3 className="mt-0.5 truncate text-[13.5px] font-semibold text-ink">
            {run.room?.hostName || run.room?.title || t("tasks.detailNoRoom")}
          </h3>
          <p className="mt-0.5 truncate text-[11px] text-muted">{run.room?.title ?? "—"}</p>
        </div>
        <div className="shrink-0 text-right">
          <div className="text-[11px] font-medium tabular-nums text-ink-soft">{formatDuration(run.durationMs)}</div>
          <div className="mt-0.5 text-[10px] tabular-nums text-faint">{formatBytes(run.bytes)}</div>
          <div className="mt-1">
            <Chip tone={stateTone(run.state)} dot>
              {stateLabel(t, run.state)}
            </Chip>
          </div>
        </div>
      </div>

      <div className="mt-3">
        <StageRail run={run} />
      </div>

      <div className="mt-2 flex min-w-0 items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-[11px] text-muted" title={detail ?? undefined}>
          {detail}
        </span>
        {percent != null ? (
          <span className="shrink-0 text-[10px] tabular-nums text-faint">{percent}%</span>
        ) : null}
      </div>
      {percent != null ? (
        <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-raised">
          <div className="h-full rounded-full bg-accent transition-[width] duration-500" style={{ width: percent + "%" }} />
        </div>
      ) : null}

      <div className="mt-2 flex items-center gap-2">
        {clip ? <span className="rounded-full bg-raised px-2 py-0.5 text-[10px] tabular-nums text-muted">{clip}</span> : null}
        <span className="text-[10px] tabular-nums text-faint">{formatTime(run.startedAt)}</span>
        <div className="ml-auto flex items-center gap-1">
          <Button size="sm" variant="ghost" onClick={onOpen}>
            {t("tasks.detailOpen")}
          </Button>
          {cancelTask ? (
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => onCancel(cancelTask)}>
              {t("tasks.cancel")}
            </Button>
          ) : null}
        </div>
      </div>
    </article>
  );
}

function QueuedRow({
  run,
  now,
  busy,
  onCancel,
  onOpen,
}: {
  run: RunView;
  now: number;
  busy: boolean;
  onCancel: (task: TaskView) => void;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const tasks = queuedTasks(run);
  const task = tasks[0];
  const wait = task ? Math.max(0, now - (task.queuedAt ?? task.startedAt)) : 0;
  const kind = task ? t(KIND_KEYS[task.kind] ?? "tasks.kindPost") : t("tasks.stateQueued");
  const clip = task ? clipText(t, task) : null;
  const more = tasks.length > 1 ? ` +${tasks.length - 1}` : "";
  const waiting = t("tasks.waitingSlot", { time: formatElapsed(wait) });
  return (
    <div className="flex w-full items-center border-b border-line last:border-b-0 hover:bg-raised/50">
      <button
        type="button"
        onClick={onOpen}
        className="focus-ring flex min-w-0 flex-1 items-center gap-3 px-4 py-2.5 text-left"
      >
        <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-warn" />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[12px] text-ink">
            {run.room?.hostName || run.room?.title || t("tasks.detailNoRoom")}
          </span>
          <span className="mt-0.5 block truncate text-[10.5px] text-faint">
            {kind}{clip ? ` · ${clip}` : ""}{more} · {waiting}
          </span>
        </span>
        <span className="w-[92px] shrink-0 truncate text-right text-[10px] text-faint">{platformName(t, run.room?.platform)}</span>
      </button>
      {task ? (
        <Button
          size="sm"
          variant="ghost"
          className="mr-2 shrink-0"
          disabled={busy}
          onClick={() => onCancel(task)}
        >
          {t("tasks.cancel")}
        </Button>
      ) : null}
    </div>
  );
}

function IncidentRow({
  incident,
  run,
  busy,
  onRetry,
  onOpenSettings,
  onOpenRun,
  onOpenRoom,
}: {
  incident: Incident;
  run: RunView | undefined;
  busy: boolean;
  onRetry: (task: TaskView) => void;
  onOpenSettings: (page: "account" | "storage" | "system") => void;
  onOpenRun: (run: RunView) => void;
  onOpenRoom: (roomId: string) => void;
}) {
  const { t } = useTranslation();
  const task = incident.taskId ? run?.tasks.find((item) => item.id === incident.taskId) : undefined;
  const title =
    incident.code === "login_expired"
      ? t("tasks.incidentLoginExpiredPlatform", { platform: platformName(t, incident.platform) })
      : incident.code === "monitor_backoff" || incident.code === "record_failed"
        ? run?.room?.hostName
          ? t(
              incident.code === "record_failed"
                ? "tasks.incidentRecordFailedHost"
                : "tasks.incidentMonitorBackoffHost",
              { host: run.room.hostName },
            )
          : t(INCIDENT_KEYS[incident.code] ?? "tasks.incidentUnknown")
        : t(INCIDENT_KEYS[incident.code] ?? "tasks.incidentUnknown");
  // last_outcome 是内部枚举（failed/retry/throttled），不是给用户看的文案。
  const backoffDetail =
    incident.detail && !["failed", "retry", "throttled"].includes(incident.detail)
      ? incident.detail
      : t("tasks.incidentMonitorBackoffDetail");
  const detail =
    incident.code === "login_expired"
      ? t("tasks.incidentLoginExpiredDetail")
      : incident.code === "monitor_backoff"
        ? backoffDetail
        : incident.code === "disk_low"
          ? t("tasks.incidentDiskLowDetail")
          : incident.detail;
  const action =
    incident.action === "retry_finalize" && task?.sessionId
      ? { label: t("tasks.retryFinalize"), run: () => onRetry(task) }
      : incident.action === "retry_post" && task?.sessionId
        ? { label: t("tasks.retry"), run: () => onRetry(task) }
        : incident.action === "open_account"
        ? { label: t("tasks.actionOpenAccount"), run: () => onOpenSettings("account") }
        : incident.action === "open_storage"
          ? { label: t("tasks.actionOpenStorage"), run: () => onOpenSettings("storage") }
          : incident.action === "open_room" && incident.roomId
            ? { label: t("tasks.actionOpenRoom"), run: () => onOpenRoom(incident.roomId!) }
            : incident.action === "open_task" && run
              ? { label: t("tasks.actionOpenTask"), run: () => onOpenRun(run) }
              : null;
  return (
    <div className="flex items-start gap-3 border-b border-line px-4 py-3 last:border-b-0">
      <span
        className={cx(
          "mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full",
          incident.severity === "error" ? "bg-rec" : "bg-warn",
        )}
      />
      <div className="min-w-0 flex-1">
        <div className="truncate text-[12px] font-medium text-ink">{title}</div>
        {detail ? <div className="mt-0.5 truncate text-[10.5px] text-muted">{detail}</div> : null}
      </div>
      {action ? (
        <Button size="sm" variant="ghost" disabled={busy} onClick={action.run}>
          {action.label}
        </Button>
      ) : null}
    </div>
  );
}

function DetailDrawer({
  run,
  events,
  eventsHasMore,
  eventsLoading,
  eventsLoadingMore,
  eventsError,
  onLoadEarlier,
  busy,
  canOpenFiles,
  onClose,
  onCancel,
  onRetry,
  onOpenFiles,
  onCopy,
}: {
  run: RunView;
  events: TaskEvent[];
  eventsHasMore: boolean;
  eventsLoading: boolean;
  eventsLoadingMore: boolean;
  eventsError: boolean;
  onLoadEarlier: () => Promise<boolean>;
  busy: boolean;
  canOpenFiles: boolean;
  onClose: () => void;
  onCancel: (task: TaskView) => void;
  onRetry: (task: TaskView) => void;
  onOpenFiles: () => void;
  onCopy: () => void;
}) {
  const { t } = useTranslation();
  const cancelTask = canCancel(run);
  const retryTask = failedTask(run);
  const retryFinalizeTask = failedFinalizeTask(run);
  // 时间线一次只挂一个 200 条的窗口；2100 条事件如果全部进 DOM，
  // 可访问性树和滚动布局都会明显变慢。
  const timelinePages = Math.max(1, Math.ceil(events.length / TIMELINE_WINDOW));
  const [timelinePage, setTimelinePage] = useState(0);
  const timelineRunRef = useRef<string | null>(null);
  // 成功拉到更早一页后再前进一页。不能只看 events.length：失败后运行中的
  // Run 仍会追加新事件，旧实现会误以为用户刚刚加载了更早页。
  const [olderLoaded, setOlderLoaded] = useState(0);
  const timelinePagesRef = useRef(timelinePages);
  timelinePagesRef.current = timelinePages;
  useEffect(() => {
    if (timelineRunRef.current !== run.id) {
      timelineRunRef.current = run.id;
      setTimelinePage(0);
    }
  }, [run.id]);
  useEffect(() => {
    setTimelinePage((page) => Math.min(page, timelinePages - 1));
  }, [timelinePages]);
  useEffect(() => {
    if (olderLoaded === 0) return;
    setTimelinePage((page) => Math.min(page + 1, timelinePagesRef.current - 1));
  }, [olderLoaded]);
  const timelineEnd = Math.max(0, events.length - timelinePage * TIMELINE_WINDOW);
  const timelineStart = Math.max(0, timelineEnd - TIMELINE_WINDOW);
  const visibleTimeline = events.slice(timelineStart, timelineEnd);
  const loadOlder = async () => {
    if (await onLoadEarlier()) setOlderLoaded((count) => count + 1);
  };
  // Run id 的前缀是内部键：session:/task:/starting:。详情里不要把技术前缀直接当「场次」。
  const runRef = run.id.startsWith("session:")
    ? { label: t("tasks.detailSession"), value: run.id.slice("session:".length) }
    : run.id.startsWith("task:")
      ? { label: t("tasks.detailTask"), value: run.id.slice("task:".length) }
      : { label: t("tasks.detailStarting"), value: run.id.slice("starting:".length) };
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  return createPortal(
    <div className="fixed inset-0 z-50 flex justify-end">
      <button
        type="button"
        aria-label={t("tasks.close")}
        onClick={onClose}
        className="absolute inset-0 animate-fade-in bg-black/35 backdrop-blur-[2px]"
      />
      <aside className="relative flex h-full w-[min(460px,94vw)] animate-sheet flex-col border-l border-line bg-surface shadow-2xl">
        <div className="flex items-start justify-between gap-3 border-b border-line px-5 py-4">
          <div className="min-w-0">
            <div className="font-mono text-[10px] uppercase tracking-[0.14em] text-faint">
              run · {stateLabel(t, run.state)}
            </div>
            <h2 className="mt-1 truncate text-[15px] font-semibold text-ink">
              {run.room?.hostName || run.room?.title || t("tasks.detailNoRoom")}
            </h2>
            <p className="mt-0.5 truncate text-[11px] text-muted">
              {platformName(t, run.room?.platform)} · {run.room?.roomKey ?? "—"}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-1">
            <Button size="sm" variant="ghost" onClick={onCopy}>
              {t("tasks.copyDiagnostics")}
            </Button>
            <button
              type="button"
              onClick={onClose}
              aria-label={t("tasks.close")}
              className="focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
            >
              ×
            </button>
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-auto px-5 py-4">
          <div className="flex items-center gap-2">
            <Chip tone={stateTone(run.state)} dot>
              {stateLabel(t, run.state)}
            </Chip>
            {run.stage !== run.state ? (
              <span className="text-[11px] text-muted">{stageLabel(t, run.stage)}</span>
            ) : null}
            <span className="ml-auto text-[10.5px] tabular-nums text-faint">{formatTime(run.startedAt)}</span>
          </div>

          <div className="mt-3 grid grid-cols-3 gap-2 rounded-panel border border-line bg-canvas/40 px-3 py-2.5">
            <div>
              <div className="text-[10px] text-faint">{t("tasks.detailDuration")}</div>
              <div className="mt-0.5 text-[12px] font-medium tabular-nums text-ink">{formatDuration(run.durationMs)}</div>
            </div>
            <div>
              <div className="text-[10px] text-faint">{t("tasks.detailBytes")}</div>
              <div className="mt-0.5 text-[12px] font-medium tabular-nums text-ink">{formatBytes(run.bytes)}</div>
            </div>
            <div>
              <div className="text-[10px] text-faint">{t("tasks.detailEnded")}</div>
              <div className="mt-0.5 text-[12px] font-medium tabular-nums text-ink">{formatTime(run.endedAt)}</div>
            </div>
          </div>

          <section className="mt-5">
            <h3 className="text-[12px] font-semibold text-ink">{t("tasks.detailStages")}</h3>
            <div className="mt-2 space-y-0">
              {run.stages.map((stage, index) => {
                const tone =
                  stage.state === "done"
                    ? "border-live/40 bg-live text-white"
                    : stage.state === "active"
                      ? "border-accent bg-accent text-white"
                      : stage.state === "failed"
                        ? "border-rec bg-rec text-white"
                        : stage.state === "partial"
                          ? "border-warn/40 bg-warn text-white"
                          : "border-line bg-surface text-faint";
                return (
                  <div key={stage.key} className="flex gap-3">
                    <div className="flex flex-col items-center">
                      <span className={cx("flex h-5 w-5 shrink-0 items-center justify-center rounded-full border text-[10px] font-semibold", tone)}>
                        {stage.state === "done"
                          ? "✓"
                          : stage.state === "failed"
                            ? "!"
                            : stage.state === "partial"
                              ? "~"
                              : stage.state === "active"
                                ? "•"
                                : index + 1}
                      </span>
                      {index < run.stages.length - 1 ? <span className="my-1 w-px flex-1 bg-line" /> : null}
                    </div>
                    <div className="min-w-0 flex-1 pb-4">
                      <div className="flex items-baseline gap-2">
                        <span className="text-[12px] font-medium text-ink">{stageLabel(t, stage.key)}</span>
                        <span className="text-[10px] text-faint">{t(STATE_KEYS[stage.state] ?? "tasks.stateUnknown")}</span>
                        {stage.percent != null ? <span className="ml-auto text-[10px] tabular-nums text-faint">{stage.percent}%</span> : null}
                      </div>
                      {stage.detail ? <div className="mt-0.5 break-all text-[10.5px] leading-4 text-muted">{stage.detail}</div> : null}
                    </div>
                  </div>
                );
              })}
            </div>
          </section>

          <section className="mt-4">
            <h3 className="text-[12px] font-semibold text-ink">{t("tasks.detailTimeline")}</h3>
            <div className="mt-2">
              {events.length > 0 ? (
                <div className="mb-2 flex items-center gap-1.5">
                  {timelinePage < timelinePages - 1 ? (
                    <Button size="sm" variant="ghost" onClick={() => setTimelinePage((page) => page + 1)}>
                      {t("tasks.timelineOlder")}
                    </Button>
                  ) : eventsHasMore ? (
                    <Button size="sm" variant="ghost" disabled={eventsLoadingMore} onClick={() => void loadOlder()}>
                      {eventsLoadingMore ? t("tasks.timelineLoadingEarlier") : t("tasks.timelineLoadEarlier")}
                    </Button>
                  ) : null}
                  {timelinePage > 0 ? (
                    <Button size="sm" variant="ghost" onClick={() => setTimelinePage((page) => Math.max(0, page - 1))}>
                      {t("tasks.timelineNewer")}
                    </Button>
                  ) : null}
                  <span className="ml-auto text-[10px] tabular-nums text-faint">
                    {t("tasks.timelineWindow", {
                      from: timelineStart + 1,
                      to: timelineEnd,
                      total: events.length,
                    })}
                  </span>
                </div>
              ) : null}
              {eventsError && events.length > 0 ? (
                <div className="mb-2 text-[11px] text-muted">{t("tasks.timelineError")}</div>
              ) : null}
              {events.length === 0 && eventsLoading ? (
                <div className="px-1 py-2 text-[11px] text-faint">{t("tasks.timelineLoading")}</div>
              ) : events.length === 0 && eventsError ? (
                <div className="px-1 py-2 text-[11px] text-muted">{t("tasks.timelineError")}</div>
              ) : events.length === 0 ? (
                <div className="px-1 py-2 text-[11px] text-faint">{t("tasks.timelineEmpty")}</div>
              ) : (
                visibleTimeline.map((event, index) => {
                  const tone =
                    event.state === "failed"
                      ? "bg-rec"
                      : event.state === "done"
                        ? "bg-live"
                        : event.state === "queued"
                          ? "bg-line-strong"
                          : "bg-accent";
                  return (
                    <div key={event.id} className="flex gap-2.5">
                      <div className="flex flex-col items-center">
                        <span className={cx("mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full", tone)} />
                        {index < visibleTimeline.length - 1 ? <span className="my-1 w-px flex-1 bg-line" /> : null}
                      </div>
                      <div className="min-w-0 flex-1 pb-2.5">
                        <div className="flex items-baseline gap-2">
                          <span className="text-[11px] font-medium text-ink">
                            {stageLabel(t, event.stage)}
                          </span>
                          <span className="text-[10px] text-faint">{stateLabel(t, event.state)}</span>
                          {event.percent > 0 && event.percent < 100 ? (
                            <span className="text-[10px] tabular-nums text-faint">{event.percent}%</span>
                          ) : null}
                          <span className="ml-auto shrink-0 text-[10px] tabular-nums text-faint">
                            {formatClock(event.at)}
                          </span>
                        </div>
                        {event.message ? (
                          <div className="mt-0.5 break-all text-[10.5px] leading-4 text-muted">{event.message}</div>
                        ) : null}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </section>

          <section className="mt-4">
            <h3 className="text-[12px] font-semibold text-ink">{t("tasks.detailTasks")}</h3>
            <div className="mt-2 overflow-hidden rounded-panel border border-line">
              {run.tasks.length === 0 ? (
                <div className="px-3 py-3 text-[11px] text-faint">{t("tasks.detailNoTasks")}</div>
              ) : (
                run.tasks.map((task) => (
                  <div key={task.id} className="border-b border-line px-3 py-2.5 last:border-b-0">
                    <div className="flex items-center gap-2">
                      <span className="shrink-0 rounded-full bg-raised px-2 py-0.5 text-[10px] text-muted">
                        {t(KIND_KEYS[task.kind] ?? "tasks.kindPost")}
                      </span>
                      <span className="min-w-0 flex-1 truncate text-[11.5px] text-ink" title={task.title}>
                        {task.title}
                      </span>
                      <span className="shrink-0 text-[10px] text-faint">{stateLabel(t, task.state)}</span>
                    </div>
                    <div className="mt-1 flex items-center gap-2 text-[10px] text-faint">
                      {clipText(t, task) ? <span className="tabular-nums">{clipText(t, task)}</span> : null}
                      <span className="min-w-0 flex-1 truncate">{task.message || task.detail || stageLabel(t, task.stage)}</span>
                      {task.percent > 0 ? <span className="shrink-0 tabular-nums">{task.percent}%</span> : null}
                    </div>
                  </div>
                ))
              )}
            </div>
          </section>

          <section className="mt-4">
            <h3 className="text-[12px] font-semibold text-ink">{t("tasks.detailContext")}</h3>
            <dl className="mt-2 space-y-2 text-[11px]">
              <div className="flex gap-3">
                <dt className="w-[64px] shrink-0 text-faint">{runRef.label}</dt>
                <dd className="min-w-0 flex-1 break-all text-muted">{runRef.value}</dd>
              </div>
              <div className="flex gap-3">
                <dt className="w-[64px] shrink-0 text-faint">{t("tasks.detailStarted")}</dt>
                <dd className="min-w-0 flex-1 text-muted">{formatTime(run.startedAt)}</dd>
              </div>
              <div className="flex gap-3">
                <dt className="w-[64px] shrink-0 text-faint">{t("tasks.detailRoom")}</dt>
                <dd className="min-w-0 flex-1 break-all text-muted">{run.room?.id ?? "—"}</dd>
              </div>
            </dl>
          </section>
        </div>

        <div className="flex items-center gap-2 border-t border-line px-5 py-3">
          {canOpenFiles ? (
            <Button size="sm" variant="secondary" onClick={onOpenFiles}>
              {t("tasks.openFiles")}
            </Button>
          ) : null}
          <div className="ml-auto flex items-center gap-2">
            {retryFinalizeTask ? (
              <Button
                size="sm"
                variant="secondary"
                disabled={busy}
                onClick={() => onRetry(retryFinalizeTask)}
              >
                {t("tasks.retryFinalize")}
              </Button>
            ) : null}
            {retryTask ? (
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => onRetry(retryTask)}>
                {t("tasks.retry")}
              </Button>
            ) : null}
            {cancelTask ? (
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => onCancel(cancelTask)}>
                {t("tasks.cancel")}
              </Button>
            ) : null}
            <Button size="sm" variant="primary" onClick={onClose}>
              {t("tasks.close")}
            </Button>
          </div>
        </div>
      </aside>
    </div>,
    document.body,
  );
}

function EventSearchDialog({
  open,
  onOpenChange,
  initialPlatform,
  onOpenFiles,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  initialPlatform: string | null;
  onOpenFiles?: (roomId: string) => void;
}) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [query, setQuery] = useState("");
  const [onlyFailed, setOnlyFailed] = useState(false);
  const [platform, setPlatform] = useState<string | null>(initialPlatform);
  const [result, setResult] = useState<TaskEventSearch | null>(null);
  const [loading, setLoading] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [failed, setFailed] = useState(false);
  const seq = useRef(0);
  const wasOpen = useRef(false);

  // 每次打开都从当前平台筛选开始，但不要在输入过程中被外部筛选重置。
  useEffect(() => {
    if (open && !wasOpen.current) {
      setQuery("");
      setOnlyFailed(false);
      setPlatform(initialPlatform);
      setResult(null);
      setFailed(false);
    }
    wasOpen.current = open;
  }, [open, initialPlatform]);

  const runSearch = useCallback(async () => {
    if (!isTauri()) return;
    const id = ++seq.current;
    setLoading(true);
    setFailed(false);
    try {
      const next = await taskEventSearch(query, onlyFailed, platform);
      if (id === seq.current) setResult(next);
    } catch {
      if (id === seq.current) setFailed(true);
    } finally {
      if (id === seq.current) setLoading(false);
    }
  }, [query, onlyFailed, platform]);

  useEffect(() => {
    if (!open || !isTauri()) return;
    const timer = window.setTimeout(() => void runSearch(), 250);
    return () => window.clearTimeout(timer);
  }, [open, runSearch]);

  const exportCsv = async () => {
    if (!isTauri() || exporting) return;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const stamp = new Date().toISOString().slice(0, 10);
      const path = await save({
        defaultPath: `hivecast-events-${stamp}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (typeof path !== "string" || !path) return;
      setExporting(true);
      const count = await taskEventsExport(path, query, onlyFailed, platform);
      notify.success(t("tasks.eventSearchExported", { n: count }));
    } catch (err) {
      notify.error(err);
    } finally {
      setExporting(false);
    }
  };

  const platforms: Array<string | null> = [null, "bilibili", "douyu", "douyin", "huya"];
  const hitLabel = (hit: TaskEventHit) =>
    hit.hostName || hit.roomTitle || t("tasks.eventSearchNoRoom");

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        title={t("tasks.eventSearchTitle")}
        description={t("tasks.eventSearchHint")}
        size="lg"
      >
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void runSearch();
          }}
          className="flex gap-2"
        >
          <input
            autoFocus
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("tasks.eventSearchPlaceholder")}
            spellCheck={false}
            className="focus-ring h-9 min-w-0 flex-1 rounded-ctl border border-line bg-surface px-3 text-[12.5px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
          />
          <Button size="lg" type="submit" variant="primary" disabled={loading}>
            {loading ? t("tasks.eventSearchLoading") : t("tasks.eventSearchAction")}
          </Button>
        </form>

        <div className="mt-3 flex flex-wrap items-center gap-2">
          {platforms.map((item) => (
            <PageChip key={item ?? "all"} active={platform === item} onClick={() => setPlatform(item)}>
              {item ? platformName(t, item) : t("tasks.eventSearchAllPlatforms")}
            </PageChip>
          ))}
          <label className="ml-1 flex items-center gap-2 text-[11.5px] text-muted">
            <Switch
              checked={onlyFailed}
              onChange={setOnlyFailed}
              label={t("tasks.eventSearchOnlyFailed")}
            />
            {t("tasks.eventSearchOnlyFailed")}
          </label>
          <Button
            size="sm"
            variant="secondary"
            className="ml-auto"
            disabled={!result || result.total === 0 || exporting || loading}
            onClick={() => void exportCsv()}
          >
            {exporting ? t("tasks.eventSearchExporting") : t("tasks.eventSearchExport")}
          </Button>
        </div>

        <div className="mt-3 flex items-baseline justify-between gap-3">
          <span className="text-[11px] text-muted">
            {result
              ? t("tasks.eventSearchCount", { total: result.total, shown: result.hits.length })
              : t("tasks.eventSearchLoading")}
          </span>
          {failed ? <span className="text-[11px] text-rec">{t("tasks.eventSearchError")}</span> : null}
        </div>

        <div className="mt-2 max-h-[48vh] overflow-auto rounded-panel border border-line bg-surface">
          {result && result.hits.length === 0 ? (
            <div className="px-4 py-10 text-center text-[12px] text-faint">
              {t("tasks.eventSearchEmpty")}
            </div>
          ) : null}
          {result?.hits.map((hit) => (
            <div
              key={hit.id}
              className="flex items-start gap-3 border-b border-line px-3 py-2.5 last:border-b-0"
            >
              <div className="w-[86px] shrink-0 pt-0.5 font-mono text-[10px] tabular-nums text-faint">
                {formatTime(hit.at)}
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex min-w-0 items-center gap-2 text-[11px]">
                  <span className="shrink-0 text-faint">{platformName(t, hit.platform)}</span>
                  <span className="min-w-0 truncate font-medium text-ink" title={hitLabel(hit)}>
                    {hitLabel(hit)}
                  </span>
                  <span className="shrink-0 text-[10px] text-faint">
                    {stageLabel(t, hit.stage)} · {stateLabel(t, hit.state)}
                    {hit.percent > 0 && hit.percent < 100 ? ` ${hit.percent}%` : ""}
                  </span>
                </div>
                <div className="mt-0.5 truncate text-[11.5px] text-ink-soft" title={hit.message ?? undefined}>
                  {hit.message || "—"}
                </div>
                <div className="mt-0.5 truncate font-mono text-[10px] text-faint">
                  {hit.runId ?? hit.taskId}
                </div>
              </div>
              {hit.roomId && onOpenFiles ? (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    onOpenChange(false);
                    onOpenFiles(hit.roomId!);
                  }}
                >
                  {t("tasks.eventSearchFiles")}
                </Button>
              ) : null}
            </div>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}

export default function TasksPage({
  seed,
  onSeedApplied,
  onOpenSettings,
  onOpenRoomFiles,
  onOpenMonitor,
  onOpenRoom,
}: {
  seed?: { roomId: string; nonce: number } | null;
  onSeedApplied?: () => void;
  onOpenSettings?: (page: "account" | "storage" | "system") => void;
  onOpenRoomFiles?: (roomId: string) => void;
  onOpenMonitor?: () => void;
  onOpenRoom?: (roomId: string) => void;
}) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [center, setCenter] = useState<TaskCenter>(EMPTY_CENTER);
  const [loaded, setLoaded] = useState(false);
  const [filter, setFilter] = useState<Filter>("all");
  const [platformFilter, setPlatformFilter] = useState<string | null>(null);
  const [roomFilter, setRoomFilter] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [historyLimit, setHistoryLimit] = useState(HISTORY_PAGE);
  const [loadingMore, setLoadingMore] = useState(false);
  const [historyScrollTop, setHistoryScrollTop] = useState(0);
  const historyScrollRef = useRef<HTMLDivElement | null>(null);
  const historyLimitRef = useRef(HISTORY_PAGE);
  const loadingMoreRef = useRef(false);
  const loadErrorShown = useRef(false);
  const loadSeq = useRef(0);

  useEffect(() => {
    setHistoryScrollTop(0);
    historyScrollRef.current?.scrollTo({ top: 0 });
  }, [platformFilter, roomFilter, filter]);

  useEffect(() => {
    if (!seed) return;
    setRoomFilter(seed.roomId);
    setFilter("all");
    onSeedApplied?.();
    // 只响应新的跳转请求，避免 onSeedApplied 每次渲染换引用导致循环。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seed?.nonce]);

  const load = useCallback(async (limit = historyLimitRef.current) => {
    if (!isTauri()) {
      setLoaded(true);
      return { ok: true, stale: false };
    }
    // 2 秒轮询和手动刷新可能重叠；只接受最后一次请求的结果，避免旧快照覆盖新状态。
    const seq = ++loadSeq.current;
    try {
      const next = await taskCenter(limit);
      if (seq !== loadSeq.current) return { ok: false, stale: true };
      setCenter(next);
      loadErrorShown.current = false;
      return { ok: true, stale: false };
    } catch (err) {
      if (seq !== loadSeq.current) return { ok: false, stale: true };
      if (!loadErrorShown.current) {
        loadErrorShown.current = true;
        notify.error(err);
      }
      return { ok: false, stale: false };
    } finally {
      if (seq === loadSeq.current) setLoaded(true);
    }
  }, [notify]);

  useEffect(() => {
    void load();
    if (!isTauri()) return;
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [load]);

  // 历史分页：先拿 100 场，按钮每次多取 100 场；轮询始终沿用当前已展开的数量，
  // 避免加载更多后下一次轮询又缩回 100 场。
  const loadMore = useCallback(async () => {
    // 双击「加载更多」时两次点击都会进到这里；用 ref 而不是 state 拦截，
    // 避免第二次在 setLoadingMore 生效前又把上限推进一页。
    if (loadingMoreRef.current) return;
    const previous = historyLimitRef.current;
    const next = Math.min(previous + HISTORY_PAGE, HISTORY_MAX);
    if (next <= previous) return;
    historyLimitRef.current = next;
    setHistoryLimit(next);
    setLoadingMore(true);
    loadingMoreRef.current = true;
    try {
      const result = await load(next);
      // 请求真的失败时把上限退回：否则下一次点击会从 200 直接跳到 300，
      // 中间那一页被永久跳过。旧快照（stale）说明另一拍已经用新上限取到数据，
      // 保留推进后的值，等下一拍继续。
      if (!result.ok && !result.stale) {
        historyLimitRef.current = previous;
        setHistoryLimit(previous);
      }
    } finally {
      loadingMoreRef.current = false;
      setLoadingMore(false);
    }
  }, [load]);

  const allRuns = useMemo(() => [...center.running, ...center.history], [center.running, center.history]);
  const selected = useMemo(() => allRuns.find((run) => run.id === selectedId) ?? null, [allRuns, selectedId]);

  const matches = useCallback(
    (run: RunView) =>
      (!platformFilter || run.room?.platform === platformFilter) &&
      (!roomFilter || run.room?.id === roomFilter),
    [platformFilter, roomFilter],
  );

  // 筛选变化后，已经打开的抽屉如果不再属于当前筛选，就关掉，避免
  // 背景列表显示斗鱼、右侧却还挂着哔哩哔哩的 Run。
  useEffect(() => {
    if (selectedId && (!selected || !matches(selected))) setSelectedId(null);
  }, [matches, selected, selectedId]);

  // 时间线按需读取：首屏不再携带上千场的事件。抽屉打开时先取最近 200 条，
  // 之后只有运行中的 Run 每 5 秒刷新；已结束的 Run 只取一次，避免无意义轮询。
  const [detailEvents, setDetailEvents] = useState<TaskEvent[]>([]);
  const [detailEventsHasMore, setDetailEventsHasMore] = useState(false);
  const [detailEventsLoading, setDetailEventsLoading] = useState(false);
  const [detailEventsLoadingMore, setDetailEventsLoadingMore] = useState(false);
  const [detailEventsError, setDetailEventsError] = useState(false);
  const detailActive = selected ? ACTIVE_RUN_STATES.has(selected.state) : false;
  const selectedIdRef = useRef(selectedId);
  selectedIdRef.current = selectedId;
  const lastDetailRun = useRef<string | null>(null);
  useEffect(() => {
    if (!selectedId || !isTauri()) {
      lastDetailRun.current = null;
      setDetailEvents([]);
      setDetailEventsHasMore(false);
      setDetailEventsLoading(false);
      setDetailEventsLoadingMore(false);
      setDetailEventsError(false);
      return;
    }
    let cancelled = false;
    const isNewRun = lastDetailRun.current !== selectedId;
    lastDetailRun.current = selectedId;
    if (isNewRun) {
      setDetailEvents([]);
      setDetailEventsHasMore(false);
      setDetailEventsLoading(true);
      setDetailEventsLoadingMore(false);
      setDetailEventsError(false);
    }
    const fetchLatest = async (replace: boolean) => {
      try {
        const page = await taskRunEvents(selectedId, undefined, EVENT_PAGE);
        if (cancelled) return;
        setDetailEvents((previous) => (replace ? page.events : mergeEvents(previous, page.events)));
        // 刷新只补最新事件，不能把用户已经翻完的更早页重新标成「还有更多」。
        setDetailEventsHasMore((previous) => (replace ? page.hasMore : previous));
        setDetailEventsError(false);
      } catch {
        if (!cancelled) setDetailEventsError(true);
      } finally {
        if (!cancelled && replace) setDetailEventsLoading(false);
      }
    };
    void fetchLatest(isNewRun);
    if (!detailActive) {
      return () => {
        cancelled = true;
      };
    }
    const timer = window.setInterval(() => void fetchLatest(false), 5_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [selectedId, detailActive]);

  const loadEarlierDetailEvents = async (): Promise<boolean> => {
    const oldest = detailEvents[0];
    const runId = selectedId;
    if (!runId || !oldest || detailEventsLoadingMore) return false;
    setDetailEventsLoadingMore(true);
    try {
      const page = await taskRunEvents(runId, { at: oldest.at, id: oldest.id }, EVENT_PAGE);
      if (selectedIdRef.current !== runId) return false;
      setDetailEvents((previous) => mergeEvents(page.events, previous));
      setDetailEventsHasMore(page.hasMore);
      setDetailEventsError(false);
      return page.events.length > 0;
    } catch {
      if (selectedIdRef.current === runId) setDetailEventsError(true);
      return false;
    } finally {
      if (selectedIdRef.current === runId) setDetailEventsLoadingMore(false);
    }
  };

  const activeRuns = useMemo(
    () =>
      center.running.filter(
        (run) =>
          matches(run) &&
          (run.state === "running" || run.state === "recording" || run.state === "finalizing"),
      ),
    [center.running, matches],
  );
  const queuedRuns = useMemo(
    () => center.running.filter((run) => run.state === "queued" && matches(run)),
    [center.running, matches],
  );
  const historyRuns = useMemo(() => center.history.filter(matches), [center.history, matches]);
  // 历史最多 1000 场，全量 DOM 在低配机器上首次布局开销明显。
  // 表格保持原生语义，用上下 spacer 行撑出滚动高度，只渲染视口附近的行。
  const historyStart = Math.min(
    historyRuns.length,
    Math.max(0, Math.floor(historyScrollTop / HISTORY_ROW) - HISTORY_OVERSCAN),
  );
  const historyEnd = Math.max(
    historyStart,
    Math.min(
      historyRuns.length,
      Math.ceil((historyScrollTop + HISTORY_VIEWPORT) / HISTORY_ROW) + HISTORY_OVERSCAN,
    ),
  );
  const visibleHistory = historyRuns.slice(historyStart, historyEnd);
  const incidents = useMemo(
    () =>
      center.incidents.filter((incident) => {
        if (platformFilter && incident.platform !== platformFilter) return false;
        if (roomFilter && incident.roomId !== roomFilter) return false;
        return true;
      }),
    [center.incidents, platformFilter, roomFilter],
  );
  const queuedCount = queuedRuns.length;
  const runCount = activeRuns.length + queuedCount;
  // 失败 Run 同时出现在「需要处理」和历史里，不能把两张卡算成两条记录。
  // 只有找不到对应 Run 的异常（例如登录失效）才是额外的一条。
  const orphanIncidentCount = useMemo(
    () =>
      incidents.filter((incident) => {
        if (incident.taskId) {
          return !allRuns.some((run) => run.tasks.some((task) => task.id === incident.taskId));
        }
        if (incident.roomId) return !allRuns.some((run) => run.room?.id === incident.roomId);
        return true;
      }).length,
    [allRuns, incidents],
  );

  // 平台/直播间筛选后，顶部统计卡必须跟着筛选走；否则会出现
  // 「列表 0 条，但统计仍显示 1 个房间在录」的自相矛盾。
  const shownSummary = useMemo(() => {
    if (!platformFilter && !roomFilter) return center.summary;
    const health = platformFilter
      ? center.platforms.find((item) => item.platform === platformFilter)
      : undefined;
    const room = roomFilter
      ? allRuns.map((run) => run.room).find((item) => item?.id === roomFilter)
      : undefined;
    const now = center.generatedAt || Math.floor(Date.now() / 1000);
    const recentFailed = historyRuns.filter(
      (run) =>
        (run.state === "failed" || run.state === "partial") &&
        run.endedAt !== null &&
        now - run.endedAt <= 86400,
    ).length;
    // 直播间筛选可能和平台筛选同时存在（从设置页跳转后又会点平台卡），
    // 此时房间必须同时属于该平台，否则统计会和列表一样归零。
    const roomMatches = !!room && (!platformFilter || room.platform === platformFilter);
    return {
      ...center.summary,
      rooms: roomFilter ? (roomMatches ? 1 : 0) : (health?.rooms ?? 0),
      live: roomFilter ? (roomMatches && room?.isLive ? 1 : 0) : (health?.live ?? 0),
      recording: activeRuns.filter((run) => run.state === "recording").length,
      queued: queuedCount,
      failed: recentFailed,
    };
  }, [
    activeRuns,
    allRuns,
    center.generatedAt,
    center.platforms,
    center.summary,
    historyRuns,
    platformFilter,
    queuedCount,
    roomFilter,
  ]);

  const refresh = async () => {
    if ((await load()).ok) notify.success(t("tasks.refreshed"));
  };

  const cancel = async (task: TaskView) => {
    if (!task.sessionId) return;
    setBusyId(task.id);
    try {
      await postCancel(task.sessionId);
      notify.info(t("tasks.cancelRequested"));
      await load();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusyId(null);
    }
  };

  const retry = async (task: TaskView) => {
    if (!task.sessionId) return;
    setBusyId(task.id);
    try {
      if (task.kind === "finalize") {
        await finalizeRetry(task.sessionId);
        notify.info(t("tasks.retryFinalizeRequested"));
      } else {
        await postRetry(task.sessionId);
        notify.info(t("tasks.retryRequested"));
      }
      await load();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusyId(null);
    }
  };

  const copyDiagnostics = async (run: RunView) => {
    const text = [
      `run: ${run.id}`,
      `state: ${run.state}${run.stage !== run.state ? " / " + run.stage : ""}`,
      `platform: ${run.room?.platform ?? "-"}`,
      `room: ${run.room?.id ?? "-"} (${run.room?.roomKey ?? "-"})`,
      `host: ${run.room?.hostName ?? "-"}`,
      `started: ${formatTime(run.startedAt)}`,
      `ended: ${formatTime(run.endedAt)}`,
      `duration: ${formatDuration(run.durationMs)}`,
      `bytes: ${run.bytes}`,
      `quality: ${run.quality ?? "-"}`,
      `stages: ${run.stages.map((stage) => stage.key + "=" + stage.state).join(", ")}`,
      `tasks: ${run.tasks.map((task) => task.id + "=" + task.state + (task.stage ? "/" + task.stage : "")).join(", ")}`,
    ].join("\n");
    try {
      await navigator.clipboard.writeText(text);
      notify.success(t("tasks.copied"));
    } catch (err) {
      notify.error(err);
    }
  };

  const incidentRun = (incident: Incident): RunView | undefined =>
    incident.taskId
      ? allRuns.find((run) => run.tasks.some((task) => task.id === incident.taskId))
      : incident.roomId
        ? allRuns.find((run) => run.room?.id === incident.roomId)
        : undefined;

  const showRunning = filter === "all" || filter === "running";
  const showQueued = filter === "all" || filter === "queued";
  const showIncidents = filter === "all" || filter === "incidents";
  const showHistory = filter === "all";

  return (
    <PageShell>
      <PageHeader
        title={t("tasks.title")}
        subtitle={t("tasks.subtitle")}
        leading={
          <div className="flex items-center gap-2 font-mono text-[10px] uppercase tracking-[0.14em] text-faint">
            <span className="h-1.5 w-1.5 rounded-full bg-accent" />
            <span>{t("tasks.eyebrow")}</span>
          </div>
        }
        actions={
          <div className="flex items-center gap-2">
            <Button size="sm" variant="secondary" onClick={() => setSearchOpen(true)}>
              {t("tasks.eventSearch")}
            </Button>
            <Button size="sm" variant="secondary" onClick={() => void refresh()}>
              {t("tasks.refresh")}
            </Button>
          </div>
        }
      />

      <PageToolbar>
        <PageChip active={filter === "all"} onClick={() => setFilter("all")}>
          {t("tasks.filterAll")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{runCount + historyRuns.length + orphanIncidentCount}</span>
        </PageChip>
        <PageChip active={filter === "running"} onClick={() => setFilter("running")}>
          {t("tasks.filterRunning")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{activeRuns.length}</span>
        </PageChip>
        <PageChip active={filter === "queued"} onClick={() => setFilter("queued")}>
          {t("tasks.filterQueued")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{queuedCount}</span>
        </PageChip>
        <PageChip active={filter === "incidents"} onClick={() => setFilter("incidents")}>
          {t("tasks.filterIncidents")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{incidents.length}</span>
        </PageChip>
        {platformFilter ? (
          <PageChip active onClick={() => setPlatformFilter(null)}>
            {platformName(t, platformFilter)}
            <span className="ml-1.5 opacity-60">×</span>
          </PageChip>
        ) : null}
        {roomFilter ? (
          <PageChip active onClick={() => setRoomFilter(null)}>
            {t("tasks.filterRoom")}
            <span className="ml-1.5 opacity-60">×</span>
          </PageChip>
        ) : null}
      </PageToolbar>

      <PageBody className="pb-8">
        {loaded && center.summary.rooms === 0 && allRuns.length === 0 && center.incidents.length === 0 ? (
          <PageEmpty
            className="flex-1"
            title={t("tasks.emptyTitle")}
            description={t("tasks.emptyBody")}
            icon={<EmptyArt kind="tasks" />}
            action={
              onOpenMonitor ? (
                <Button size="sm" variant="secondary" onClick={onOpenMonitor}>
                  {t("tasks.emptyAction")}
                </Button>
              ) : undefined
            }
          />
        ) : (
          <>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-5">
              {[
                { label: t("tasks.summaryRooms"), value: shownSummary.rooms, tone: "text-ink" },
                { label: t("tasks.summaryLive"), value: shownSummary.live, tone: "text-live" },
                { label: t("tasks.summaryRecording"), value: shownSummary.recording, tone: "text-rec" },
                { label: t("tasks.summaryQueued"), value: shownSummary.queued, tone: "text-warn" },
                { label: t("tasks.summaryFailed"), value: shownSummary.failed, tone: shownSummary.failed > 0 ? "text-rec" : "text-ink" },
              ].map((item) => (
                <div key={item.label} className="rounded-panel border border-line bg-surface px-3 py-2.5">
                  <div className="text-[10px] text-faint">{item.label}</div>
                  <div className={cx("mt-0.5 text-[18px] font-semibold leading-6 tabular-nums", item.tone)}>{item.value}</div>
                </div>
              ))}
            </div>

            <section className="mt-4">
              <div className="mb-2 flex items-baseline gap-2">
                <h2 className="text-[11px] font-semibold uppercase tracking-[0.12em] text-faint">{t("tasks.platformTitle")}</h2>
                <span className="text-[10.5px] text-faint">{t("tasks.platformHint")}</span>
              </div>
              <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
                {center.platforms.map((health) => (
                  <PlatformCard
                    key={health.platform}
                    health={health}
                    active={platformFilter === health.platform}
                    onSelect={() => setPlatformFilter((current) => (current === health.platform ? null : health.platform))}
                  />
                ))}
              </div>
              <div className="mt-2 flex min-w-0 items-center gap-3 rounded-panel border border-line bg-surface px-3.5 py-2 text-[10.5px] text-muted">
                <span className="shrink-0 font-mono uppercase tracking-[0.12em] text-faint">{t("tasks.systemTitle")}</span>
                <span className="inline-flex shrink-0 items-center gap-1.5">
                  <span className={cx("h-1.5 w-1.5 rounded-full", center.system.ffmpegOk ? "bg-live" : "bg-rec")} />
                  {center.system.ffmpegOk ? t("tasks.systemFfmpegOk") : t("tasks.systemFfmpegMissing")}
                </span>
                <span className="shrink-0 text-faint">·</span>
                <span className={cx("shrink-0", center.system.diskLow && "text-warn")}>
                  {t("tasks.systemDisk")}{" "}
                  {center.system.diskFreeBytes != null ? formatBytes(center.system.diskFreeBytes) : "—"}
                </span>
                {center.system.mediaRoot ? (
                  <span className="min-w-0 flex-1 truncate text-right text-[10px] text-faint" title={center.system.mediaRoot}>
                    {center.system.mediaRoot}
                  </span>
                ) : null}
              </div>
            </section>

            {showIncidents && incidents.length > 0 ? (
              <section className="mt-5">
                <div className="mb-2 flex items-baseline gap-2">
                  <h2 className="text-[12.5px] font-semibold text-ink">{t("tasks.incidentsTitle")}</h2>
                  <span className="text-[11px] tabular-nums text-faint">{incidents.length}</span>
                  <span className="ml-auto text-[10.5px] text-faint">{t("tasks.incidentsHint")}</span>
                </div>
                <div className="overflow-hidden rounded-panel border border-line bg-surface">
                  {incidents.map((incident) => (
                    <IncidentRow
                      key={incident.id}
                      incident={incident}
                      run={incidentRun(incident)}
                      busy={busyId != null}
                      onRetry={(task) => void retry(task)}
                      onOpenSettings={(page) => onOpenSettings?.(page)}
                      onOpenRun={(run) => setSelectedId(run.id)}
                      onOpenRoom={(roomId) => onOpenRoom?.(roomId)}
                    />
                  ))}
                </div>
              </section>
            ) : null}

            {showIncidents && filter === "incidents" && incidents.length === 0 ? (
              <section className="mt-5">
                <div className="mb-2 flex items-baseline gap-2">
                  <h2 className="text-[12.5px] font-semibold text-ink">{t("tasks.incidentsTitle")}</h2>
                  <span className="text-[11px] tabular-nums text-faint">0</span>
                </div>
                <div className="rounded-panel border border-line bg-surface px-5 py-7 text-center">
                  <p className="text-[12.5px] text-muted">{t("tasks.emptyIncidents")}</p>
                  <p className="mt-1 text-[11px] text-faint">{t("tasks.emptyIncidentsHint")}</p>
                </div>
              </section>
            ) : null}

            {showRunning ? (
              <section className="mt-5">
                <div className="mb-2 flex items-baseline gap-2">
                  <h2 className="text-[12.5px] font-semibold text-ink">{t("tasks.running")}</h2>
                  <span className="text-[11px] tabular-nums text-faint">{activeRuns.length}</span>
                </div>
                {activeRuns.length === 0 ? (
                  <div className="rounded-panel border border-line bg-surface px-5 py-7 text-center">
                    <p className="text-[12.5px] text-muted">{t("tasks.emptyRunning")}</p>
                    <p className="mt-1 text-[11px] text-faint">{t("tasks.emptyRunningHint")}</p>
                  </div>
                ) : (
                  <div className="space-y-2">
                    {activeRuns.map((run) => (
                      <RunCard
                        key={run.id}
                        run={run}
                        now={center.generatedAt || Math.floor(Date.now() / 1000)}
                        busy={busyId != null}
                        onCancel={(task) => void cancel(task)}
                        onOpen={() => setSelectedId(run.id)}
                      />
                    ))}
                  </div>
                )}
              </section>
            ) : null}

            {showQueued && (queuedRuns.length > 0 || filter === "queued") ? (
              <section className="mt-5">
                <div className="mb-2 flex items-baseline gap-2">
                  <h2 className="text-[12.5px] font-semibold text-ink">{t("tasks.queuedTitle")}</h2>
                  <span className="text-[11px] tabular-nums text-faint">{queuedCount}</span>
                </div>
                {queuedRuns.length === 0 ? (
                  <div className="rounded-panel border border-line bg-surface px-5 py-4 text-[11.5px] text-faint">
                    {t("tasks.emptyQueued")}
                  </div>
                ) : (
                  <div className="overflow-hidden rounded-panel border border-line bg-surface">
                    {queuedRuns.map((run) => (
                      <QueuedRow
                        key={run.id}
                        run={run}
                        now={center.generatedAt || Math.floor(Date.now() / 1000)}
                        busy={busyId != null}
                        onCancel={(task) => void cancel(task)}
                        onOpen={() => setSelectedId(run.id)}
                      />
                    ))}
                  </div>
                )}
              </section>
            ) : null}

            {showHistory ? (
              <section className="mt-5">
                <div className="mb-2 flex items-baseline gap-2">
                  <h2 className="text-[12.5px] font-semibold text-ink">{t("tasks.history")}</h2>
                  <span className="text-[11px] tabular-nums text-faint">{historyRuns.length}</span>
                </div>
                {!loaded ? (
                  <div className="rounded-panel border border-line bg-surface px-5 py-8 text-center text-[12px] text-faint">
                    {t("tasks.loading")}
                  </div>
                ) : historyRuns.length === 0 ? (
                  <div className="rounded-panel border border-line bg-surface px-5 py-8 text-center">
                    <p className="text-[12.5px] text-muted">{t("tasks.emptyHistory")}</p>
                    <p className="mt-1 text-[11px] text-faint">{t("tasks.emptyHistoryHint")}</p>
                  </div>
                ) : (
                  <div className="overflow-hidden rounded-panel border border-line bg-surface">
                    <div
                      ref={historyScrollRef}
                      onScroll={(event) => setHistoryScrollTop(event.currentTarget.scrollTop)}
                      className="max-h-[420px] overflow-auto"
                    >
                    <table className="w-full table-fixed border-collapse">
                      <thead className="sticky top-0 z-10 bg-surface">
                        <tr className="border-b border-line text-left text-[10px] text-faint">
                          <th className="w-[36%] px-4 py-2 font-medium">{t("tasks.colRoom")}</th>
                          <th className="w-[13%] px-2 py-2 font-medium">{t("tasks.colDuration")}</th>
                          <th className="w-[13%] px-2 py-2 font-medium">{t("tasks.colBytes")}</th>
                          <th className="w-[16%] px-2 py-2 font-medium">{t("tasks.colEnded")}</th>
                          <th className="w-[22%] px-4 py-2 text-right font-medium">{t("tasks.colState")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {historyStart > 0 ? (
                          <tr aria-hidden="true">
                            <td colSpan={5} style={{ height: historyStart * HISTORY_ROW, padding: 0, border: 0 }} />
                          </tr>
                        ) : null}
                        {visibleHistory.map((run) => (
                          <tr
                            key={run.id}
                            tabIndex={0}
                            onClick={() => setSelectedId(run.id)}
                            onKeyDown={(event) => {
                              if (event.key !== "Enter" || event.target !== event.currentTarget) return;
                              event.preventDefault();
                              setSelectedId(run.id);
                            }}
                            className="h-14 cursor-pointer border-b border-line hover:bg-raised/50 focus-visible:bg-raised/60 focus-visible:outline-none"
                          >
                            <td className="min-w-0 px-4 py-1.5">
                              <div className="flex min-w-0 items-center gap-2">
                                <span className="shrink-0 text-[10px] text-faint">{platformName(t, run.room?.platform)}</span>
                                <span className="min-w-0 flex-1 truncate text-[11.5px] text-ink">
                                  {run.room?.hostName || run.room?.title || t("tasks.detailNoRoom")}
                                </span>
                              </div>
                              <div className="mt-0.5 truncate text-[10px] text-faint">
                                {run.room?.title ?? "—"}
                              </div>
                            </td>
                            <td className="truncate px-2 py-1.5 text-[10.5px] tabular-nums text-muted">{formatDuration(run.durationMs)}</td>
                            <td className="truncate px-2 py-1.5 text-[10.5px] tabular-nums text-muted">{formatBytes(run.bytes)}</td>
                            <td className="truncate px-2 py-1.5 text-[10.5px] tabular-nums text-faint">{formatTime(run.endedAt ?? run.startedAt)}</td>
                            <td className="px-4 py-1.5 text-right">
                              <div className="flex items-center justify-end gap-1.5">
                                <Chip tone={stateTone(run.state)} dot>
                                  {stateLabel(t, run.state)}
                                </Chip>
                                <button
                                  type="button"
                                  aria-label={`${t("tasks.detailOpen")} ${run.room?.hostName || run.room?.title || run.id}`}
                                  onClick={(event) => {
                                    event.stopPropagation();
                                    setSelectedId(run.id);
                                  }}
                                  className="focus-ring flex h-6 w-6 shrink-0 items-center justify-center rounded-ctl text-[14px] leading-none text-faint transition-colors hover:bg-raised hover:text-ink"
                                >
                                  <span aria-hidden="true">›</span>
                                </button>
                              </div>
                            </td>
                          </tr>
                        ))}
                        {historyEnd < historyRuns.length ? (
                          <tr aria-hidden="true">
                            <td colSpan={5} style={{ height: (historyRuns.length - historyEnd) * HISTORY_ROW, padding: 0, border: 0 }} />
                          </tr>
                        ) : null}
                      </tbody>
                    </table>
                    </div>
                  </div>
                )}
                {loaded && center.historyHasMore ? (
                  <div className="mt-3 flex justify-center">
                    {historyLimit < HISTORY_MAX ? (
                      <Button size="sm" variant="secondary" disabled={loadingMore} onClick={() => void loadMore()}>
                        {loadingMore ? t("tasks.loadingMore") : t("tasks.loadMore")}
                      </Button>
                    ) : (
                      <span className="text-[10.5px] text-faint">{t("tasks.historyMax", { n: HISTORY_MAX })}</span>
                    )}
                  </div>
                ) : null}
              </section>
            ) : null}
          </>
        )}
      </PageBody>

      <EventSearchDialog
        open={searchOpen}
        onOpenChange={setSearchOpen}
        initialPlatform={platformFilter}
        onOpenFiles={onOpenRoomFiles}
      />

      {selected ? (
        <DetailDrawer
          run={selected}
          events={detailEvents}
          eventsHasMore={detailEventsHasMore}
          eventsLoading={detailEventsLoading}
          eventsLoadingMore={detailEventsLoadingMore}
          eventsError={detailEventsError}
          onLoadEarlier={loadEarlierDetailEvents}
          busy={busyId != null}
          canOpenFiles={!!selected.room && !!onOpenRoomFiles}
          onClose={() => setSelectedId(null)}
          onCancel={(task) => void cancel(task)}
          onRetry={(task) => void retry(task)}
          onOpenFiles={() => {
            if (selected.room && onOpenRoomFiles) onOpenRoomFiles(selected.room.id);
          }}
          onCopy={() => void copyDiagnostics(selected)}
        />
      ) : null}
    </PageShell>
  );
}
