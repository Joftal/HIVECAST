import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Button, Chip, cx } from "../../components/ui/Controls";
import EmptyArt from "../../components/ui/EmptyArt";
import PageEmpty from "../../components/ui/PageEmpty";
import { PageBody, PageChip, PageHeader, PageShell, PageToolbar } from "../../components/ui/PageShell";
import {
  isTauri,
  postCancel,
  postRetry,
  taskCenter,
  type Incident,
  type PlatformHealth,
  type RunRoom,
  type RunView,
  type TaskCenter,
  type TaskView,
} from "../../lib/ipc";
import { useNotify } from "../../lib/notify";

const EMPTY_CENTER: TaskCenter = {
  generatedAt: 0,
  summary: { rooms: 0, live: 0, recording: 0, queued: 0, running: 0, failed: 0, incidents: 0 },
  platforms: [],
  system: { ffmpegOk: true, diskFreeBytes: null, diskLow: false, mediaRoot: "" },
  running: [],
  queued: [],
  history: [],
  incidents: [],
};

const STAGE_KEYS: Record<string, string> = {
  record: "tasks.stageRecord",
  finalize: "tasks.stageFinalize",
  post: "tasks.stagePost",
  commit: "tasks.stageCommit",
  merging: "tasks.stageMerging",
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

function queuedTask(run: RunView): TaskView | undefined {
  return run.tasks.find((task) => task.state === "queued");
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
      task.state === "running" &&
      !!task.sessionId &&
      (task.kind === "post" || task.kind === "remux" || task.kind === "merge"),
  );
}

function failedTask(run: RunView): TaskView | undefined {
  return run.tasks.find((task) => task.state === "failed" && task.sessionId);
}

