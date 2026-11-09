import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, cx } from "../../components/ui/Controls";
import { SelectMenu } from "../../components/ui/SelectMenu";
import InfoTip from "../../components/ui/InfoTip";
import { roomSetRules } from "../../lib/ipc";
import type { RoomRecord } from "../../lib/bindings/RoomRecord";
import { useNotify } from "../../lib/notify";

const inputClass =
  "focus-ring h-8 w-full rounded-ctl border border-line bg-surface px-2 text-[12px] text-ink transition-colors placeholder:text-faint hover:border-line-strong";

type NotifyChoice = "inherit" | "on" | "off";

const toChoice = (value: boolean | null): NotifyChoice =>
  value === null ? "inherit" : value ? "on" : "off";
const fromChoice = (value: NotifyChoice): boolean | null =>
  value === "inherit" ? null : value === "on";

function toOptionalNumber(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed)) return null;
  return Math.max(0, Math.round(parsed));
}

export default function RoomRules({ room, busy }: { room: RoomRecord; busy: boolean }) {
  const { t } = useTranslation();
  const notify = useNotify();
  const [note, setNote] = useState(room.note);
  const [tags, setTags] = useState(room.tags);
  const [titleInclude, setTitleInclude] = useState(room.titleInclude ?? "");
  const [titleExclude, setTitleExclude] = useState(room.titleExclude ?? "");
  const [splitDuration, setSplitDuration] = useState(
    room.splitMaxDurationSec === null ? "" : String(room.splitMaxDurationSec),
  );
  const [splitSize, setSplitSize] = useState(
    room.splitMaxSizeMb === null ? "" : String(room.splitMaxSizeMb),
  );
  const [notifyLive, setNotifyLive] = useState<NotifyChoice>(toChoice(room.notifyLive));
  const [notifyError, setNotifyError] = useState<NotifyChoice>(toChoice(room.notifyError));
  const [saving, setSaving] = useState(false);
  const [open, setOpen] = useState(true);

  const save = async () => {
    if (saving || busy) return;
    setSaving(true);
    try {
      await roomSetRules(room.id, {
        note: note.trim(),
        tags: tags.trim(),
        titleInclude: titleInclude.trim() || null,
        titleExclude: titleExclude.trim() || null,
        splitMaxDurationSec: toOptionalNumber(splitDuration),
        splitMaxSizeMb: toOptionalNumber(splitSize),
        notifyLive: fromChoice(notifyLive),
        notifyError: fromChoice(notifyError),
      });
      notify.success(t("monitor.rulesSaved"));
    } catch (err) {
      notify.error(err);
    } finally {
      setSaving(false);
    }
  };

  const notifyOptions = [
    { value: "inherit", label: t("monitor.inherit") },
    { value: "on", label: t("monitor.enabled") },
    { value: "off", label: t("monitor.disabled") },
  ];

  return (
    <details
      className="group mt-5"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="focus-ring flex cursor-pointer list-none items-center gap-1.5 rounded-ctl text-[11.5px] font-medium text-muted [&::-webkit-details-marker]:hidden">
        {t("monitor.rulesTitle")}
        <svg
          width="12"
          height="12"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          className="ml-auto transition-transform group-open:rotate-180"
          aria-hidden
        >
          <path d="m4 6 4 4 4-4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </summary>
      <div className="mt-2 space-y-2.5 rounded-[8px] border border-line p-3">
        <label className="block">
          <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
            {t("monitor.note")}
            <InfoTip content={t("monitor.tip.note")} />
          </span>
          <textarea
            value={note}
            onChange={(event) => setNote(event.target.value)}
            rows={2}
            placeholder={t("monitor.notePlaceholder")}
            className={cx(inputClass, "h-auto min-h-[52px] resize-y py-1.5 leading-4")}
          />
        </label>

        <label className="block">
          <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
            {t("monitor.tags")}
            <InfoTip content={t("monitor.tip.tags")} />
          </span>
          <input
            value={tags}
            onChange={(event) => setTags(event.target.value)}
            placeholder={t("monitor.tagsPlaceholder")}
            className={inputClass}
          />
        </label>

        <div className="grid grid-cols-2 gap-2">
          <label className="block">
            <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
              {t("monitor.titleInclude")}
              <InfoTip content={t("monitor.tip.titleInclude")} />
            </span>
            <input
              value={titleInclude}
              onChange={(event) => setTitleInclude(event.target.value)}
              placeholder={t("monitor.keywordPlaceholder")}
              className={inputClass}
            />
          </label>
          <label className="block">
            <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
              {t("monitor.titleExclude")}
              <InfoTip content={t("monitor.tip.titleExclude")} />
            </span>
            <input
              value={titleExclude}
              onChange={(event) => setTitleExclude(event.target.value)}
              placeholder={t("monitor.keywordPlaceholder")}
              className={inputClass}
            />
          </label>
        </div>

        <div className="grid grid-cols-2 gap-2">
          <label className="block">
            <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
              {t("monitor.splitDuration")}
              <InfoTip content={t("monitor.tip.splitDuration")} />
            </span>
            <input
              type="number"
              min={0}
              value={splitDuration}
              onChange={(event) => setSplitDuration(event.target.value)}
              placeholder={t("monitor.inherit")}
              className={inputClass}
            />
          </label>
          <label className="block">
            <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
              {t("monitor.splitSize")}
              <InfoTip content={t("monitor.tip.splitSize")} />
            </span>
            <input
              type="number"
              min={0}
              value={splitSize}
              onChange={(event) => setSplitSize(event.target.value)}
              placeholder={t("monitor.inherit")}
              className={inputClass}
            />
          </label>
        </div>

        <div className="grid grid-cols-2 gap-2">
          <div>
            <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
              {t("monitor.notifyLive")}
              <InfoTip content={t("monitor.tip.notifyLive")} />
            </span>
            <SelectMenu
              value={notifyLive}
              size="sm"
              onChange={(value) => setNotifyLive(value as NotifyChoice)}
              options={notifyOptions}
              className="w-full justify-between"
            />
          </div>
          <div>
            <span className="mb-1 flex items-center gap-1 text-[11.5px] text-faint">
              {t("monitor.notifyError")}
              <InfoTip content={t("monitor.tip.notifyError")} />
            </span>
            <SelectMenu
              value={notifyError}
              size="sm"
              onChange={(value) => setNotifyError(value as NotifyChoice)}
              options={notifyOptions}
              className="w-full justify-between"
            />
          </div>
        </div>

        <p className="text-[11px] leading-4 text-faint">{t("monitor.rulesHint")}</p>

        <div className="flex justify-end pt-0.5">
          <Button
            size="sm"
            variant="secondary"
            loading={saving}
            disabled={busy}
            onClick={() => void save()}
          >
            {saving ? t("monitor.rulesSaving") : t("monitor.rulesSave")}
          </Button>
        </div>
      </div>
    </details>
  );
}
