import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconButton, Segmented } from "./components/ui/Controls";
import MediaPage from "./features/media/MediaPage";
import MonitorPage from "./features/monitor/MonitorPage";
import SettingsPanel from "./features/settings/SettingsPanel";
import WatchPage from "./features/watch/WatchPage";
import { setLanguage, type Language } from "./i18n";
import { isTauri, onAppNotice, settingsGet, settingsUpdate } from "./lib/ipc";
import { applyTheme, watchSystemTheme, type ThemeMode } from "./lib/theme";

type TabKey = "monitor" | "watch" | "media";

function BrandMark() {
  return (
    <svg width="22" height="22" viewBox="0 0 32 32" fill="none" aria-hidden>
      <defs>
        <linearGradient id="hive" x1="4" y1="2" x2="28" y2="30" gradientUnits="userSpaceOnUse">
          <stop stopColor="#FFD37A" />
          <stop offset="1" stopColor="#E08B00" />
        </linearGradient>
      </defs>
      <path d="M16 2.6 27.2 9v14L16 29.4 4.8 23V9L16 2.6Z" stroke="url(#hive)" strokeWidth="1.6" />
      <path d="M16 9.4 21.8 12.7v6.6L16 22.6l-5.8-3.3v-6.6L16 9.4Z" fill="url(#hive)" fillOpacity="0.9" />
    </svg>
  );
}

export default function App() {
  const { t, i18n } = useTranslation();
  const [tab, setTab] = useState<TabKey>("monitor");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [theme, setTheme] = useState<ThemeMode>("light");
  const notifyRef = useRef(true);
  const lang = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";

  // 启动时读一次性设置：语言、主题与通知开关。读失败就保持默认，不打扰用户。
  useEffect(() => {
    if (!isTauri()) {
      applyTheme("light");
      return;
    }
    void settingsGet()
      .then((settings) => {
        notifyRef.current = settings.notifyEnabled;
        if (settings.language !== "system") setLanguage(settings.language as Language);
        const mode = (settings.theme as ThemeMode) ?? "light";
        setTheme(mode);
        applyTheme(mode);
      })
      .catch(() => applyTheme("light"));
  }, []);

  // 跟随系统时，系统换主题要跟着换。
  useEffect(() => {
    if (theme !== "system") return;
    return watchSystemTheme(() => applyTheme("system"));
  }, [theme]);

  const persist = (patch: Record<string, unknown>) => {
    if (!isTauri()) return;
    void settingsGet()
      .then((settings) => settingsUpdate({ ...settings, ...patch }))
      .catch(() => undefined);
  };

  // 顶栏语言切换也写回设置，免得重启后被设置里的旧值覆盖。
  const pickLanguage = (code: Language) => {
    setLanguage(code);
    persist({ language: code });
  };

  const toggleTheme = () => {
    // 顶栏只做明暗互换；「跟随系统」在设置里选。
    const resolved: ThemeMode = theme === "dark" ? "light" : "dark";
    setTheme(resolved);
    applyTheme(resolved);
    persist({ theme: resolved });
  };

  // 录制播报走系统通知；文案在前端 i18n，切语言即时生效。
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    let disposed = false;

    void (async () => {
      try {
        const { isPermissionGranted, requestPermission, sendNotification } = await import(
          "@tauri-apps/plugin-notification"
        );
        let granted = await isPermissionGranted();
        if (!granted) granted = (await requestPermission()) === "granted";

        const off = await onAppNotice((notice) => {
          if (!granted || !notifyRef.current) return;
          const key =
            notice.kind === "disk_full"
              ? "diskFull"
              : notice.kind === "view_failed"
                ? "viewFailed"
                : notice.kind === "credential_expired"
                  ? "credentialExpired"
                  : notice.kind;
          sendNotification({
            title: t(`notice.${key}.title`),
            body: t(`notice.${key}.body`, { host: notice.hostName, detail: notice.detail ?? "" }),
          });
        });
        if (disposed) off();
        else unlisten = off;
      } catch (err) {
        console.warn("notification unavailable", err);
      }
    })();

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [t]);

  const dark = theme === "dark";

  return (
    <div className="flex h-full min-h-0 flex-col bg-canvas text-ink">
      <header className="flex h-12 shrink-0 items-center gap-3 border-b border-line px-3.5">
        <div className="flex items-center gap-2 pr-1">
          <BrandMark />
          <span className="text-[12px] font-semibold tracking-[0.14em] text-ink">HIVECAST</span>
        </div>

        <Segmented
          value={tab}
          onChange={(next) => setTab(next)}
          items={[
            { key: "monitor", label: t("tab.monitor") },
            { key: "watch", label: t("tab.watch") },
            { key: "media", label: t("tab.media") },
          ]}
        />

        <div className="ml-auto flex items-center gap-1">
          {!isTauri() && (
            <span className="rounded-full bg-warn/12 px-2 py-1 text-[10.5px] text-warn">preview</span>
          )}
          <button
            type="button"
            onClick={() => pickLanguage(lang === "zh-CN" ? "en-US" : "zh-CN")}
            title={t("settings.language")}
            className="focus-ring h-7 rounded-ctl px-2 text-[11px] font-medium text-muted transition-colors hover:bg-raised hover:text-ink"
          >
            {lang === "zh-CN" ? "中" : "EN"}
          </button>
          <IconButton label={dark ? t("settings.themeLight") : t("settings.themeDark")} onClick={toggleTheme}>
            {dark ? (
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                <circle cx="8" cy="8" r="3" />
                <path d="M8 1.6v1.6M8 12.8v1.6M1.6 8h1.6M12.8 8h1.6M3.5 3.5l1.1 1.1M11.4 11.4l1.1 1.1M12.5 3.5l-1.1 1.1M4.6 11.4l-1.1 1.1" strokeLinecap="round" />
              </svg>
            ) : (
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                <path d="M13.2 9.6A5.6 5.6 0 0 1 6.4 2.8a5.6 5.6 0 1 0 6.8 6.8Z" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            )}
          </IconButton>
          <IconButton label={t("settings.title")} onClick={() => setSettingsOpen(true)}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 1.55V21a2 2 0 1 1-4 0v-.09A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.55-1H3a2 2 0 1 1 0-4h.09A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.6h.09A1.7 1.7 0 0 0 10 3.05V3a2 2 0 1 1 4 0v.09A1.7 1.7 0 0 0 15 4.6a1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 9v.09a1.7 1.7 0 0 0 1.55 1H21a2 2 0 1 1 0 4h-.09a1.7 1.7 0 0 0-1.51 1Z" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </IconButton>
        </div>
      </header>

      <main key={tab} className="animate-fade-rise flex min-h-0 flex-1 flex-col">
        {tab === "monitor" ? (
          <MonitorPage />
        ) : tab === "watch" ? (
          <WatchPage />
        ) : (
          <div className="min-h-0 flex-1 overflow-auto">
            <div className="mx-auto flex min-h-full w-full max-w-[1360px] flex-col px-6 py-5">
              <MediaPage />
            </div>
          </div>
        )}
      </main>

      <SettingsPanel
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        onSaved={(settings) => {
          notifyRef.current = settings.notifyEnabled;
          const mode = (settings.theme as ThemeMode) ?? "light";
          setTheme(mode);
          applyTheme(mode);
        }}
      />
    </div>
  );
}