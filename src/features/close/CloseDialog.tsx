import { useEffect } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Button, Switch } from "../../components/ui/Controls";
import type { ActiveTask } from "../../lib/ipc";

type Props = {
  tasks: ActiveTask[];
  armed: boolean;
  busy: boolean;
  onCancel: () => void;
  onArm: () => void;
  onDisarm: () => void;
  onQuitNow: () => void;
};

const KIND_KEY: Record<string, string> = {
  record: "close.kindRecord",
  remux: "close.kindRemux",
  merge: "close.kindMerge",
  danmaku: "close.kindDanmaku",
};

/// 退出被任务拦下时的提示；任务清空后同一块面板切成「可以安全退出」。
export default function CloseDialog({
  tasks,
  armed,
  busy,
  onCancel,
  onArm,
  onDisarm,
  onQuitNow,
}: Props) {
  const { t } = useTranslation();
  const ready = tasks.length === 0;

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onCancel]);

  return createPortal(
    <div className="fixed inset-0 z-[80] flex items-center justify-center px-6">
      <div className="absolute inset-0 animate-fade-in bg-black/35 backdrop-blur-[2px]" onClick={busy ? undefined : onCancel} />
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="close-dialog-title"
        className="relative w-[min(460px,92vw)] animate-pop rounded-panel border border-line bg-surface p-5 shadow-pop"
      >
        <div className="flex items-start gap-3">
          <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-accent-soft text-accent-text">
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
              <circle cx="8" cy="8" r="6.2" />
              <path d="M8 4.8v3.8M8 11.2v.1" strokeLinecap="round" />
            </svg>
          </span>
          <div className="min-w-0 flex-1">
            <h2 id="close-dialog-title" className="text-[14px] font-semibold text-ink">
              {ready ? t("close.readyTitle") : t("close.title")}
            </h2>
            <p className="mt-1 text-[11.5px] leading-4 text-muted">
              {ready ? t("close.readyBody") : t("close.body", { n: tasks.length })}
            </p>
          </div>
        </div>

        {ready ? null : (
          <ul className="mt-4 max-h-[220px] divide-y divide-line overflow-auto rounded-[8px] border border-line">
            {tasks.map((task) => (
              <li key={task.id} className="flex items-center gap-3 px-3 py-2.5">
                <span className="shrink-0 rounded-[4px] bg-raised px-1.5 py-0.5 text-[10px] text-muted">
                  {t(KIND_KEY[task.kind] ?? "close.kindTask")}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[12px] text-ink">{task.title}</span>
                  {task.detail ? (
                    <span className="mt-0.5 block truncate text-[10.5px] text-faint">{task.detail}</span>
                  ) : null}
                </span>
              </li>
            ))}
          </ul>
        )}

        {ready ? null : (
          <label className="mt-4 flex items-center justify-between gap-3 rounded-[8px] border border-line px-3 py-2.5">
            <span className="min-w-0">
              <span className="block text-[12px] text-ink">{t("close.autoQuit")}</span>
              <span className="mt-0.5 block text-[10.5px] leading-4 text-faint">{t("close.autoQuitHint")}</span>
            </span>
            <Switch
              checked={armed}
              onChange={(next) => (next ? onArm() : onDisarm())}
              label={t("close.autoQuit")}
            />
          </label>
        )}

        <div className="mt-5 flex justify-end gap-2">
          <Button variant="ghost" onClick={onCancel} disabled={busy}>
            {t("close.cancel")}
          </Button>
          {ready ? (
            <Button variant="primary" onClick={onQuitNow} disabled={busy}>
              {t("close.quitNow")}
            </Button>
          ) : (
            <Button variant="primary" onClick={onArm} disabled={busy || armed}>
              {armed ? t("close.armed") : t("close.waitQuit")}
            </Button>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}