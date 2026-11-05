import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import Player from "../watch/Player";
import {
  isTauri,
  mediaDelete,
  mediaList,
  mediaPlay,
  mediaReveal,
  mediaStop,
  type MediaItem,
  type Playback,
} from "../../lib/ipc";

function formatSize(bytes: number): string {
  if (bytes <= 0) return "—";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return value.toFixed(unit === 0 ? 0 : 1) + " " + units[unit];
}

function formatDuration(ms: number): string {
  if (ms <= 0) return "—";
  const total = Math.round(ms / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return hours > 0 ? hours + ":" + pad(minutes) + ":" + pad(seconds) : minutes + ":" + pad(seconds);
}

function formatDate(ts: number): string {
  const date = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    date.getFullYear() +
    "-" +
    pad(date.getMonth() + 1) +
    "-" +
    pad(date.getDate()) +
    " " +
    pad(date.getHours()) +
    ":" +
    pad(date.getMinutes())
  );
}

function describeError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

const PLATFORM_TONE: Record<string, string> = {
  bilibili: "border-sky-400/30 bg-sky-400/10 text-sky-300",
  douyu: "border-orange-400/30 bg-orange-400/10 text-orange-300",
  huya: "border-amber-400/30 bg-amber-400/10 text-amber-300",
  douyin: "border-fuchsia-400/30 bg-fuchsia-400/10 text-fuchsia-300",
};

