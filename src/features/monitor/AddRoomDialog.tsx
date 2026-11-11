import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Dialog, DialogContent } from "../../components/arc/dialog/dialog";
import { Button, Switch } from "../../components/ui/Controls";
import { useNotify } from "../../lib/notify";
import { roomAdd, type RoomView } from "../../lib/ipc";

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 添加成功后调用；监控页用来刷新列表，观看页可以直接开播。 */
  onAdded?: (view: RoomView) => void | Promise<void>;
};

/**
 * 添加直播间弹窗：监控页与观看页共用。
 *
 * 表单状态留在弹窗内部，关闭即重置；业务侧只需要处理「加完以后做什么」。
 */
export default function AddRoomDialog({ open, onOpenChange, onAdded }: Props) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [input, setInput] = useState("");
  // 不监控就没有自动录制，所以关掉监控会顺手关掉它。
  const [monitor, setMonitor] = useState(true);
  const [autoRecord, setAutoRecord] = useState(true);
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    if (open) return;
    setInput("");
    setMonitor(true);
    setAutoRecord(true);
  }, [open]);

  async function add() {
    const value = input.trim();
    if (!value || adding) return;
    setAdding(true);
    try {
      const result = await roomAdd(value, monitor, autoRecord);
      setInput("");
      setMonitor(true);
      setAutoRecord(true);
      onOpenChange(false);
      const name = result.room.hostName || result.room.title || result.room.roomKey;
      if (result.created) {
        notify.success(t("monitor.added"), name);
      } else {
        notify.info(t("monitor.alreadyAdded"), name);
      }
      await onAdded?.(result);
    } catch (err) {
      notify.error(err);
    } finally {
      setAdding(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) setInput("");
      }}
    >
      <DialogContent
        title={t("monitor.addRoom")}
        description={t("monitor.inputPlaceholder")}
        size="md"
      >
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void add();
          }}
        >
          <input
            autoFocus
            value={input}
            onChange={(event) => setInput(event.target.value)}
            placeholder="https://live.bilibili.com/6"
            spellCheck={false}
            className="focus-ring h-10 w-full rounded-ctl border border-line bg-surface px-3 text-[13px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
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
                checked={monitor}
                onChange={(next) => {
                  setMonitor(next);
                  setAutoRecord(next);
                }}
                label={t("monitor.addMonitor")}
              />
            </div>
            <div className="flex items-center gap-3 px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="text-[12.5px] text-ink">{t("monitor.addAutoRecord")}</div>
                <p className="mt-0.5 text-[11px] leading-4 text-faint">
                  {monitor ? t("monitor.addAutoRecordHint") : t("monitor.addAutoRecordDisabled")}
                </p>
              </div>
              <Switch
                checked={autoRecord}
                disabled={!monitor}
                onChange={setAutoRecord}
                label={t("monitor.addAutoRecord")}
              />
            </div>
          </div>
          <div className="mt-4 flex justify-end gap-2">
            <Button
              size="lg"
              variant="ghost"
              onClick={() => {
                onOpenChange(false);
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
      </DialogContent>
    </Dialog>
  );
}
