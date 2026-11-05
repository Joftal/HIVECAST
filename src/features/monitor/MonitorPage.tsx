import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Chip, IconButton, Select, Switch, TextField, cx } from "../../components/ui/Controls";
import {
  appInfo,
  describeError,
  isTauri,
  onRoomsChanged,
  recordStart,
  recordStop,
  roomAdd,
  roomLines,
  roomMergeSessions,
  roomRefresh,
  roomRemove,
  roomSetAutoRecord,
  roomSetLine,
  roomSetQuality,
  roomsList,
  sessionExportDanmaku,
  sessionRemux,
  sessionScreenshot,
  sessionsList,
  type AppInfo,
  type LineInfo,
  type PostResult,
  type RecordSession,
  type RoomView,
} from "../../lib/ipc";

const QUALITIES = [10000, 400, 250, 150, 80];
type PostKind = "mp4" | "shot" | "ass" | "merge";

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
  return (unit === 0 ? String(Math.round(value)) : value.toFixed(1)) + " " + units[unit];
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

function postKey(roomId: string, kind: PostKind, sessionId?: string): string {
  return roomId + ":" + kind + ":" + (sessionId ?? "");
}

function Thumb({ room }: { room: RoomView["room"] }) {
  return (
    <div className="relative h-[34px] w-[60px] shrink-0 overflow-hidden rounded-[6px] bg-raised">
      {room.coverUrl || room.avatarUrl ? (
        <img
          src={room.coverUrl ?? room.avatarUrl ?? undefined}
          alt=""
          className="h-full w-full object-cover"
          referrerPolicy="no-referrer"
        />
      ) : (
        <div className="flex h-full w-full items-center justify-center text-[11px] text-faint">
          {room.hostName.slice(0, 1) || "?"}
        </div>
      )}
      {room.isLive ? (
        <span className="absolute left-1 top-1 flex items-center gap-1 rounded-[3px] bg-black/55 px-1 py-[1px] text-[8.5px] font-medium leading-none text-white">
          <span className="h-[4px] w-[4px] rounded-full bg-live" />
          LIVE
        </span>
      ) : null}
    </div>
  );
}

type RowProps = {
  view: RoomView;
  selected: boolean;
  busy: boolean;
  now: number;
  locale: string;
  onSelect: () => void;
  onToggleRecord: () => void;
};