function clipText(t: (key: string, options?: Record<string, unknown>) => string, task: TaskView): string | null {
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
              {stage.state === "done" ? "✓" : stage.state === "failed" ? "!" : stage.state === "active" ? "•" : ""}
            </span>
            <span
              className={cx(
                "shrink-0 text-[11px]",
                stage.state === "active"
                  ? "font-medium text-accent-text"
                  : stage.state === "failed"
                    ? "text-rec"
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

function QueuedRow({ run, now, onOpen }: { run: RunView; now: number; onOpen: () => void }) {
  const { t } = useTranslation();
  const task = queuedTask(run);
  const wait = task ? Math.max(0, now - (task.queuedAt ?? task.startedAt)) : 0;
  return (
    <button
      type="button"
      onClick={onOpen}
      className="focus-ring flex w-full items-center gap-3 border-b border-line px-4 py-2.5 text-left last:border-b-0 hover:bg-raised/50"
    >
      <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-warn" />
      <span className="min-w-0 flex-1 truncate text-[12px] text-ink">
        {run.room?.hostName || run.room?.title || t("tasks.detailNoRoom")}
      </span>
      <span className="hidden shrink-0 text-[10.5px] text-muted sm:block">{t("tasks.waitingSlot", { time: formatElapsed(wait) })}</span>
      <span className="w-[92px] shrink-0 truncate text-right text-[10px] text-faint">{platformName(t, run.room?.platform)}</span>
    </button>
  );
}

function IncidentRow({
  incident,
  run,
  busy,
  onRetry,
  onOpenSettings,
}: {
  incident: Incident;
  run: RunView | undefined;
  busy: boolean;
  onRetry: (task: TaskView) => void;
  onOpenSettings: (page: "account" | "storage" | "system") => void;
}) {
  const { t } = useTranslation();
  const task = incident.taskId ? run?.tasks.find((item) => item.id === incident.taskId) : undefined;
  const title =
    incident.code === "login_expired"
      ? t("tasks.incidentLoginExpiredPlatform", { platform: platformName(t, incident.platform) })
      : incident.code === "monitor_backoff" || incident.code === "record_failed"
        ? run?.room?.hostName || incident.detail || t(INCIDENT_KEYS[incident.code] ?? "tasks.incidentUnknown")
        : t(INCIDENT_KEYS[incident.code] ?? "tasks.incidentUnknown");
  const detail =
    incident.code === "login_expired"
      ? t("tasks.incidentLoginExpiredDetail")
      : incident.code === "monitor_backoff"
        ? incident.detail || t("tasks.incidentMonitorBackoffDetail")
        : incident.code === "disk_low"
          ? t("tasks.incidentDiskLowDetail")
          : incident.detail;
  const action =
    incident.action === "retry_post" && task?.sessionId
      ? { label: t("tasks.retry"), run: () => onRetry(task) }
      : incident.action === "open_account"
        ? { label: t("tasks.actionOpenAccount"), run: () => onOpenSettings("account") }
        : incident.action === "open_storage"
          ? { label: t("tasks.actionOpenStorage"), run: () => onOpenSettings("storage") }
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
  busy,
  canOpenFiles,
  onClose,
  onCancel,
  onRetry,
  onOpenFiles,
  onCopy,
}: {
  run: RunView;
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
                        : "border-line bg-surface text-faint";
                return (
                  <div key={stage.key} className="flex gap-3">
                    <div className="flex flex-col items-center">
                      <span className={cx("flex h-5 w-5 shrink-0 items-center justify-center rounded-full border text-[10px] font-semibold", tone)}>
                        {stage.state === "done" ? "✓" : stage.state === "failed" ? "!" : stage.state === "active" ? "•" : index + 1}
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
                <dt className="w-[64px] shrink-0 text-faint">{t("tasks.detailSession")}</dt>
                <dd className="min-w-0 flex-1 break-all text-muted">{run.id}</dd>
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

export default function TasksPage({
  seed,
  onSeedApplied,
  onOpenSettings,
  onOpenRoomFiles,
  onOpenMonitor,
}: {
  seed?: { roomId: string; nonce: number } | null;
  onSeedApplied?: () => void;
  onOpenSettings?: (page: "account" | "storage" | "system") => void;
  onOpenRoomFiles?: (roomId: string) => void;
  onOpenMonitor?: () => void;
}) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [center, setCenter] = useState<TaskCenter>(EMPTY_CENTER);
  const [loaded, setLoaded] = useState(false);
  const [filter, setFilter] = useState<Filter>("all");
  const [platformFilter, setPlatformFilter] = useState<string | null>(null);
  const [roomFilter, setRoomFilter] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const loadErrorShown = useRef(false);

  useEffect(() => {
    if (!seed) return;
    setRoomFilter(seed.roomId);
    setFilter("all");
    onSeedApplied?.();
    // 只响应新的跳转请求，避免 onSeedApplied 每次渲染换引用导致循环。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seed?.nonce]);

  const load = useCallback(async () => {
    if (!isTauri()) {
      setLoaded(true);
      return true;
    }
    try {
      const next = await taskCenter(100);
      setCenter(next);
      loadErrorShown.current = false;
      return true;
    } catch (err) {
      if (!loadErrorShown.current) {
        loadErrorShown.current = true;
        notify.error(err);
      }
      return false;
    } finally {
      setLoaded(true);
    }
  }, [notify]);

  useEffect(() => {
    void load();
    if (!isTauri()) return;
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [load]);

  const allRuns = useMemo(() => [...center.running, ...center.history], [center.running, center.history]);
  const selected = useMemo(() => allRuns.find((run) => run.id === selectedId) ?? null, [allRuns, selectedId]);

  const matches = useCallback(
    (run: RunView) =>
      (!platformFilter || run.room?.platform === platformFilter) &&
      (!roomFilter || run.room?.id === roomFilter),
    [platformFilter, roomFilter],
  );

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

  const refresh = async () => {
    if (await load()) notify.success(t("tasks.refreshed"));
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
      await postRetry(task.sessionId);
      notify.info(t("tasks.retryRequested"));
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
          <Button size="sm" variant="secondary" onClick={() => void refresh()}>
            {t("tasks.refresh")}
          </Button>
        }
      />

      <PageToolbar>
        <PageChip active={filter === "all"} onClick={() => setFilter("all")}>
          {t("tasks.filterAll")}
          <span className="tnum ml-1.5 text-[11.5px] opacity-60">{runCount + historyRuns.length + incidents.length}</span>
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
                { label: t("tasks.summaryRooms"), value: center.summary.rooms, tone: "text-ink" },
                { label: t("tasks.summaryLive"), value: center.summary.live, tone: "text-live" },
                { label: t("tasks.summaryRecording"), value: center.summary.recording, tone: "text-rec" },
                { label: t("tasks.summaryQueued"), value: center.summary.queued, tone: "text-warn" },
                { label: t("tasks.summaryFailed"), value: center.summary.failed, tone: center.summary.failed > 0 ? "text-rec" : "text-ink" },
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
                    />
                  ))}
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
                    <table className="w-full table-fixed border-collapse">
                      <thead>
                        <tr className="border-b border-line text-left text-[10px] text-faint">
                          <th className="w-[36%] px-4 py-2 font-medium">{t("tasks.colRoom")}</th>
                          <th className="w-[13%] px-2 py-2 font-medium">{t("tasks.colDuration")}</th>
                          <th className="w-[13%] px-2 py-2 font-medium">{t("tasks.colBytes")}</th>
                          <th className="w-[16%] px-2 py-2 font-medium">{t("tasks.colEnded")}</th>
                          <th className="w-[22%] px-4 py-2 text-right font-medium">{t("tasks.colState")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {historyRuns.map((run) => (
                          <tr
                            key={run.id}
                            onClick={() => setSelectedId(run.id)}
                            className="cursor-pointer border-b border-line last:border-b-0 hover:bg-raised/50"
                          >
                            <td className="min-w-0 px-4 py-2.5">
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
                            <td className="truncate px-2 py-2.5 text-[10.5px] tabular-nums text-muted">{formatDuration(run.durationMs)}</td>
                            <td className="truncate px-2 py-2.5 text-[10.5px] tabular-nums text-muted">{formatBytes(run.bytes)}</td>
                            <td className="truncate px-2 py-2.5 text-[10.5px] tabular-nums text-faint">{formatTime(run.endedAt ?? run.startedAt)}</td>
                            <td className="px-4 py-2.5 text-right">
                              <Chip tone={stateTone(run.state)} dot>
                                {stateLabel(t, run.state)}
                              </Chip>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </section>
            ) : null}
          </>
        )}
      </PageBody>

      {selected ? (
        <DetailDrawer
          run={selected}
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
