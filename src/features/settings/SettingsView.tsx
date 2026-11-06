import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { detectLanguage, setLanguage, type Language } from "../../i18n";
import { appInfo, isTauri, settingsGet, settingsUpdate, type Settings } from "../../lib/ipc";
import appIcon from "../../assets/app-icon.png";
import { Button, Segmented, Switch, cx } from "../../components/ui/Controls";
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

type PageKey = "account" | "record" | "storage" | "notify" | "appearance" | "system" | "about";

type PageMeta = {
  key: PageKey;
  labelKey: string;
  titleKey: string;
  descKey: string;
  icon: ReactNode;
};

const ICON = {
  account: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <circle cx="8" cy="5.4" r="2.6" />
      <path d="M3 13.4c.7-2.3 2.7-3.6 5-3.6s4.3 1.3 5 3.6" strokeLinecap="round" />
    </svg>
  ),
  record: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <rect x="1.8" y="3.6" width="8.4" height="8.8" rx="1.6" />
      <path d="M10.2 7.2 14 5.2v5.6l-3.8-2Z" strokeLinejoin="round" />
    </svg>
  ),
  storage: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <ellipse cx="8" cy="4" rx="5.2" ry="2.2" />
      <path d="M2.8 4v8c0 1.2 2.3 2.2 5.2 2.2s5.2-1 5.2-2.2V4" />
      <path d="M2.8 8c0 1.2 2.3 2.2 5.2 2.2s5.2-1 5.2-2.2" />
    </svg>
  ),
  notify: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <path d="M4 6.8a4 4 0 1 1 8 0c0 2.6.8 3.6 1.2 4.1H2.8c.4-.5 1.2-1.5 1.2-4.1Z" strokeLinejoin="round" />
      <path d="M6.4 12.9a1.7 1.7 0 0 0 3.2 0" strokeLinecap="round" />
    </svg>
  ),
  appearance: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <circle cx="8" cy="8" r="3.1" />
      <path d="M8 1.4v1.4M8 13.2v1.4M1.4 8h1.4M13.2 8h1.4M3.3 3.3l1 1M11.7 11.7l1 1M12.7 3.3l-1 1M4.3 11.7l-1 1" strokeLinecap="round" />
    </svg>
  ),
  system: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <rect x="1.8" y="2.8" width="12.4" height="8" rx="1.6" />
      <path d="M8 10.8v2.4M5.4 13.4h5.2" strokeLinecap="round" />
    </svg>
  ),
  about: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <circle cx="8" cy="8" r="6.2" />
      <path d="M8 7.2v4.2M8 4.9v.1" strokeLinecap="round" />
    </svg>
  ),
};

const PAGES: PageMeta[] = [
  { key: "account", labelKey: "settings.navAccount", titleKey: "settings.pageAccount", descKey: "settings.pageAccountDesc", icon: ICON.account },
  { key: "record", labelKey: "settings.navRecord", titleKey: "settings.pageRecord", descKey: "settings.pageRecordDesc", icon: ICON.record },
  { key: "storage", labelKey: "settings.navStorage", titleKey: "settings.pageStorage", descKey: "settings.pageStorageDesc", icon: ICON.storage },
  { key: "notify", labelKey: "settings.navNotify", titleKey: "settings.pageNotify", descKey: "settings.pageNotifyDesc", icon: ICON.notify },
  { key: "appearance", labelKey: "settings.navAppearance", titleKey: "settings.pageAppearance", descKey: "settings.pageAppearanceDesc", icon: ICON.appearance },
  { key: "system", labelKey: "settings.navSystem", titleKey: "settings.pageSystem", descKey: "settings.pageSystemDesc", icon: ICON.system },
  { key: "about", labelKey: "settings.navAbout", titleKey: "settings.pageAbout", descKey: "settings.pageAboutDesc", icon: ICON.about },
];

type SearchEntry = { page: PageKey; id: string; titleKey: string; descKey?: string; extra: string };

