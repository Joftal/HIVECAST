import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import TabBar from "./components/TabBar";
import MediaPage from "./features/media/MediaPage";
import MonitorPage from "./features/monitor/MonitorPage";
import SettingsPanel from "./features/settings/SettingsPanel";
import WatchPage from "./features/watch/WatchPage";
import { setLanguage, type Language } from "./i18n";
import { isTauri, onAppNotice, settingsGet, settingsUpdate } from "./lib/ipc";

type TabKey = "monitor" | "watch" | "media";

function BrandMark() {
  return (
    <svg width="26" height="26" viewBox="0 0 32 32" fill="none" aria-hidden>
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
  const notifyRef = useRef(true);
  const lang = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";

  // 启动时读一次性设置：语言与通知开关。读失败就保持默认，不打扰用户。
  useEffect(() => {
    if (!isTauri()) return;
    void settingsGet()
      .then((settings) => {
        notifyRef.current = settings.notifyEnabled;
        if (settings.language !== "system") setLanguage(settings.language as Language);
      })
      .catch(() => undefined);
  }, []);

  // 顶栏语言切换也写回设置，免得重启后被设置里的旧值覆盖。
  const pickLanguage = (code: Language) => {
    setLanguage(code);
    if (!isTauri()) return;
    void settingsGet()
      .then((settings) => settingsUpdate({ ...settings, language: code }))
      .catch(() => undefined);
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

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex items-center gap-4 border-b hairline px-6 py-3">
        <div className="flex items-center gap-2.5">
          <BrandMark />
          <div className="leading-tight">
            <div className="text-[13px] font-semibold tracking-[0.18em] text-zinc-100">HIVECAST</div>
            <div className="text-[11px] text-zinc-500">{t("app.tagline")}</div>
          </div>
        </div>

        <div className="mx-auto">
          <TabBar
            value={tab}
            onChange={(next) => setTab(next as TabKey)}
            items={[
              { key: "monitor", label: t("tab.monitor") },
              { key: "watch", label: t("tab.watch") },
              { key: "media", label: t("tab.media") },
            ]}
          />
        </div>

        <div className="flex items-center gap-2 text-[11px] text-zinc-500">
          <span className="rounded-full border hairline px-2 py-1">M11</span>
          {!isTauri() && (
            <span className="rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-amber-300">
              browser preview
            </span>
          )}
          <div className="flex overflow-hidden rounded-full border hairline text-[11px]">
            {(["zh-CN", "en-US"] as const).map((code) => (
              <button
                key={code}
                type="button"
                onClick={() => pickLanguage(code)}
                className={[
                  "focus-ring px-2 py-1 transition-colors",
                  lang === code ? "bg-ink-700 text-zinc-100" : "text-zinc-500 hover:text-zinc-300",
                ].join(" ")}
              >
                {code === "zh-CN" ? "中" : "EN"}
              </button>
            ))}
          </div>
          <button
            type="button"
            onClick={() => setSettingsOpen(true)}
            aria-label={t("settings.title")}
            className="focus-ring rounded-full border hairline px-2 py-1 text-zinc-400 transition-colors hover:border-white/25 hover:text-white"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" aria-hidden>
              <circle cx="12" cy="12" r="3.2" stroke="currentColor" strokeWidth="1.7" />
              <path
                d="M12 3.4v2.2M12 18.4v2.2M20.6 12h-2.2M5.6 12H3.4M18.1 5.9l-1.6 1.6M7.5 16.5l-1.6 1.6M18.1 18.1l-1.6-1.6M7.5 7.5 5.9 5.9"
                stroke="currentColor"
                strokeWidth="1.7"
                strokeLinecap="round"
              />
            </svg>
          </button>
        </div>
      </header>

      <main className="min-h-0 flex-1 overflow-auto">
        <div key={tab} className="animate-fade-rise mx-auto flex h-full w-full max-w-[1400px] flex-col px-6 py-6">
          {tab === "monitor" ? <MonitorPage /> : tab === "watch" ? <WatchPage /> : <MediaPage />}
        </div>
      </main>

      <SettingsPanel
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        onSaved={(settings) => {
          notifyRef.current = settings.notifyEnabled;
        }}
      />
    </div>
  );
}