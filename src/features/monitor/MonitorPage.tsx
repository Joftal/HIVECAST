import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  appInfo,
  describeError,
  isTauri,
  onRoomsChanged,
  recordStart,
  recordStop,
  roomAdd,
  roomRefresh,
  roomRemove,
  roomSetAutoRecord,
  roomSetQuality,
  roomsList,
  sessionsList,
  type AppInfo,
  type RecordSession,
  type RoomView,
} from "../../lib/ipc";

const QUALITIES = [10000, 400, 250, 150, 80];

function pad(value: number): string {
  return value < 10 ? "0" + value : String(value);
}

function formatClock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0 ? h + ":" + pad(m) + ":" + pad(s) : pad(m) + ":" + pad(s);
}

function formatSize(bytes: number): string {
  if (!bytes) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return (unit === 0 ? String(value) : value.toFixed(1)) + " " + units[unit];
}

function formatDateTime(seconds: number | null, locale: string): string {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString(locale === "zh-CN" ? "zh-CN" : "en-US", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function IconButton(props: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={props.label}
      aria-label={props.label}
      onClick={props.onClick}
      disabled={props.disabled}
      className={[
        "focus-ring flex h-7 w-7 items-center justify-center rounded-lg border border-transparent text-zinc-500 transition-all",
        props.disabled ? "cursor-not-allowed opacity-40" : "hover:border-zinc-700/70 hover:bg-ink-700/60 hover:text-zinc-200",
        props.active ? "border-zinc-700/70 bg-ink-700/60 text-zinc-100" : "",
      ].join(" ")}
    >
      {props.children}
    </button>
  );
}

function Switch(props: { checked: boolean; onChange: (next: boolean) => void; label: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.checked}
      aria-label={props.label}
      title={props.label}
      onClick={() => props.onChange(!props.checked)}
      className={[
        "focus-ring relative h-[18px] w-[32px] shrink-0 rounded-full border transition-colors",
        props.checked ? "border-amber-500/60 bg-amber-500/25" : "border-zinc-700 bg-ink-700",
      ].join(" ")}
    >
      <span
        className={[
          "absolute top-[2px] h-[12px] w-[12px] rounded-full transition-all duration-200",
          props.checked ? "left-[16px] bg-amber-300" : "left-[2px] bg-zinc-500",
        ].join(" ")}
      />
    </button>
  );
}

type RowProps = {
  view: RoomView;
  busy: boolean;
  now: number;
  locale: string;
  history: RecordSession[] | null;
  onRefresh: () => void;
  onRemove: () => void;
  onToggleRecord: () => void;
  onToggleAuto: (next: boolean) => void;
  onQuality: (qn: number) => void;
  onHistory: () => void;
};