const SEARCH_INDEX: SearchEntry[] = [
  { page: "account", id: "set-account", titleKey: "settings.navAccount", descKey: "settings.pageAccountDesc", extra: "cookie 登录 扫码 qr login account" },
  { page: "record", id: "set-record-quality", titleKey: "settings.recordQuality", descKey: "settings.recordQualityHint", extra: "画质 清晰度 quality qn" },
  { page: "record", id: "set-view-quality", titleKey: "settings.viewQuality", descKey: "settings.viewQualityHint", extra: "观看 画质 quality qn view" },
  { page: "record", id: "set-max-concurrent", titleKey: "settings.maxConcurrent", descKey: "settings.maxConcurrentHint", extra: "并发 同时录制 concurrent" },
  { page: "record", id: "set-auto-remux", titleKey: "settings.autoRemux", descKey: "settings.autoRemuxHint", extra: "mp4 转码 remux 后处理" },
  { page: "storage", id: "set-media-root", titleKey: "settings.mediaRoot", descKey: "settings.mediaRootHint", extra: "目录 路径 folder path" },
  { page: "storage", id: "set-retention", titleKey: "settings.retention", descKey: "settings.retentionHint", extra: "清理 保留 容量 retention cleanup" },
  { page: "notify", id: "set-notify-record", titleKey: "settings.notifyRecord", descKey: "settings.notifyRecordHint", extra: "通知 录制开始 结束 notify" },
  { page: "notify", id: "set-notify-error", titleKey: "settings.notifyError", descKey: "settings.notifyErrorHint", extra: "通知 异常 失败 error" },
  { page: "notify", id: "set-notify-auth", titleKey: "settings.notifyAuth", descKey: "settings.notifyAuthHint", extra: "通知 登录 失效 cookie expired" },
  { page: "appearance", id: "set-theme", titleKey: "settings.theme", descKey: "settings.themeHint", extra: "主题 深色 浅色 theme dark light" },
  { page: "appearance", id: "set-language", titleKey: "settings.language", descKey: "settings.languageHint", extra: "语言 language english 中文" },
  { page: "system", id: "set-close-action", titleKey: "settings.closeAction", descKey: "settings.closeActionHint", extra: "关闭 托盘 退出 close tray quit" },
  { page: "system", id: "set-prevent-sleep", titleKey: "settings.preventSleep", descKey: "settings.preventSleepHint", extra: "休眠 睡眠 唤醒 sleep power wake" },
  { page: "about", id: "set-about", titleKey: "settings.pageAbout", descKey: "settings.aboutBuild", extra: "版本 version 关于 about 构建" },
];

const FIELD =
  "focus-ring h-8 w-full rounded-ctl border border-line bg-surface px-2.5 text-[12px] text-ink transition-colors placeholder:text-faint hover:border-line-strong";
const GROUP = "text-[10.5px] font-semibold uppercase tracking-[0.1em] text-faint";
const HINT = "mt-0.5 block text-[11px] leading-4 text-faint";

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
  className,
}: {
  value: number;
  onChange: (next: number) => void;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cx("relative", className ?? "w-[208px]")}>
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

function Row({
  id,
  title,
  desc,
  children,
  focus,
}: {
  id: string;
  title: string;
  desc?: string;
  children: ReactNode;
  focus: boolean;
}) {
  return (
    <div
      id={id}
      className={cx(
        "flex items-start gap-4 px-4 py-3 transition-colors",
        focus && "bg-accent-soft",
      )}
    >
      <div className="min-w-0 flex-1">
        <div className="text-[12.5px] text-ink">{title}</div>
        {desc ? <p className={HINT}>{desc}</p> : null}
      </div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-2">
      <h3 className={GROUP}>{title}</h3>
      <div className="divide-y divide-line overflow-hidden rounded-[8px] border border-line bg-surface">
        {children}
      </div>
    </section>
  );
}

type Props = {
  open: boolean;
  onClose: () => void;
  onSaved: (settings: Settings) => void;
};

