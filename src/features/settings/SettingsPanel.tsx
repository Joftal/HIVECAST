import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { detectLanguage, setLanguage, type Language } from "../../i18n";
import { isTauri, settingsGet, settingsUpdate, type Settings } from "../../lib/ipc";
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

  const fieldClass =
    "w-full rounded-lg border hairline bg-ink-800/80 px-2.5 py-1.5 text-[12px] text-zinc-100 outline-none transition-colors focus:border-amber-400/50";
  const labelClass = "text-[11px] font-medium tracking-wide text-zinc-400";
  // 分组标题：抽屉只有一列，靠它把长表单切成三段。
  const groupClass = "text-[11px] font-semibold tracking-[0.08em] text-zinc-200";

  return (
    <div className="fixed inset-0 z-50 flex justify-end">
      <button
        type="button"
        aria-label={t("settings.close")}
        onClick={onClose}
        className="absolute inset-0 bg-black/50 backdrop-blur-[2px]"
      />
      <aside className="relative flex h-full w-[min(430px,92vw)] flex-col border-l hairline bg-ink-900/95 shadow-2xl">
        <div className="flex items-start justify-between gap-3 border-b hairline px-5 py-4">
          <div>
            <h2 className="text-[15px] font-semibold text-zinc-100">{t("settings.title")}</h2>
            <p className="mt-1 text-[11px] leading-relaxed text-zinc-500">{t("settings.subtitle")}</p>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="focus-ring rounded-full border hairline px-2.5 py-1 text-[12px] text-zinc-400 hover:text-white"
          >
            {t("settings.close")}
          </button>
        </div>

        <div className="flex-1 space-y-4 overflow-auto px-5 py-4">
          {!settings ? (
            <div className="text-[12px] text-zinc-500">{t("common.loading")}</div>
          ) : (
            <>
              <p className={groupClass}>{t("settings.groupRecord")}</p>

              <label className="block space-y-1.5">
                <span className={labelClass}>{t("settings.recordQuality")}</span>
                <select
                  value={settings.defaultRecordQn}
                  onChange={(event) => patch({ defaultRecordQn: Number(event.target.value) })}
                  className={fieldClass}
                >
                  {QN_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {t("settings." + option.key)}
                    </option>
                  ))}
                </select>
              </label>

              <label className="block space-y-1.5">
                <span className={labelClass}>{t("settings.viewQuality")}</span>
                <select
                  value={settings.defaultViewQn}
                  onChange={(event) => patch({ defaultViewQn: Number(event.target.value) })}
                  className={fieldClass}
                >
                  {QN_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {t("settings." + option.key)}
                    </option>
                  ))}
                </select>
              </label>

              <p className={groupClass + " border-t hairline pt-3.5"}>{t("settings.groupStorage")}</p>

              <div className="space-y-1.5">
                <span className={labelClass}>{t("settings.mediaRoot")}</span>
                <div className="flex items-center gap-1.5">
                  <input
                    value={settings.mediaRoot}
                    onChange={(event) => patch({ mediaRoot: event.target.value })}
                    className={fieldClass + " font-mono text-[11px]"}
                    spellCheck={false}
                  />
                  <button
                    type="button"
                    onClick={() => void chooseFolder()}
                    className="focus-ring shrink-0 rounded-lg border hairline px-2.5 py-1.5 text-[12px] text-zinc-300 hover:border-white/25 hover:text-white"
                  >
                    {t("settings.choose")}
                  </button>
                </div>
              </div>

              <label className="block space-y-1.5">
                <span className={labelClass}>{t("settings.maxConcurrent")}</span>
                <input
                  type="number"
                  min={1}
                  max={8}
                  value={settings.maxConcurrentRecords}
                  onChange={(event) => patch({ maxConcurrentRecords: Number(event.target.value) })}
                  className={fieldClass}
                />
                <span className="block text-[11px] leading-relaxed text-zinc-500">
                  {t("settings.maxConcurrentHint")}
                </span>
              </label>

              <div className="space-y-1.5">
                <span className={labelClass}>{t("settings.retention")}</span>
                <div className="grid grid-cols-2 gap-2">
                  <label className="block space-y-1">
                    <span className="text-[11px] text-zinc-500">{t("settings.retentionDays")}</span>
                    <input
                      type="number"
                      min={0}
                      max={3650}
                      value={settings.retentionDays}
                      onChange={(event) => patch({ retentionDays: Number(event.target.value) })}
                      className={fieldClass}
                    />
                  </label>
                  <label className="block space-y-1">
                    <span className="text-[11px] text-zinc-500">{t("settings.retentionGb")}</span>
                    <input
                      type="number"
                      min={0}
                      max={100000}
                      value={settings.maxTotalGb}
                      onChange={(event) => patch({ maxTotalGb: Number(event.target.value) })}
                      className={fieldClass}
                    />
                  </label>
                </div>
                <span className="block text-[11px] leading-relaxed text-zinc-500">
                  {t("settings.retentionHint")}
                </span>
              </div>

              <p className={groupClass + " border-t hairline pt-3.5"}>{t("settings.groupInterface")}</p>

              <label className="block space-y-1.5">
                <span className={labelClass}>{t("settings.language")}</span>
                <select
                  value={settings.language}
                  onChange={(event) => patch({ language: event.target.value })}
                  className={fieldClass}
                >
                  <option value="system">{t("settings.languageSystem")}</option>
                  <option value="zh-CN">中文</option>
                  <option value="en-US">English</option>
                </select>
              </label>

              <AccountSection />

              <label className="flex items-start gap-2.5 rounded-xl border hairline bg-ink-850/60 px-3 py-2.5">
                <input
                  type="checkbox"
                  checked={settings.notifyEnabled}
                  onChange={(event) => patch({ notifyEnabled: event.target.checked })}
                  className="mt-0.5 h-3.5 w-3.5 accent-amber-500"
                />
                <span className="text-[12px] leading-relaxed text-zinc-300">{t("settings.notify")}</span>
              </label>
            </>
          )}

          {error && (
            <div className="rounded-xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-[12px] text-red-300">
              {error}
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t hairline px-5 py-3">
          {saved && <span className="mr-auto text-[11px] text-emerald-400">{t("settings.saved")}</span>}
          <button
            type="button"
            onClick={onClose}
            className="focus-ring rounded-full border hairline px-3.5 py-1.5 text-[12px] text-zinc-300 hover:border-white/25 hover:text-white"
          >
            {t("settings.close")}
          </button>
          <button
            type="button"
            disabled={!settings || busy}
            onClick={() => void save()}
            className="focus-ring rounded-full border border-amber-400/40 bg-amber-400/15 px-4 py-1.5 text-[12px] font-medium text-amber-200 transition-colors hover:bg-amber-400/25 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy ? t("settings.saving") : t("settings.save")}
          </button>
        </div>
      </aside>
    </div>
  );
}
