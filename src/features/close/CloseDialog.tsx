import { useTranslation } from "react-i18next";
import { Button, Switch } from "../../components/ui/Controls";
import { Dialog, DialogContent } from "../../components/arc/dialog/dialog";
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

  return (
    <Dialog open onOpenChange={(next) => { if (!next && !busy) onCancel(); }}>
      <DialogContent
        title={ready ? t("close.readyTitle") : t("close.title")}
        description={ready ? t("close.readyBody") : t("close.body", { n: tasks.length })}
      >
        {ready ? null : (
          <ul className="max-h-[220px] divide-y divide-line overflow-auto rounded-[8px] border border-line">
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
      </DialogContent>
    </Dialog>
  );
}