export default function MediaPage() {
  const { t } = useTranslation();
  const [items, setItems] = useState<MediaItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [playback, setPlayback] = useState<Playback | null>(null);
  const [volume, setVolume] = useState(0.8);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!isTauri()) {
      setLoading(false);
      return;
    }
    try {
      const list = await mediaList(200);
      setItems(list);
      setError(null);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 离开媒体库时把回放停掉：缓存目录要清，不能留一个 ffmpeg 在后台。
  useEffect(() => () => {
    void mediaStop().catch(() => undefined);
  }, []);

  const stop = async () => {
    setPlayback(null);
    await mediaStop().catch(() => undefined);
  };

  const play = async (item: MediaItem) => {
    if (playback?.sessionId === item.id) {
      await stop();
      return;
    }
    setBusy(item.id);
    setError(null);
    setNotice(null);
    try {
      setPlayback(await mediaPlay(item.id));
    } catch (err) {
      setError(t("media.playFailed") + "：" + describeError(err));
    } finally {
      setBusy(null);
    }
  };

  const remove = async (item: MediaItem) => {
    if (confirmId !== item.id) {
      setConfirmId(item.id);
      return;
    }
    setConfirmId(null);
    setBusy(item.id);
    setError(null);
    try {
      if (playback?.sessionId === item.id) await stop();
      await mediaDelete(item.id, item.hasFile);
      setNotice(t("media.deleted"));
      await reload();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <div className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-lg font-semibold text-zinc-100">{t("media.title")}</h1>
          <p className="mt-1 text-[12px] text-zinc-500">{t("media.subtitle")}</p>
        </div>
        <div className="flex items-center gap-2">
          <span className="rounded-full border hairline px-2.5 py-1 text-[11px] text-zinc-400">
            {t("media.count", { n: items.length })}
          </span>
          <button
            type="button"
            onClick={() => void reload()}
            className="focus-ring rounded-full border hairline px-3 py-1 text-[12px] text-zinc-300 transition-colors hover:border-white/25 hover:text-white"
          >
            {t("common.refresh")}
          </button>
        </div>
      </div>

      {playback && (
        <div className="rounded-2xl border hairline bg-ink-850/80 p-3">
          <div className="mb-2 flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="truncate text-[13px] font-medium text-zinc-100">{playback.title || playback.sessionId}</div>
              <div className="text-[11px] text-zinc-500">
                {t("media.playing")} · {formatDuration(playback.durationMs)}
              </div>
            </div>
            <button
              type="button"
              onClick={() => void stop()}
              className="focus-ring rounded-full border hairline px-3 py-1 text-[12px] text-zinc-300 hover:border-white/25 hover:text-white"
            >
              {t("media.stop")}
            </button>
          </div>
          <div className="overflow-hidden rounded-xl border hairline bg-black">
            <div className="aspect-video max-h-[52vh] w-full">
              <Player url={playback.url} volume={volume} onVolumeChange={setVolume} />
            </div>
          </div>
        </div>
      )}

      {error && (
        <div className="rounded-xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-[12px] text-red-300">{error}</div>
      )}
      {notice && (
        <div className="rounded-xl border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-[12px] text-emerald-300">{notice}</div>
      )}

      {loading ? (
        <div className="rounded-2xl border hairline bg-ink-850/60 px-4 py-10 text-center text-[12px] text-zinc-500">
          {t("common.loading")}
        </div>
      ) : items.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 rounded-2xl border border-dashed hairline bg-ink-850/40 px-6 py-16 text-center">
          <div className="text-[14px] font-medium text-zinc-200">{t("media.emptyTitle")}</div>
          <div className="max-w-[420px] text-[12px] leading-relaxed text-zinc-500">{t("media.emptyBody")}</div>
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 auto-rows-max grid-cols-1 gap-3 overflow-auto pb-2 md:grid-cols-2 xl:grid-cols-3">
          {items.map((item) => {
            const tone = PLATFORM_TONE[item.platform] ?? "border-white/15 bg-white/5 text-zinc-300";
            const isPlaying = playback?.sessionId === item.id;
            const busyItem = busy === item.id;
            return (
              <div
                key={item.id}
                className={[
                  "flex flex-col gap-2.5 rounded-2xl border bg-ink-850/70 p-3.5 transition-colors",
                  isPlaying ? "border-amber-400/50" : "hairline hover:border-white/20",
                  item.hasFile ? "" : "opacity-60",
                ].join(" ")}
              >
                <div className="flex items-center justify-between gap-2">
                  <span className={"rounded-full border px-2 py-0.5 text-[10px] font-medium " + tone}>
                    {t("platformName." + item.platform, { defaultValue: item.platform })}
                  </span>
                  <span className="text-[11px] text-zinc-500">{formatDate(item.startedAt)}</span>
                </div>

                <div className="min-h-[38px]">
                  <div className="line-clamp-2 text-[13px] font-medium leading-snug text-zinc-100">
                    {item.title || item.hostName || item.id}
                  </div>
                  <div className="mt-0.5 truncate text-[11px] text-zinc-500">{item.hostName}</div>
                </div>

                <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-zinc-500">
                  <span>{formatDuration(item.durationMs)}</span>
                  <span>{formatSize(item.size)}</span>
                  {item.danmakuCount > 0 && <span>{t("media.danmakuCount", { n: item.danmakuCount })}</span>}
                  {!item.hasFile && <span className="text-amber-400">{t("media.missing")}</span>}
                </div>

                <div className="mt-auto flex items-center gap-1.5 pt-1">
                  <button
                    type="button"
                    disabled={!item.hasFile || busyItem}
                    onClick={() => void play(item)}
                    className="focus-ring rounded-full border border-amber-400/40 bg-amber-400/10 px-3 py-1 text-[12px] font-medium text-amber-200 transition-colors hover:bg-amber-400/20 disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {busyItem ? "…" : isPlaying ? t("media.stop") : t("media.play")}
                  </button>
                  <button
                    type="button"
                    onClick={() => void mediaReveal(item.id).catch((err) => setError(describeError(err)))}
                    className="focus-ring rounded-full border hairline px-3 py-1 text-[12px] text-zinc-300 transition-colors hover:border-white/25 hover:text-white"
                  >
                    {t("media.reveal")}
                  </button>
                  <button
                    type="button"
                    disabled={busyItem}
                    onClick={() => void remove(item)}
                    onBlur={() => setConfirmId(null)}
                    className={[
                      "focus-ring ml-auto rounded-full border px-3 py-1 text-[12px] transition-colors disabled:opacity-40",
                      confirmId === item.id
                        ? "border-red-400/60 bg-red-500/15 text-red-300"
                        : "hairline text-zinc-400 hover:border-red-400/40 hover:text-red-300",
                    ].join(" ")}
                  >
                    {confirmId === item.id ? t("media.confirmDelete") : t("media.delete")}
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}