function RoomRow(props: RowProps) {
  const { t } = useTranslation();
  const { view, busy } = props;
  const room = view.room;
  const recording = view.recording;
  const elapsed = recording ? props.now / 1000 - recording.startedAt : 0;

  return (
    <article className="group border-b hairline px-4 py-3 transition-colors last:border-b-0 hover:bg-ink-800/40">
      <div className="flex items-start gap-3">
        <div className="relative mt-0.5 h-9 w-9 shrink-0 overflow-hidden rounded-xl border hairline bg-ink-800">
          {room.avatarUrl ? (
            <img src={room.avatarUrl} alt="" className="h-full w-full object-cover" referrerPolicy="no-referrer" />
          ) : (
            <div className="flex h-full w-full items-center justify-center text-[12px] text-zinc-500">
              {room.hostName.slice(0, 1)}
            </div>
          )}
          {room.isLive && (
            <span className="absolute right-0.5 bottom-0.5 h-1.5 w-1.5 rounded-full bg-red-400 shadow-[0_0_6px_rgba(248,113,113,0.9)]" />
          )}
        </div>

        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="truncate text-[13px] font-medium text-zinc-100" title={room.title}>
              {room.title || room.roomKey}
            </h3>
            <span
              className={[
                "shrink-0 rounded-full border px-1.5 py-[1px] text-[10px]",
                room.isLive
                  ? "border-red-500/40 bg-red-500/10 text-red-300"
                  : "border-zinc-700/70 bg-ink-800 text-zinc-500",
              ].join(" ")}
            >
              {room.isLive ? t("monitor.live") : t("monitor.offline")}
            </span>
            {recording && (
              <span className="flex shrink-0 items-center gap-1 rounded-full border border-red-500/30 bg-red-500/10 px-1.5 py-[1px] text-[10px] text-red-300">
                <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-red-400" />
                {t("monitor.recordingClock", { time: formatClock(elapsed) })}
              </span>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-zinc-500">
            <span className="rounded border hairline px-1 text-[10px] uppercase tracking-wide text-zinc-400">
              {t(`platformName.${room.platform}`, { defaultValue: room.platform })}
            </span>
            <span className="truncate">{room.hostName}</span>
            {room.areaName && <span className="text-zinc-600">· {room.areaName}</span>}
            {room.online != null && <span className="text-zinc-600">· {t("monitor.online", { n: room.online })}</span>}
            <span className="text-zinc-600">· {formatDateTime(room.lastCheckedAt, props.locale)}</span>
          </div>

          {recording && (
            <div className="truncate font-mono text-[10.5px] text-zinc-600" title={recording.filePath}>
              {recording.filePath}
            </div>
          )}
          {view.error && <div className="break-all text-[11px] text-amber-300/90">{view.error}</div>}

          {props.history && (
            <div className="mt-1 flex flex-col gap-1 rounded-lg border hairline bg-ink-900/50 p-2">
              {props.history.length === 0 && (
                <div className="px-1 py-1 text-[11px] text-zinc-600">{t("monitor.noHistory")}</div>
              )}
              {props.history.map((session) => (
                <div key={session.id} className="flex items-center gap-2 text-[11px]">
                  <span className="w-[92px] shrink-0 text-zinc-500">{formatDateTime(session.startedAt, props.locale)}</span>
                  <span className="w-[70px] shrink-0 text-zinc-400">
                    {formatClock(
                      session.status === "recording"
                        ? props.now / 1000 - session.startedAt
                        : session.durationMs / 1000,
                    )}
                  </span>
                  <span className="w-[64px] shrink-0 text-zinc-500">{formatSize(session.fileSize)}</span>
                  <span
                    className={[
                      "shrink-0 rounded px-1 text-[10px]",
                      session.status === "finished"
                        ? "bg-emerald-500/10 text-emerald-300/90"
                        : session.status === "recording"
                          ? "bg-red-500/10 text-red-300"
                          : "bg-amber-500/10 text-amber-300/90",
                    ].join(" ")}
                  >
                    {t("monitor.status." + session.status)}
                  </span>
                  {session.reason && <span className="truncate text-zinc-600">{t("monitor.reason." + session.reason)}</span>}
                  {session.error && <span className="truncate text-amber-300/80">{session.error}</span>}
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          <select
            value={room.recordQn}
            onChange={(event) => props.onQuality(Number(event.target.value))}
            disabled={busy}
            title={t("monitor.quality")}
            className="focus-ring rounded-lg border hairline bg-ink-800 px-1.5 py-1 text-[11px] text-zinc-300 disabled:opacity-40"
          >
            {QUALITIES.map((qn) => (
              <option key={qn} value={qn}>
                {t("monitor.qn." + qn)}
              </option>
            ))}
          </select>

          <Switch
            checked={room.autoRecord}
            onChange={props.onToggleAuto}
            label={t("monitor.autoRecord")}
          />

          <button
            type="button"
            onClick={props.onToggleRecord}
            disabled={busy || (!room.isLive && !recording)}
            className={[
              "focus-ring rounded-lg border px-2 py-1 text-[11px] transition-colors",
              recording
                ? "border-red-500/40 bg-red-500/10 text-red-300 hover:bg-red-500/20"
                : "border-zinc-700 bg-ink-800 text-zinc-300 hover:border-zinc-600 hover:text-zinc-100",
              busy || (!room.isLive && !recording) ? "cursor-not-allowed opacity-40" : "",
            ].join(" ")}
          >
            {recording ? t("monitor.stopRecord") : t("monitor.startRecord")}
          </button>

          <IconButton label={t("monitor.history")} onClick={props.onHistory} active={props.history != null}>
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
              <circle cx="8" cy="8" r="5.6" />
              <path d="M8 4.8V8l2.2 1.4" strokeLinecap="round" />
            </svg>
          </IconButton>
          <IconButton label={t("monitor.refreshOne")} onClick={props.onRefresh} disabled={busy}>
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
              <path d="M13 8a5 5 0 1 1-1.6-3.7" strokeLinecap="round" />
              <path d="M13.2 2.6v2.6h-2.6" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </IconButton>
          <IconButton label={t("monitor.remove")} onClick={props.onRemove} disabled={busy}>
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
              <path d="M3.4 4.6h9.2M6.4 4.6V3.2h3.2v1.4M5 4.6l.5 8h5l.5-8" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </IconButton>
        </div>
      </div>
    </article>
  );
}

export default function MonitorPage() {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage === "en-US" ? "en-US" : "zh-CN";
  const [views, setViews] = useState<RoomView[]>([]);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [input, setInput] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [history, setHistory] = useState<Record<string, RecordSession[] | null>>({});
  const [now, setNow] = useState(() => Date.now());
  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const rows = await roomsList();
      setViews(rows);
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void load();
    appInfo()
      .then(setInfo)
      .catch((err) => setError(describeError(err)));

    let unlisten: (() => void) | undefined;
    let alive = true;
    onRoomsChanged(() => {
      void load();
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      if (unlisten) unlisten();
    };
  }, [load]);

  const recordingActive = useMemo(() => views.some((view) => view.recording), [views]);

  useEffect(() => {
    if (!recordingActive) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [recordingActive]);

  const run = useCallback(async (id: string, action: () => Promise<unknown>) => {
    setBusyId(id);
    setError(null);
    try {
      await action();
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusyId(null);
    }
  }, [load]);

  const add = async () => {
    const value = input.trim();
    if (!value || adding) return;
    setAdding(true);
    setError(null);
    try {
      await roomAdd(value);
      setInput("");
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setAdding(false);
    }
  };

  const toggleHistory = async (roomId: string) => {
    if (history[roomId] != null) {
      setHistory((prev) => ({ ...prev, [roomId]: null }));
      return;
    }
    try {
      const rows = await sessionsList(roomId, 20);
      setHistory((prev) => ({ ...prev, [roomId]: rows }));
    } catch (err) {
      setError(describeError(err));
    }
  };

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
    <div className="flex h-full min-h-0 flex-col gap-4 lg:flex-row">
      <section className="panel flex min-h-0 flex-1 flex-col overflow-hidden">
        <div className="flex flex-col gap-3 border-b hairline px-4 py-3">
          <div className="flex items-center justify-between gap-3">
            <div>
              <h2 className="text-[13px] font-semibold text-zinc-200">{t("monitor.listTitle")}</h2>
              <p className="mt-0.5 text-[11px] text-zinc-500">{t("monitor.listBody")}</p>
            </div>
            <span className="shrink-0 rounded-full border hairline px-2 py-1 font-mono text-[10.5px] text-zinc-500">
              {t("monitor.count", { n: views.length })}
            </span>
          </div>

          <form
            className="flex gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void add();
            }}
          >
            <input
              value={input}
              onChange={(event) => setInput(event.target.value)}
              placeholder={t("monitor.inputPlaceholder")}
              spellCheck={false}
              className="focus-ring min-w-0 flex-1 rounded-lg border hairline bg-ink-900/60 px-3 py-1.5 text-[12px] text-zinc-200 placeholder:text-zinc-600"
            />
            <button
              type="submit"
              disabled={adding || !input.trim()}
              className={[
                "focus-ring shrink-0 rounded-lg border px-3 py-1.5 text-[12px] transition-colors",
                adding || !input.trim()
                  ? "cursor-not-allowed border-zinc-800 bg-ink-800 text-zinc-600"
                  : "border-amber-500/40 bg-amber-500/15 text-amber-200 hover:bg-amber-500/25",
              ].join(" ")}
            >
              {adding ? t("monitor.adding") : t("monitor.add")}
            </button>
          </form>

          {error && <p className="break-all text-[11px] text-red-300">{error}</p>}
          {!isTauri() && <p className="text-[11px] text-amber-300/80">browser preview — IPC unavailable</p>}
        </div>

        <div className="min-h-0 flex-1 overflow-auto">
          {views.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 px-8 py-12 text-center">
              <div className="flex h-10 w-10 items-center justify-center rounded-2xl border hairline bg-ink-800/80">
                <svg width="16" height="16" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.4" className="text-zinc-500" aria-hidden>
                  <rect x="2.2" y="3.2" width="13.6" height="9.4" rx="2" />
                  <path d="M6.4 15.4h5.2" strokeLinecap="round" />
                </svg>
              </div>
              <h3 className="text-[13px] text-zinc-300">{t("monitor.emptyTitle")}</h3>
              <p className="max-w-[380px] text-[11.5px] leading-5 text-zinc-500">{t("monitor.emptyBody")}</p>
            </div>
          ) : (
            views.map((view) => (
              <RoomRow
                key={view.room.id}
                view={view}
                busy={busyId === view.room.id}
                now={now}
                locale={locale}
                history={history[view.room.id] ?? null}
                onRefresh={() => void run(view.room.id, () => roomRefresh(view.room.id))}
                onRemove={() => void run(view.room.id, () => roomRemove(view.room.id))}
                onToggleRecord={() =>
                  void run(view.room.id, () =>
                    view.recording ? recordStop(view.room.id) : recordStart(view.room.id),
                  )
                }
                onToggleAuto={(next) => void run(view.room.id, () => roomSetAutoRecord(view.room.id, next))}
                onQuality={(qn) => void run(view.room.id, () => roomSetQuality(view.room.id, qn, view.room.viewQn))}
                onHistory={() => void toggleHistory(view.room.id)}
              />
            ))
          )}
        </div>
      </section>

      <aside className="flex w-full shrink-0 flex-col gap-3 lg:w-[320px]">
        <section className="panel flex flex-col gap-3 px-5 py-4">
          <div>
            <h2 className="text-[13px] font-semibold text-zinc-200">{t("monitor.envTitle")}</h2>
            <p className="mt-0.5 text-[11px] text-zinc-500">{t("monitor.envBody")}</p>
          </div>
          <dl className="flex flex-col gap-2 text-[11.5px]">
            {rows.map(([key, value]) => (
              <div key={key} className="flex flex-col gap-0.5 border-b hairline pb-2 last:border-0 last:pb-0">
                <dt className="text-zinc-500">{key}</dt>
                <dd className="break-all font-mono text-[11px] text-zinc-300">{value}</dd>
              </div>
            ))}
          </dl>
        </section>

        <section className="panel flex flex-col gap-2 px-5 py-4">
          <h2 className="text-[13px] font-semibold text-zinc-200">{t("monitor.tipsTitle")}</h2>
          <ul className="flex flex-col gap-1.5 text-[11px] leading-5 text-zinc-500">
            <li>{t("monitor.tipInput")}</li>
            <li>{t("monitor.tipAuto")}</li>
            <li>{t("monitor.tipQuality")}</li>
          </ul>
        </section>
      </aside>
    </div>
  );
}
