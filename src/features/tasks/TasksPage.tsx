import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, cx } from "../../components/ui/Controls";
import {
  describeError,
  isTauri,
  postCancel,
  taskBoard,
  type TaskBoard,
  type TaskRecord,
} from "../../lib/ipc";

const KIND_KEYS: Record<string, string> = {
  record: "tasks.kindRecord",
  post: "tasks.kindPost",
  remux: "tasks.kindRemux",
  merge: "tasks.kindMerge",
  danmaku: "tasks.kindDanmaku",
};

const STATE_KEYS: Record<string, string> = {
  running: "tasks.stateRunning",
  done: "tasks.stateDone",
  failed: "tasks.stateFailed",
  cancelled: "tasks.stateCancelled",
  interrupted: "tasks.stateInterrupted",
};

const STATE_STYLE: Record<string, string> = {
  running: "bg-accent-soft text-accent",
  done: "bg-ok/12 text-ok",
  failed: "bg-rec/12 text-rec",
  cancelled: "bg-raised text-muted",
  interrupted: "bg-warn/12 text-warn",
};

function formatTime(seconds: number): string {
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

function progressOf(task: TaskRecord): number {
  if (task.state === "done") return 100;
  const value = Number(task.percent);
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, Math.round(value)));
}

function canCancel(task: TaskRecord): boolean {
  return (
    task.state === "running" &&
    !!task.sessionId &&
    (task.kind === "post" || task.kind === "remux" || task.kind === "merge")
  );
}

export default function TasksPage() {
  const { t } = useTranslation();
  const [board, setBoard] = useState<TaskBoard>({ running: [], history: [] });
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    if (!isTauri()) {
      setLoaded(true);
      return;
    }
    try {
      const next = await taskBoard(100);
      setBoard(next);
      setError(null);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void load();
    if (!isTauri()) return;
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [load]);

  const running = useMemo(
    () => [...board.running].sort((a, b) => b.startedAt - a.startedAt),
    [board.running],
  );
  const history = useMemo(
    () => [...board.history].sort((a, b) => b.startedAt - a.startedAt),
    [board.history],
  );

  const cancel = async (task: TaskRecord) => {
    if (!task.sessionId) return;
    setBusyId(task.id);
    setError(null);
    try {
      await postCancel(task.sessionId);
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-3 px-5 pb-3 pt-4">
        <div className="min-w-0">
          <h1 className="text-[21px] font-semibold leading-7 tracking-[-0.01em] text-ink">{t("tasks.title")}</h1>
          <p className="mt-0.5 text-[11.5px] leading-5 text-muted">{t("tasks.subtitle")}</p>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <Button size="sm" variant="secondary" onClick={() => void load()}>
            {t("tasks.refresh")}
          </Button>
        </div>
      </div>

      {error ? (
        <div className="mx-5 mb-2 rounded-ctl border border-rec/30 bg-rec/8 px-3 py-2 text-[12px] text-rec">
          {error}
        </div>
      ) : null}

      <div className="min-h-0 flex-1 overflow-auto px-5 pb-8">
        <section>
          <div className="mb-2 flex items-baseline gap-2">
            <h2 className="text-[13px] font-semibold text-ink">{t("tasks.running")}</h2>
            <span className="text-[11px] tabular-nums text-faint">{running.length}</span>
          </div>
          {running.length === 0 ? (
            <div className="rounded-panel border border-line bg-surface px-5 py-8 text-center">
              <p className="text-[13px] text-muted">{t("tasks.emptyRunning")}</p>
              <p className="mt-1 text-[11.5px] text-faint">{t("tasks.emptyRunningHint")}</p>
            </div>
          ) : (
            <div className="space-y-2">
              {running.map((task) => {
                const percent = progressOf(task);
                return (
                  <div key={task.id} className="rounded-panel border border-line bg-surface px-4 py-3">
                    <div className="flex items-center gap-2">
                      <span className="rounded-full bg-raised px-2 py-0.5 text-[10.5px] font-medium text-muted">
                        {t(KIND_KEYS[task.kind] ?? "tasks.kindPost")}
                      </span>
                      <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-ink" title={task.title}>
                        {task.title}
                      </span>
                      <span className={cx("rounded-full px-2 py-0.5 text-[10.5px] font-medium", STATE_STYLE.running)}>
                        {t(STATE_KEYS.running)}
                      </span>
                      {canCancel(task) ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busyId === task.id}
                          onClick={() => void cancel(task)}
                        >
                          {t("tasks.cancel")}
                        </Button>
                      ) : null}
                    </div>
                    <div className="mt-2 h-1 overflow-hidden rounded-full bg-raised">
                      <div
                        className="h-full rounded-full bg-accent transition-[width] duration-500"
                        style={{ width: percent + "%" }}
                      />
                    </div>
                    <div className="mt-1.5 flex items-center gap-3 text-[10.5px] text-faint">
                      <span className="tabular-nums">{percent}%</span>
                      <span className="truncate">{task.message || task.stage || t("tasks.stagePending")}</span>
                      <span className="ml-auto shrink-0 tabular-nums">{formatTime(task.startedAt)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </section>

        <section className="mt-7">
          <div className="mb-2 flex items-baseline gap-2">
            <h2 className="text-[13px] font-semibold text-ink">{t("tasks.history")}</h2>
            <span className="text-[11px] tabular-nums text-faint">{history.length}</span>
          </div>
          {!loaded ? (
            <div className="rounded-panel border border-line bg-surface px-5 py-8 text-center text-[12px] text-faint">
              {t("tasks.loading")}
            </div>
          ) : history.length === 0 ? (
            <div className="rounded-panel border border-line bg-surface px-5 py-8 text-center">
              <p className="text-[13px] text-muted">{t("tasks.emptyHistory")}</p>
              <p className="mt-1 text-[11.5px] text-faint">{t("tasks.emptyHistoryHint")}</p>
            </div>
          ) : (
            <div className="overflow-hidden rounded-panel border border-line bg-surface">
              {history.map((task) => (
                <div
                  key={task.id}
                  className="flex items-center gap-3 border-b border-line px-4 py-2.5 last:border-b-0"
                >
                  <span className="w-[54px] shrink-0 text-[10.5px] text-faint">
                    {t(KIND_KEYS[task.kind] ?? "tasks.kindPost")}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[12px] text-ink" title={task.title}>
                    {task.title}
                  </span>
                  {task.message ? (
                    <span className="hidden max-w-[380px] truncate text-[10.5px] text-faint md:block" title={task.message}>
                      {task.message}
                    </span>
                  ) : null}
                  <span className={cx("shrink-0 rounded-full px-2 py-0.5 text-[10.5px] font-medium", STATE_STYLE[task.state] ?? STATE_STYLE.cancelled)}>
                    {t(STATE_KEYS[task.state] ?? "tasks.stateCancelled")}
                  </span>
                  <span className="w-[92px] shrink-0 text-right text-[10.5px] tabular-nums text-faint">
                    {formatTime(task.startedAt)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