export default function SettingsView({ open, onClose, onSaved }: Props) {
  const { t } = useTranslation();
  const [page, setPage] = useState<PageKey>("account");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [initial, setInitial] = useState("");
  const [version, setVersion] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [focusId, setFocusId] = useState<string | null>(null);
  const [discardOpen, setDiscardOpen] = useState(false);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const dirty = Boolean(settings) && JSON.stringify(settings) !== initial;
  // macOS 用 Overlay 标题栏，红绿灯浮在左上角，需要给标题让位。
  const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

  useEffect(() => {
    if (!isTauri()) return;
    void appInfo()
      .then((info) => setVersion(info.version))
      .catch(() => setVersion(""));
  }, []);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const loaded = await settingsGet();
      setSettings(loaded);
      setInitial(JSON.stringify(loaded));
      setError(null);
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    setError(null);
    setQuery("");
    setFocusId(null);
    setDiscardOpen(false);
    void load();
  }, [open, load]);

  const patch = (next: Partial<Settings>) => {
    setSettings((current) => (current ? { ...current, ...next } : current));
  };

  const save = useCallback(async () => {
    if (!settings) return;
    setBusy(true);
    setError(null);
    try {
      const result = await settingsUpdate(settings);
      setSettings(result);
      setInitial(JSON.stringify(result));
      if (result.language === "system") {
        setLanguage(detectLanguage());
      } else {
        setLanguage(result.language as Language);
      }
      onSaved(result);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }, [settings, onSaved]);

  const attemptClose = useCallback(() => {
    if (dirty) setDiscardOpen(true);
    else onClose();
  }, [dirty, onClose]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        if (discardOpen) setDiscardOpen(false);
        else attemptClose();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void save();
      }
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, discardOpen, attemptClose, save]);

  useEffect(() => {
    if (!focusId) return;
    const node = document.getElementById(focusId);
    node?.scrollIntoView({ block: "center", behavior: "smooth" });
    const timer = window.setTimeout(() => setFocusId(null), 1600);
    return () => window.clearTimeout(timer);
  }, [focusId, page]);

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

  const results = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return [];
    return SEARCH_INDEX.filter((entry) => {
      const hay = [t(entry.titleKey), entry.descKey ? t(entry.descKey) : "", entry.extra]
        .join(" ")
        .toLowerCase();
      return hay.includes(needle);
    });
  }, [query, t]);

  if (!open) return null;

  const meta = PAGES.find((one) => one.key === page) ?? PAGES[0];

  const renderBody = () => {
    if (!settings) return <div className="py-10 text-center text-[12px] text-muted">{t("common.loading")}</div>;

    if (query.trim()) {
      if (results.length === 0) {
        return <div className="py-10 text-center text-[12px] text-faint">{t("settings.searchEmpty")}</div>;
      }
      return (
        <div className="space-y-5">
          {PAGES.map((one) => {
            const hits = results.filter((entry) => entry.page === one.key);
            if (hits.length === 0) return null;
            return (
              <section key={one.key} className="space-y-2">
                <h3 className={GROUP}>{t(one.titleKey)}</h3>
                <div className="divide-y divide-line overflow-hidden rounded-[8px] border border-line bg-surface">
                  {hits.map((entry) => (
                    <button
                      key={entry.id}
                      type="button"
                      onClick={() => {
                        setPage(entry.page);
                        setQuery("");
                        setFocusId(entry.id);
                      }}
                      className="focus-ring flex w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-raised"
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block text-[12.5px] text-ink">{t(entry.titleKey)}</span>
                        {entry.descKey ? (
                          <span className="mt-0.5 block truncate text-[11px] text-faint">{t(entry.descKey)}</span>
                        ) : null}
                      </span>
                      <span className="shrink-0 text-[10.5px] text-faint">{t(one.labelKey)}</span>
                    </button>
                  ))}
                </div>
              </section>
            );
          })}
        </div>
      );
    }

    if (page === "account") {
      return (
        <Section title={t("settings.groupPlatforms")}>
          <AccountSection />
        </Section>
      );
    }

    if (page === "record") {
      return (
        <div className="space-y-6">
          <Section title={t("settings.sectionQuality")}>
            <Row
              id="set-record-quality"
              focus={focusId === "set-record-quality"}
              title={t("settings.recordQuality")}
              desc={t("settings.recordQualityHint")}
            >
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
            </Row>
            <Row
              id="set-view-quality"
              focus={focusId === "set-view-quality"}
              title={t("settings.viewQuality")}
              desc={t("settings.viewQualityHint")}
            >
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
            </Row>
          </Section>

          <Section title={t("settings.sectionConcurrency")}>
            <Row
              id="set-max-concurrent"
              focus={focusId === "set-max-concurrent"}
              title={t("settings.maxConcurrent")}
              desc={t("settings.maxConcurrentHint")}
            >
              <div className="w-[120px]">
              <input
                type="number"
                min={1}
                max={8}
                value={settings.maxConcurrentRecords}
                onChange={(event) => patch({ maxConcurrentRecords: Number(event.target.value) })}
                className={FIELD + " font-mono tabular-nums"}
              />
              </div>
            </Row>
          </Section>

          <Section title={t("settings.sectionPost")}>
            <Row
              id="set-auto-remux"
              focus={focusId === "set-auto-remux"}
              title={t("settings.autoRemux")}
              desc={t("settings.autoRemuxHint")}
            >
              <Switch
                checked={settings.autoRemux}
                onChange={(next) => patch({ autoRemux: next })}
                label={t("settings.autoRemux")}
              />
            </Row>
          </Section>
        </div>
      );
    }

    if (page === "storage") {
      return (
        <div className="space-y-6">
          <Section title={t("settings.sectionDirectory")}>
            <Row
              id="set-media-root"
              focus={focusId === "set-media-root"}
              title={t("settings.mediaRoot")}
              desc={t("settings.mediaRootHint")}
            >
              <div className="flex w-[420px] items-center gap-1.5">
                <input
                  value={settings.mediaRoot}
                  onChange={(event) => patch({ mediaRoot: event.target.value })}
                  className={FIELD + " font-mono text-[11px]"}
                  spellCheck={false}
                />
                <Button size="sm" variant="secondary" onClick={() => void chooseFolder()}>
                  {t("settings.choose")}
                </Button>
              </div>
            </Row>
          </Section>

          <Section title={t("settings.sectionRetention")}>
            <Row
              id="set-retention"
              focus={focusId === "set-retention"}
              title={t("settings.retention")}
              desc={t("settings.retentionHint")}
            >
              <div className="flex items-center gap-3">
                <label className="flex items-center gap-2 whitespace-nowrap text-[11px] text-muted">
                  {t("settings.retentionDays")}
                  <div className="w-[72px]">
                    <input
                      type="number"
                      min={0}
                      max={3650}
                      value={settings.retentionDays}
                      onChange={(event) => patch({ retentionDays: Number(event.target.value) })}
                      className={FIELD + " font-mono tabular-nums"}
                    />
                  </div>
                  <span>{t("settings.unitDays")}</span>
                </label>
                <label className="flex items-center gap-2 whitespace-nowrap text-[11px] text-muted">
                  {t("settings.retentionGb")}
                  <div className="w-[72px]">
                    <input
                      type="number"
                      min={0}
                      max={100000}
                      value={settings.maxTotalGb}
                      onChange={(event) => patch({ maxTotalGb: Number(event.target.value) })}
                      className={FIELD + " font-mono tabular-nums"}
                    />
                  </div>
                  <span>{t("settings.unitGb")}</span>
                </label>
              </div>
            </Row>
          </Section>
        </div>
      );
    }

    if (page === "notify") {
      return (
        <Section title={t("settings.sectionEvents")}>
          <Row
            id="set-notify-record"
            focus={focusId === "set-notify-record"}
            title={t("settings.notifyRecord")}
            desc={t("settings.notifyRecordHint")}
          >
            <Switch
              checked={settings.notifyEnabled}
              onChange={(next) => patch({ notifyEnabled: next })}
              label={t("settings.notifyRecord")}
            />
          </Row>
          <Row
            id="set-notify-error"
            focus={focusId === "set-notify-error"}
            title={t("settings.notifyError")}
            desc={t("settings.notifyErrorHint")}
          >
            <Switch
              checked={settings.notifyError}
              onChange={(next) => patch({ notifyError: next })}
              label={t("settings.notifyError")}
            />
          </Row>
          <Row
            id="set-notify-auth"
            focus={focusId === "set-notify-auth"}
            title={t("settings.notifyAuth")}
            desc={t("settings.notifyAuthHint")}
          >
            <Switch
              checked={settings.notifyAuth}
              onChange={(next) => patch({ notifyAuth: next })}
              label={t("settings.notifyAuth")}
            />
          </Row>
        </Section>
      );
    }

    if (page === "appearance") {
      return (
        <Section title={t("settings.sectionInterface")}>
          <Row
            id="set-theme"
            focus={focusId === "set-theme"}
            title={t("settings.theme")}
            desc={t("settings.themeHint")}
          >
            <Segmented
              items={[
                { key: "light", label: t("settings.themeLight") },
                { key: "dark", label: t("settings.themeDark") },
                { key: "system", label: t("settings.themeSystem") },
              ]}
              value={(settings.theme as "light" | "dark" | "system") ?? "light"}
              onChange={(next) => patch({ theme: next })}
            />
          </Row>
          <Row
            id="set-language"
            focus={focusId === "set-language"}
            title={t("settings.language")}
            desc={t("settings.languageHint")}
          >
            <Segmented
              items={[
                { key: "system", label: t("settings.languageSystem") },
                { key: "zh-CN", label: "中文" },
                { key: "en-US", label: "English" },
              ]}
              value={settings.language}
              onChange={(next) => patch({ language: next })}
            />
          </Row>
          <div className="px-4 py-3">
            <p className="text-[11px] leading-4 text-faint">{t("settings.trayHint")}</p>
          </div>
        </Section>
      );
    }

    if (page === "system") {
      return (
        <div className="space-y-6">
          <Section title={t("settings.sectionClose")}>
            <Row
              id="set-close-action"
              focus={focusId === "set-close-action"}
              title={t("settings.closeAction")}
              desc={t("settings.closeActionHint")}
            >
              <Segmented
                items={[
                  { key: "tray", label: t("settings.closeTray") },
                  { key: "quit", label: t("settings.closeQuit") },
                ]}
                value={settings.closeAction === "quit" ? "quit" : "tray"}
                onChange={(next) => patch({ closeAction: next })}
              />
            </Row>
          </Section>

          <Section title={t("settings.sectionPower")}>
            <Row
              id="set-prevent-sleep"
              focus={focusId === "set-prevent-sleep"}
              title={t("settings.preventSleep")}
              desc={t("settings.preventSleepHint")}
            >
              <Switch
                checked={settings.preventSleep}
                onChange={(next) => patch({ preventSleep: next })}
                label={t("settings.preventSleep")}
              />
            </Row>
          </Section>
        </div>
      );
    }

    return (
      <div className="space-y-6">
        <Section title={t("settings.groupAbout")}>
          <div id="set-about" className={cx("flex items-center gap-3 px-4 py-4", focusId === "set-about" && "bg-accent-soft")}>
            <img
              src={appIcon}
              alt=""
              width={48}
              height={48}
              className="h-12 w-12 shrink-0 select-none rounded-[11px]"
              draggable={false}
            />
            <div className="min-w-0">
              <div className="text-[12.5px] font-semibold text-ink">HIVECAST</div>
              <div className="mt-0.5 font-mono text-[10.5px] text-faint">
                {version ? "v" + version : "—"}
                <span className="ml-2 font-sans">{t("settings.aboutBuild")}</span>
              </div>
            </div>
          </div>
          <div className="px-4 py-3">
            <p className="text-[11px] leading-4 text-faint">{t("settings.aboutHint")}</p>
          </div>
        </Section>
      </div>
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex bg-canvas text-ink">
      <aside className="flex w-[208px] shrink-0 flex-col border-r border-line bg-surface">
        <div
          className="flex h-12 shrink-0 items-center justify-between border-b border-line pr-4"
          style={{ paddingLeft: isMac ? 78 : 16 }}
        >
          <span className="text-[13px] font-semibold text-ink">{t("settings.title")}</span>
          <button
            type="button"
            onClick={attemptClose}
            aria-label={t("settings.close")}
            className="focus-ring -mr-1 flex h-7 w-7 items-center justify-center rounded-ctl text-faint transition-colors hover:bg-raised hover:text-ink"
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
              <path d="M2.5 2.5 9.5 9.5M9.5 2.5 2.5 9.5" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <nav className="min-h-0 flex-1 space-y-0.5 overflow-auto px-2 py-3">
          {PAGES.map((one) => {
            const active = one.key === page && !query.trim();
            return (
              <button
                key={one.key}
                type="button"
                onClick={() => {
                  setPage(one.key);
                  setQuery("");
                }}
                aria-current={active ? "page" : undefined}
                className={cx(
                  "focus-ring flex h-8 w-full items-center gap-2.5 rounded-ctl px-2 text-left text-[12.5px] transition-colors",
                  active
                    ? "bg-accent-soft font-medium text-accent-text"
                    : "text-muted hover:bg-raised hover:text-ink",
                )}
              >
                <span className={cx("shrink-0", active ? "text-accent-text" : "text-faint")}>{one.icon}</span>
                {t(one.labelKey)}
              </button>
            );
          })}
        </nav>

        <div className="shrink-0 border-t border-line px-4 py-3 font-mono text-[10.5px] text-faint">
          HIVECAST {version ? "v" + version : ""}
        </div>
      </aside>

      <div className="flex min-w-0 flex-1 flex-col">
        <div className="shrink-0 px-8 pb-4 pt-6">
          <div className="mx-auto flex w-full max-w-[720px] items-start justify-between gap-4">
            <div className="min-w-0">
              <h1 className="text-[18px] font-semibold text-ink">
                {query.trim() ? t("settings.searchSettings") : t(meta.titleKey)}
              </h1>
              <p className="mt-1 text-[11.5px] leading-4 text-muted">
                {query.trim()
                  ? t("settings.searchResults", { n: results.length })
                  : t(meta.descKey)}
              </p>
            </div>
            <div className="relative w-[196px] shrink-0">
              <svg
                width="12"
                height="12"
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                aria-hidden
                className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
              >
                <circle cx="7" cy="7" r="4.2" />
                <path d="M10.2 10.2 13.6 13.6" strokeLinecap="round" />
              </svg>
              <input
                ref={searchRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape" && query) {
                    event.stopPropagation();
                    setQuery("");
                  }
                }}
                placeholder={t("settings.searchSettings")}
                spellCheck={false}
                className={FIELD + " pl-8"}
              />
            </div>
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-auto px-8">
          <div className="mx-auto w-full max-w-[720px] pb-8">
            {error ? (
              <div className="mb-4 rounded-ctl border border-rec/30 bg-rec/8 px-3 py-2 text-[11.5px] text-rec">{error}</div>
            ) : null}
            {renderBody()}
          </div>
        </div>

        <div className="shrink-0 border-t border-line px-8 py-3">
          <div className="mx-auto flex w-full max-w-[720px] items-center gap-2">
            <span className={cx("text-[11px]", dirty ? "text-warn" : "text-faint")}>
              {dirty ? t("settings.unsaved") : t("settings.savedAll")}
            </span>
            <Button variant="ghost" onClick={attemptClose} className="ml-auto">
              {t("settings.close")}
            </Button>
            <Button variant="primary" disabled={!settings || busy || !dirty} onClick={() => void save()}>
              {busy ? t("settings.saving") : t("settings.save")}
            </Button>
          </div>
        </div>
      </div>

      {discardOpen ? (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/30 px-6 backdrop-blur-[2px] animate-fade-in">
          <div className="w-[380px] rounded-panel border border-line bg-surface p-5 shadow-pop animate-pop">
            <h2 className="text-[14px] font-semibold text-ink">{t("settings.discardTitle")}</h2>
            <p className="mt-1.5 text-[11.5px] leading-5 text-muted">{t("settings.discardBody")}</p>
            <div className="mt-5 flex justify-end gap-2">
              <Button variant="secondary" onClick={() => setDiscardOpen(false)}>
                {t("settings.keepEditing")}
              </Button>
              <Button
                variant="danger"
                onClick={() => {
                  setDiscardOpen(false);
                  setSettings(null);
                  setInitial("");
                  onClose();
                }}
              >
                {t("settings.discard")}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
