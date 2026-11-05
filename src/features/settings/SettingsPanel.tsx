import { useCallback, useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { detectLanguage, setLanguage, type Language } from "../../i18n";
import { isTauri, settingsGet, settingsUpdate, type Settings } from "../../lib/ipc";
import { Button, Segmented, Switch } from "../../components/ui/Controls";
import AccountSection from "./AccountSection";

function describeError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

const QN_OPTIONS = [
  { value: 0, key: "qnAuto" },
  { value: 10000, key: "qnOrigin" },
  { value: 400, key: "qnBluray" },
  { value: 250, key: "qnUltra" },
  { value: 150, key: "qnHigh" },
  { value: 80, key: "qnStandard" },
];

type Props = {
  open: boolean;
  onClose: () => void;
  onSaved: (settings: Settings) => void;
};

const FIELD =
  "focus-ring h-8 w-full rounded-ctl border border-line bg-surface px-2.5 text-[12px] text-ink transition-colors placeholder:text-faint hover:border-line-strong";
const LABEL = "block text-[11px] font-medium text-muted";
const GROUP = "text-[10.5px] font-semibold uppercase tracking-[0.1em] text-faint";
const HINT = "mt-1 block text-[10.5px] leading-4 text-faint";

const CHEVRON = (
  <svg
    width="10"
    height="10"
    viewBox="0 0 12 12"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.4"
    aria-hidden
    className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-faint"
  >
    <path d="M3 4.5 6 7.5 9 4.5" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

function SelectField({
  value,
  onChange,
  children,
}: {
  value: number;
  onChange: (next: number) => void;
  children: ReactNode;
}) {
  return (
    <div className="relative">
      <select
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
        className={FIELD + " cursor-pointer appearance-none pr-7"}
      >
        {children}
      </select>
      {CHEVRON}
    </div>
  );
}

export default function SettingsPanel({ open, onClose, onSaved }: Props) {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setSettings(await settingsGet());
      setError(null);
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    setSaved(false);
    setError(null);
    void load();
  }, [open, load]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const patch = (next: Partial<Settings>) => {
    setSettings((current) => (current ? { ...current, ...next } : current));
    setSaved(false);
  };

  const chooseFolder = async () => {
    if (!settings) return;
    try {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const dir = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: settings.mediaRoot,
      });
      if (typeof dir === "string" && dir.length > 0) patch({ mediaRoot: dir });
    } catch (err) {
      setError(describeError(err));
    }
  };

  const save = async () => {
    if (!settings) return;
    setBusy(true);
    setError(null);
    try {
      const result = await settingsUpdate(settings);
      setSettings(result);
      if (result.language === "system") {
        setLanguage(detectLanguage());
      } else {
        setLanguage(result.language as Language);
      }
      setSaved(true);
      onSaved(result);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex justify-end">
      <button
        type="button"
        aria-label={t("settings.close")}
        onClick={onClose}
        className="absolute inset-0 animate-fade-in bg-black/35 backdrop-blur-[2px]"
      />
      <aside className="relative flex h-full w-[min(440px,94vw)] animate-sheet flex-col border-l border-line bg-surface shadow-2xl">
        <div className="flex items-start justify-between gap-3 border-b border-line px-5 py-4">
          <div>
            <h2 className="text-[14px] font-semibold text-ink">{t("settings.title")}</h2>
            <p className="mt-0.5 text-[11px] leading-4 text-muted">{t("settings.subtitle")}</p>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("settings.close")}
            className="focus-ring -mr-1 flex h-7 w-7 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
              <path d="M2.5 2.5 9.5 9.5M9.5 2.5 2.5 9.5" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-auto px-5 py-4">
          {!settings ? (
            <div className="text-[12px] text-muted">{t("common.loading")}</div>
          ) : (
            <div className="space-y-6">
              <section className="space-y-3">
                <h3 className={GROUP}>{t("settings.groupRecord")}</h3>
                <label className="block space-y-1.5">
                  <span className={LABEL}>{t("settings.recordQuality")}</span>
                  <SelectField
                    value={settings.defaultRecordQn}
                    onChange={(next) => patch({ defaultRecordQn: next })}
                  >
                    {QN_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {t("settings." + option.key)}
                      </option>
                    ))}
                  </SelectField>
                </label>
                <label className="block space-y-1.5">
                  <span className={LABEL}>{t("settings.viewQuality")}</span>
                  <SelectField
                    value={settings.defaultViewQn}
                    onChange={(next) => patch({ defaultViewQn: next })}
                  >
                    {QN_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {t("settings." + option.key)}
                      </option>
                    ))}
                  </SelectField>
                </label>
                <label className="block space-y-1.5">
                  <span className={LABEL}>{t("settings.maxConcurrent")}</span>
                  <input
                    type="number"
                    min={1}
                    max={8}
                    value={settings.maxConcurrentRecords}
                    onChange={(event) => patch({ maxConcurrentRecords: Number(event.target.value) })}
                    className={FIELD + " font-mono tabular-nums"}
                  />
                  <span className={HINT}>{t("settings.maxConcurrentHint")}</span>
                </label>
              </section>

              <section className="space-y-3 border-t border-line pt-5">
                <h3 className={GROUP}>{t("settings.groupStorage")}</h3>
                <div className="space-y-1.5">
                  <span className={LABEL}>{t("settings.mediaRoot")}</span>
                  <div className="flex items-center gap-1.5">
                    <input
                      value={settings.mediaRoot}
                      onChange={(event) => patch({ mediaRoot: event.target.value })}
                      className={FIELD + " font-mono text-[11px]"}
                      spellCheck={false}
                    />
                    <Button size="md" variant="secondary" onClick={() => void chooseFolder()}>
                      {t("settings.choose")}
                    </Button>
                  </div>
                </div>

                <div className="space-y-1.5">
                  <span className={LABEL}>{t("settings.retention")}</span>
                  <div className="grid grid-cols-2 gap-2">
                    <label className="space-y-1">
                      <span className="text-[10.5px] text-faint">{t("settings.retentionDays")}</span>
                      <input
                        type="number"
                        min={0}
                        max={3650}
                        value={settings.retentionDays}
                        onChange={(event) => patch({ retentionDays: Number(event.target.value) })}
                        className={FIELD + " font-mono tabular-nums"}
                      />
                    </label>
                    <label className="space-y-1">
                      <span className="text-[10.5px] text-faint">{t("settings.retentionGb")}</span>
                      <input
                        type="number"
                        min={0}
                        max={100000}
                        value={settings.maxTotalGb}
                        onChange={(event) => patch({ maxTotalGb: Number(event.target.value) })}
                        className={FIELD + " font-mono tabular-nums"}
                      />
                    </label>
                  </div>
                  <span className={HINT}>{t("settings.retentionHint")}</span>
                </div>
              </section>

              <section className="space-y-3 border-t border-line pt-5">
                <h3 className={GROUP}>{t("settings.groupPost")}</h3>
                <div className="flex items-start justify-between gap-3">
                  <span className="min-w-0">
                    <span className="block text-[11.5px] font-medium text-ink">{t("settings.autoRemux")}</span>
                    <span className="mt-0.5 block text-[10.5px] leading-4 text-faint">{t("settings.autoRemuxHint")}</span>
                  </span>
                  <Switch
                    checked={settings.autoRemux}
                    onChange={(next) => patch({ autoRemux: next })}
                    label={t("settings.autoRemux")}
                  />
                </div>
              </section>

              <section className="space-y-3 border-t border-line pt-5">
                <h3 className={GROUP}>{t("settings.groupInterface")}</h3>
                <div className="flex items-center justify-between gap-3">
                  <span className={LABEL}>{t("settings.theme")}</span>
                  <Segmented
                    items={[
                      { key: "light", label: t("settings.themeLight") },
                      { key: "dark", label: t("settings.themeDark") },
                      { key: "system", label: t("settings.themeSystem") },
                    ]}
                    value={(settings.theme as "light" | "dark" | "system") ?? "light"}
                    onChange={(next) => patch({ theme: next })}
                  />
                </div>
                <div className="flex items-center justify-between gap-3">
                  <span className={LABEL}>{t("settings.language")}</span>
                  <Segmented
                    items={[
                      { key: "system", label: t("settings.languageSystem") },
                      { key: "zh-CN", label: "中文" },
                      { key: "en-US", label: "English" },
                    ]}
                    value={settings.language}
                    onChange={(next) => patch({ language: next })}
                  />
                </div>
              </section>

              <section className="space-y-3 border-t border-line pt-5">
                <h3 className={GROUP}>{t("account.title")}</h3>
                <AccountSection />
              </section>

              <section className="border-t border-line pt-5">
                <div className="flex items-center justify-between gap-3">
                  <div>
                    <span className="block text-[11.5px] font-medium text-ink">{t("settings.notify")}</span>
                    <span className="mt-0.5 block text-[10.5px] leading-4 text-faint">{t("settings.notifyHint")}</span>
                  </div>
                  <Switch
                    checked={settings.notifyEnabled}
                    onChange={(next) => patch({ notifyEnabled: next })}
                    label={t("settings.notify")}
                  />
                </div>
              </section>
            </div>
          )}

          {error && (
            <div className="mt-4 rounded-ctl border border-rec/30 bg-rec/8 px-3 py-2 text-[11.5px] text-rec">{error}</div>
          )}
        </div>

        <div className="flex items-center gap-2 border-t border-line px-5 py-3">
          {saved && <span className="mr-auto animate-fade-rise text-[11px] text-live">{t("settings.saved")}</span>}
          <Button variant="ghost" onClick={onClose} className="ml-auto">
            {t("settings.close")}
          </Button>
          <Button variant="primary" disabled={!settings || busy} onClick={() => void save()}>
            {busy ? t("settings.saving") : t("settings.save")}
          </Button>
        </div>
      </aside>
    </div>
  );
}