function RoomRow({ view, selected, busy, now, locale, onSelect, onToggleRecord }: RowProps) {
  const { t } = useTranslation();
  const room = view.room;
  const recording = view.recording;
  const elapsed = recording ? now / 1000 - recording.startedAt : 0;
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onSelect}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") onSelect();
      }}
      className={cx(
        "group relative flex cursor-default items-center gap-3 border-b border-line px-3 py-2.5 transition-colors last:border-b-0",
        selected ? "bg-raised" : "hover:bg-raised/60",
        busy ? "opacity-60" : "",
      )}
    >
      {selected ? <span className="absolute inset-y-0 left-0 w-[2px] bg-accent" /> : null}
      <Thumb room={room} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span className="truncate text-[12.5px] font-medium text-ink" title={room.title}>
            {room.title || room.roomKey}
          </span>
          {recording ? <Chip tone="rec" dot>{formatClock(elapsed)}</Chip> : null}
        </div>
        <div className="mt-[3px] flex items-center gap-1.5 truncate text-[10.5px] text-muted">
          <span className="truncate">{t("platformName." + room.platform)}</span>
          <span className="text-faint">·</span>
          <span className="truncate">{room.hostName || "—"}</span>
          {room.areaName ? (
            <>
              <span className="text-faint">·</span>
              <span className="truncate">{room.areaName}</span>
            </>
          ) : null}
          <span className="text-faint">·</span>
          <span className="shrink-0 tnum">{formatDateTime(room.lastCheckedAt, locale)}</span>
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        {!room.isLive && !recording ? <span className="text-[10.5px] text-faint">{t("monitor.offline")}</span> : null}
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onToggleRecord();
          }}
          disabled={busy || (!room.isLive && !recording)}
          title={recording ? t("monitor.stopRecord") : t("monitor.startRecord")}
          aria-label={recording ? t("monitor.stopRecord") : t("monitor.startRecord")}
          className={cx(
            "focus-ring flex h-6 w-6 items-center justify-center rounded-[7px] transition-colors",
            recording
              ? "text-rec hover:bg-rec/10"
              : "text-faint hover:bg-raised hover:text-ink",
            busy || (!room.isLive && !recording) ? "pointer-events-none opacity-35" : "",
          )}
        >
          {recording ? (
            <span className="h-[10px] w-[10px] rounded-[2px] bg-current" />
          ) : (
            <span className="h-[11px] w-[11px] rounded-full border-[1.6px] border-current" />
          )}
        </button>
      </div>
    </div>
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
  const [now, setNow] = useState(() => Date.now());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [sessions, setSessions] = useState<RecordSession[] | null>(null);
  const [lines, setLines] = useState<LineInfo[] | null>(null);
  const [lineBusy, setLineBusy] = useState(false);
  const [postBusy, setPostBusy] = useState<string | null>(null);
  const [postMsg, setPostMsg] = useState<{ text: string; error: boolean } | null>(null);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setViews(await roomsList());
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void load();
    appInfo()
      .then(setInfo)
      .catch(() => undefined);

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

  const selected = useMemo(() => views.find((view) => view.room.id === selectedId) ?? null, [views, selectedId]);

  const recordingActive = useMemo(() => views.some((view) => view.recording), [views]);
  useEffect(() => {
    if (!recordingActive) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [recordingActive]);

  // 选中房间时才拉录制历史与线路，避免列表轮询顺手打平台接口。
  const loadSessions = useCallback(async (roomId: string) => {
    if (!isTauri()) return;
    try {
      setSessions(await sessionsList(roomId, 8));
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  const loadLines = useCallback(async (roomId: string, qn: number) => {
    if (!isTauri()) return;
    setLineBusy(true);
    try {
      setLines(await roomLines(roomId, qn));
    } catch (err) {
      setLines([]);
      setError(describeError(err));
    } finally {
      setLineBusy(false);
    }
  }, []);

  useEffect(() => {
    if (!selectedId) {
      setSessions(null);
      setLines(null);
      return;
    }
    setSessions(null);
    setLines(null);
    void loadSessions(selectedId);
  }, [selectedId, loadSessions]);

  const run = useCallback(
    async (id: string, action: () => Promise<unknown>) => {
      setBusyId(id);
      setError(null);
      try {
        await action();
        await load();
        if (id === selectedId) void loadSessions(id);
      } catch (err) {
        setError(describeError(err));
      } finally {
        setBusyId(null);
      }
    },
    [load, loadSessions, selectedId],
  );

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

  const doPost = useCallback(
    async (roomId: string, kind: PostKind, sessionId?: string) => {
      const key = postKey(roomId, kind, sessionId);
      const pick = (id: string | undefined, run: (id: string) => Promise<PostResult>) => {
        if (!id) return Promise.reject(new Error("missing session"));
        return run(id);
      };
      const action = () => {
        switch (kind) {
          case "mp4":
            return pick(sessionId, sessionRemux);
          case "shot":
            return pick(sessionId, sessionScreenshot);
          case "ass":
            return pick(sessionId, sessionExportDanmaku);
          case "merge":
            return roomMergeSessions(roomId);
        }
      };
      setPostBusy(key);
      setPostMsg(null);
      try {
        const result = await action();
        const name = result.path.split(/[\\/]/).pop() ?? result.path;
        setPostMsg({ text: formatSize(result.size) + " · " + name, error: false });
        void load();
        void loadSessions(roomId);
      } catch (err) {
        setPostMsg({ text: describeError(err), error: true });
      } finally {
        setPostBusy(null);
      }
    },
    [load, loadSessions],
  );

  const online = views.filter((view) => view.room.isLive).length;
  const recording = views.filter((view) => view.recording).length;
  const mergeable = (sessions ?? []).filter((s) => s.status !== "recording" && s.filePath).length;

  const lineOptions = lines && lines.length > 0
    ? [{ value: "", label: t("monitor.lineAuto") }].concat(
        lines.map((line) => ({ value: line.name, label: line.name + " · " + line.quality })),
      )
    : [{ value: "", label: t("monitor.lineAuto") }];

  return (
    <div className="flex min-h-0 flex-1 gap-3 p-3">
      {/* 列表 */}
      <section className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-panel border border-line bg-surface shadow-[var(--shadow-card)]">
        <div className="shrink-0 border-b border-line px-3 pb-2.5 pt-3">
          <div className="flex items-center gap-2">
            <h1 className="text-[13.5px] font-semibold text-ink">{t("monitor.listTitle")}</h1>
            <span className="tnum rounded-full bg-raised px-1.5 py-[2px] text-[10.5px] text-muted">{views.length}</span>
            <span className="ml-auto flex items-center gap-2.5 text-[10.5px] text-muted">
              <span className="flex items-center gap-1.5">
                <span className="h-[5px] w-[5px] rounded-full bg-live" />
                {t("monitor.onlineCount", { n: online })}
              </span>
              <span className="flex items-center gap-1.5">
                <span className="h-[5px] w-[5px] rounded-full bg-rec" />
                {t("monitor.recordingCount", { n: recording })}
              </span>
            </span>
          </div>
          <form
            className="mt-2.5 flex items-center gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void add();
            }}
          >
            <TextField
              value={input}
              onChange={setInput}
              placeholder={t("monitor.inputPlaceholder")}
              onSubmit={() => void add()}
            />
            <Button type="submit" variant="primary" disabled={adding || !input.trim()} onClick={() => void add()}>
              {adding ? t("monitor.adding") : t("monitor.add")}
            </Button>
          </form>
          {error ? <p className="mt-2 break-all text-[11px] text-rec">{error}</p> : null}
          {postMsg ? (
            <p className={cx("mt-2 break-all text-[11px]", postMsg.error ? "text-rec" : "text-ok")}>{postMsg.text}</p>
          ) : null}
        </div>

        <div className="min-h-0 flex-1 overflow-auto">
          {views.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center">
              <div className="flex h-11 w-11 items-center justify-center rounded-full bg-raised text-faint">
                <svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                  <rect x="2.2" y="3.4" width="13.6" height="9.2" rx="2" />
                  <path d="M6.6 15.6h4.8" strokeLinecap="round" />
                </svg>
              </div>
              <div>
                <p className="text-[12.5px] font-medium text-ink">{t("monitor.emptyTitle")}</p>
                <p className="mt-1 max-w-[300px] text-[11px] leading-5 text-muted">{t("monitor.emptyBody")}</p>
              </div>
            </div>
          ) : (
            views.map((view) => (
              <RoomRow
                key={view.room.id}
                view={view}
                selected={view.room.id === selectedId}
                busy={busyId === view.room.id}
                now={now}
                locale={locale}
                onSelect={() => setSelectedId(view.room.id)}
                onToggleRecord={() =>
                  void run(view.room.id, () =>
                    view.recording ? recordStop(view.room.id) : recordStart(view.room.id),
                  )
                }
              />
            ))
          )}
        </div>
      </section>

      {/* 详情 */}
      <aside className="flex w-[330px] shrink-0 flex-col gap-3 overflow-auto">
        {selected ? (
          <>
            <section className="overflow-hidden rounded-panel border border-line bg-surface shadow-[var(--shadow-card)]">
              <div className="relative aspect-video bg-raised">
                {selected.room.coverUrl || selected.room.avatarUrl ? (
                  <img
                    src={selected.room.coverUrl ?? selected.room.avatarUrl ?? undefined}
                    alt=""
                    className="h-full w-full object-cover"
                    referrerPolicy="no-referrer"
                  />
                ) : null}
                <div className="absolute left-2 top-2 flex gap-1.5">
                  {selected.room.isLive ? <Chip tone="live" dot>{t("monitor.live")}</Chip> : <Chip>{t("monitor.offline")}</Chip>}
                  {selected.recording ? (
                    <Chip tone="rec" dot>{formatClock(now / 1000 - selected.recording.startedAt)}</Chip>
                  ) : null}
                </div>
              </div>
              <div className="px-3 py-2.5">
                <h2 className="text-[13px] font-semibold leading-5 text-ink">{selected.room.title || selected.room.roomKey}</h2>
                <div className="mt-1 flex items-center gap-1.5 text-[11px] text-muted">
                  <span>{t("platformName." + selected.room.platform)}</span>
                  <span className="text-faint">·</span>
                  <span className="truncate">{selected.room.hostName}</span>
                  {selected.room.areaName ? (
                    <>
                      <span className="text-faint">·</span>
                      <span className="truncate">{selected.room.areaName}</span>
                    </>
                  ) : null}
                </div>
                {selected.error ? <p className="mt-2 break-all text-[11px] text-warn">{selected.error}</p> : null}
              </div>
            </section>

            <section className="rounded-panel border border-line bg-surface shadow-[var(--shadow-card)] p-3">
              <div className="flex items-center justify-between gap-3">
                <span className="text-[11.5px] text-muted">{t("monitor.quality")}</span>
                <Select
                  value={String(selected.room.recordQn)}
                  disabled={busyId === selected.room.id}
                  onChange={(next) =>
                    void run(selected.room.id, () =>
                      roomSetQuality(selected.room.id, Number(next), selected.room.viewQn),
                    )
                  }
                  options={QUALITIES.map((qn) => ({ value: String(qn), label: t("monitor.qn." + qn) }))}
                  className="w-[124px]"
                />
              </div>
              <div className="mt-2 flex items-center justify-between gap-3">
                <span className="text-[11.5px] text-muted">{t("monitor.line")}</span>
                <Select
                  value={selected.room.recordLine ?? ""}
                  disabled={lineBusy || busyId === selected.room.id}
                  title={t("monitor.lineHint")}
                  onChange={(next) =>
                    void run(selected.room.id, () => roomSetLine(selected.room.id, next || null))
                  }
                  options={lineOptions}
                  className="w-[164px]"
                />
              </div>
              <div className="mt-1.5 flex items-center justify-between gap-2">
                {lines === null ? (
                  <button
                    type="button"
                    onClick={() => void loadLines(selected.room.id, selected.room.recordQn)}
                    className="focus-ring inline-flex items-center gap-1 rounded-ctl px-1.5 py-0.5 text-[10.5px] text-accent-text transition-colors hover:bg-accent-soft"
                  >
                    <svg width="9" height="9" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                      <path d="M10 6a4 4 0 1 1-1.2-2.8M10 1.6V4H7.6" strokeLinecap="round" strokeLinejoin="round" />
                    </svg>
                    {t("monitor.lineLoad")}
                  </button>
                ) : (
                  <span className="text-[10px] text-faint">{t("monitor.lineHint")}</span>
                )}
              </div>
              <div className="mt-3 flex items-center justify-between gap-3 border-t border-line pt-3">
                <span className="text-[11.5px] text-muted">{t("monitor.autoRecord")}</span>
                <Switch
                  checked={selected.room.autoRecord}
                  label={t("monitor.autoRecord")}
                  onChange={(next) =>
                    void run(selected.room.id, () => roomSetAutoRecord(selected.room.id, next))
                  }
                />
              </div>
              <Button
                variant={selected.recording ? "recording" : "primary"}
                className="mt-3 w-full"
                disabled={busyId === selected.room.id || (!selected.room.isLive && !selected.recording)}
                onClick={() =>
                  void run(selected.room.id, () =>
                    selected.recording ? recordStop(selected.room.id) : recordStart(selected.room.id),
                  )
                }
              >
                {selected.recording ? t("monitor.stopRecord") : t("monitor.startRecord")}
              </Button>
            </section>

            <section className="min-h-0 rounded-panel border border-line bg-surface shadow-[var(--shadow-card)]">
              <div className="flex items-center justify-between px-3 py-2.5">
                <h3 className="text-[11.5px] font-medium text-ink-soft">{t("monitor.sessions")}</h3>
                {mergeable > 1 ? (
                  <button
                    type="button"
                    onClick={() => void doPost(selected.room.id, "merge")}
                    className="focus-ring rounded px-1 text-[10.5px] text-accent-text hover:underline"
                  >
                    {t("monitor.post.merge")}
                  </button>
                ) : null}
              </div>
              <div className="border-t border-line">
                {sessions === null ? (
                  <p className="px-3 py-3 text-[11px] text-faint">{t("common.loading")}</p>
                ) : sessions.length === 0 ? (
                  <p className="px-3 py-3 text-[11px] text-faint">{t("monitor.noHistory")}</p>
                ) : (
                  sessions.map((session) => (
                    <div key={session.id} className="group flex items-center gap-2 border-b border-line px-3 py-2 last:border-b-0">
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-1.5 text-[11px] text-ink-soft">
                          <span className="tnum">{formatDateTime(session.startedAt, locale)}</span>
                          <span className="tnum text-faint">
                            {formatClock(
                              session.status === "recording" ? now / 1000 - session.startedAt : session.durationMs / 1000,
                            )}
                          </span>
                          <span className="tnum text-faint">{formatSize(session.fileSize)}</span>
                        </div>
                        <div className="mt-0.5 text-[10px] text-faint">
                          {session.status === "recording" ? (
                            <span className="text-rec">{t("monitor.status.recording")}</span>
                          ) : session.reason ? (
                            t("monitor.reason." + session.reason)
                          ) : (
                            t("monitor.status." + session.status)
                          )}
                        </div>
                      </div>
                      {session.status !== "recording" && session.filePath ? (
                        <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
                          <IconButton
                            label={t("monitor.post.mp4")}
                            disabled={postBusy === postKey(selected.room.id, "mp4", session.id)}
                            onClick={() => void doPost(selected.room.id, "mp4", session.id)}
                          >
                            <span className="text-[10px]">MP4</span>
                          </IconButton>
                          <IconButton
                            label={t("monitor.post.shot")}
                            disabled={postBusy === postKey(selected.room.id, "shot", session.id)}
                            onClick={() => void doPost(selected.room.id, "shot", session.id)}
                          >
                            <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                              <rect x="1.8" y="4" width="12.4" height="8.4" rx="1.8" />
                              <circle cx="8" cy="8.2" r="2.2" />
                            </svg>
                          </IconButton>
                          {session.danmakuPath && session.danmakuCount > 0 ? (
                            <IconButton label={t("monitor.post.ass")} onClick={() => void doPost(selected.room.id, "ass", session.id)}>
                              <span className="text-[10px]">ASS</span>
                            </IconButton>
                          ) : null}
                        </div>
                      ) : null}
                    </div>
                  ))
                )}
              </div>
            </section>

            <div className="flex items-center gap-1 pb-1">
              <Button size="sm" variant="ghost" onClick={() => void run(selected.room.id, () => roomRefresh(selected.room.id))}>
                {t("monitor.refreshOne")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="ml-auto text-muted hover:text-rec"
                onClick={() => {
                  setSelectedId(null);
                  void run(selected.room.id, () => roomRemove(selected.room.id));
                }}
              >
                {t("monitor.remove")}
              </Button>
            </div>
          </>
        ) : (
          <>
            <section className="rounded-panel border border-line bg-surface shadow-[var(--shadow-card)] p-3">
              <h2 className="text-[12px] font-medium text-ink-soft">{t("monitor.overview")}</h2>
              <div className="mt-2.5 grid grid-cols-3 gap-2">
                {[
                  { label: t("monitor.statRooms"), value: views.length, tone: "text-ink" },
                  { label: t("monitor.statOnline"), value: online, tone: "text-live" },
                  { label: t("monitor.statRecording"), value: recording, tone: "text-rec" },
                ].map((item) => (
                  <div key={item.label} className="rounded-card bg-raised px-2 py-2">
                    <div className={cx("tnum text-[17px] font-semibold leading-6", item.tone)}>{item.value}</div>
                    <div className="mt-0.5 text-[10px] text-muted">{item.label}</div>
                  </div>
                ))}
              </div>
              <p className="mt-3 text-[11px] leading-5 text-muted">{t("monitor.detailEmpty")}</p>
            </section>
            <p className="break-all px-1 text-[10px] leading-4 text-faint">
              {t("monitor.ffmpeg")}: {info?.ffmpegPath ?? "—"}
            </p>
          </>
        )}
      </aside>
    </div>
  );
}