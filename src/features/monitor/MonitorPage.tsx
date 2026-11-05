import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { appInfo, describeError, isTauri, type AppInfo } from "../../lib/ipc";

const PLANNED = ["开播检测", "自动录制", "断流续录", "弹幕落盘", "本地后处理"];

export default function MonitorPage() {
  const { t } = useTranslation();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let alive = true;
    appInfo()
      .then((value) => {
        if (alive) setInfo(value);
      })
      .catch((err) => {
        if (alive) setError(describeError(err));
      });
    return () => {
      alive = false;
    };
  }, []);

  const rows: Array<[string, string]> = info
    ? [
        [t("monitor.version"), info.version],
        [t("monitor.platform"), info.platform + " / " + info.arch],
        [t("monitor.ffmpeg"), info.ffmpegPath ?? "—"],
        [t("monitor.dataDir"), info.dataDir],
        [t("monitor.cacheDir"), info.cacheDir],
      ]
    : [];

  return (
    <div className="grid h-full min-h-0 grid-cols-1 gap-4 lg:grid-cols-[minmax(0,1.55fr)_minmax(0,1fr)]">
      <section className="panel flex h-full min-h-[320px] flex-col items-center justify-center gap-3 px-8 py-12 text-center">
        <div className="flex h-11 w-11 items-center justify-center rounded-2xl border hairline bg-ink-800/80">
          <svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.4" className="text-zinc-400" aria-hidden>
            <rect x="2.2" y="3.2" width="13.6" height="9.4" rx="2" />
            <path d="M6.4 15.4h5.2" strokeLinecap="round" />
          </svg>
        </div>
        <h1 className="text-[15px] font-semibold text-zinc-100">{t("monitor.emptyTitle")}</h1>
        <p className="max-w-[420px] text-[12px] leading-5 text-zinc-500">{t("monitor.emptyBody")}</p>
        <button
          type="button"
          disabled
          title={t("monitor.pending")}
          className="mt-1 cursor-not-allowed rounded-full border hairline bg-ink-700/70 px-4 py-1.5 text-[12px] text-zinc-400"
        >
          {t("monitor.addRoom")}
        </button>
        <div className="mt-4 flex flex-wrap items-center justify-center gap-1.5">
          {PLANNED.map((item) => (
            <span key={item} className="rounded-full border hairline px-2 py-1 text-[10.5px] text-zinc-600">
              {item}
            </span>
          ))}
        </div>
      </section>

      <section className="panel flex flex-col gap-3 px-5 py-4">
        <div>
          <h2 className="text-[13px] font-semibold text-zinc-200">{t("monitor.envTitle")}</h2>
          <p className="mt-0.5 text-[11px] text-zinc-500">{t("monitor.envBody")}</p>
        </div>
        {!isTauri() && <p className="text-[11px] text-amber-300/80">browser preview — IPC unavailable</p>}
        {error && <p className="break-all font-mono text-[11px] text-red-300">{error}</p>}
        <dl className="flex flex-col gap-2 text-[11.5px]">
          {rows.map(([key, value]) => (
            <div key={key} className="flex flex-col gap-0.5 border-b hairline pb-2 last:border-0 last:pb-0">
              <dt className="text-zinc-500">{key}</dt>
              <dd className="break-all font-mono text-[11px] text-zinc-300">{value}</dd>
            </div>
          ))}
        </dl>
      </section>
    </div>
  );
}
