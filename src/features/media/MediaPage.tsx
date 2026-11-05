import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import Player from "../watch/Player";
import {
  isTauri,
  mediaDelete,
  mediaList,
  mediaPlay,
  mediaReveal,
  mediaStop,
  onPostProgress,
  sessionExportDanmaku,
  sessionRemux,
  sessionsMerge,
  sessionScreenshot,
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

// 媒体库统一音量：所有卡片共用一份，存 localStorage，下次打开还是这个音量。
const VOLUME_KEY = "hivecast.mediaVolume";

function loadVolume(): number {
  try {
    const raw = window.localStorage.getItem(VOLUME_KEY);
    const value = raw === null ? 0 : Number(raw);
    return Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0;
  } catch {
    return 0;
  }
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
  const [playbacks, setPlaybacks] = useState<Record<string, Playback>>({});
  const [volume, setVolume] = useState(loadVolume);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [platform, setPlatform] = useState<string>("all");
  const [sort, setSort] = useState<"recent" | "duration" | "size">("recent");
  const [selected, setSelected] = useState<string[]>([]);
  // 后处理任务（转封装/截图/ASS）：一次只跑一个，避免 ffmpeg 抢 IO。
  const [posting, setPosting] = useState<string | null>(null);
  // 后处理进度（0..99）：合并任务用 "merge"，与录像 id 不冲突。
  const [postPercent, setPostPercent] = useState<number | null>(null);
  // 卸载清理要用最新值，但不能再触发 effect，所以放 ref。
  const playbacksRef = useRef<Record<string, Playback>>({});
  playbacksRef.current = playbacks;

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

  // 进度事件是全局的，只负责更新数字，显示与否看 posting / busy。
  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    void onPostProgress((progress) => setPostPercent(progress.percent))
      .then((off) => {
        dispose = off;
      })
      .catch(() => undefined);
    return () => dispose?.();
  }, []);

  const changeVolume = (next: number) => {
    const value = Math.min(1, Math.max(0, next));
    setVolume(value);
    try {
      window.localStorage.setItem(VOLUME_KEY, String(value));
    } catch {
      // 隐私模式下写不进去，只影响下次启动的默认值。
    }
  };

  // 离开媒体库时把回放全部停掉：缓存目录要清，不能留 ffmpeg 在后台。
  useEffect(
    () => () => {
      for (const id of Object.keys(playbacksRef.current)) {
        void mediaStop(id).catch(() => undefined);
      }
    },
    [],
  );

  // 平台筛选只列出当前真的存在的平台，避免出现永远为空的选项。
  const platforms = useMemo(() => Array.from(new Set(items.map((item) => item.platform))), [items]);

  // 搜索命中标题与主播名；排序只换顺序，不动原数组。
  const view = useMemo(() => {
    const keyword = query.trim().toLowerCase();
    const list = items.filter((item) => {
      if (platform !== "all" && item.platform !== platform) return false;
      if (!keyword) return true;
      return (item.title + " " + item.hostName).toLowerCase().includes(keyword);
    });
    return [...list].sort((a, b) => {
      if (sort === "duration") return b.durationMs - a.durationMs;
      if (sort === "size") return b.size - a.size;
      return b.startedAt - a.startedAt;
    });
  }, [items, platform, query, sort]);

  const totalSize = view.reduce((sum, item) => sum + Math.max(0, item.size), 0);

  const toggleSelected = (id: string) => {
    setSelected((prev) => (prev.includes(id) ? prev.filter((one) => one !== id) : [...prev, id]));
  };

  const removeSelected = async () => {
    if (selected.length === 0) return;
    setBusy("batch");
    setError(null);
    setNotice(null);
    try {
      for (const id of selected) {
        const item = items.find((one) => one.id === id);
        if (!item) continue;
        if (playbacks[id]) await stop(id);
        await mediaDelete(id, item.hasFile);
      }
      setNotice(t("media.deletedBatch", { n: selected.length }));
      setSelected([]);
      await reload();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(null);
    }
  };

  const mergeSelected = async () => {
    if (selected.length < 2 || busy) return;
    setBusy("merge");
    setPostPercent(0);
    setError(null);
    setNotice(null);
    try {
      const result = await sessionsMerge(selected);
      setNotice(t("media.mergeDone", { size: formatSize(result.size) }));
      setSelected([]);
      await reload();
    } catch (err) {
      setError(t("media.postFailed") + "：" + describeError(err));
    } finally {
      setBusy(null);
      setPostPercent(null);
    }
  };

  const stop = async (id: string) => {
    setPlaybacks((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
    await mediaStop(id).catch(() => undefined);
  };

  // 每条录像各自一格播放器，可以同时放多条（后端上限 3 条）。
  const play = async (item: MediaItem) => {
    if (playbacks[item.id]) {
      await stop(item.id);
      return;
    }
    setBusy(item.id);
    setError(null);
    setNotice(null);
    try {
      const playback = await mediaPlay(item.id);
      setPlaybacks((prev) => ({ ...prev, [item.id]: playback }));
    } catch (err) {
      setError(t("media.playFailed") + "：" + describeError(err));
    } finally {
      setBusy(null);
    }
  };

  // 拖到当前窗口外：后端换一个窗口重开，播放器从新窗口的偏移起播。
  const seekTo = async (item: MediaItem, positionMs: number) => {
    try {
      const playback = await mediaPlay(item.id, positionMs);
      setPlaybacks((prev) => ({ ...prev, [item.id]: playback }));
    } catch (err) {
      setError(t("media.playFailed") + "：" + describeError(err));
    }
  };

  const post = async (item: MediaItem, kind: "mp4" | "screenshot" | "ass") => {
    if (posting) return;
    setPosting(item.id + ":" + kind);
    setPostPercent(0);
    setError(null);
    setNotice(null);
    try {
      const result =
        kind === "mp4"
          ? await sessionRemux(item.id)
          : kind === "screenshot"
            ? await sessionScreenshot(item.id, null)
            : await sessionExportDanmaku(item.id);
      const label =
        result.kind === "mp4" ? t("media.remux") : result.kind === "ass" ? t("media.exportAss") : t("media.screenshot");
      setNotice(t("media.postDone", { what: label, size: formatSize(result.size) }));
      await reload();
    } catch (err) {
      setError(t("media.postFailed") + "：" + describeError(err));
    } finally {
      setPosting(null);
      setPostPercent(null);
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
      if (playbacks[item.id]) await stop(item.id);
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

      <div className="flex flex-wrap items-center gap-2">
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t("media.search")}
          className="focus-ring w-52 rounded-full border hairline bg-ink-900/60 px-3 py-1 text-[12px] text-zinc-200 placeholder:text-zinc-600"
        />
        <div className="flex items-center gap-0.5 rounded-full border hairline p-0.5">
          <button
            type="button"
            onClick={() => setPlatform("all")}
            className={[
              "focus-ring rounded-full px-2.5 py-0.5 text-[11px] transition-colors",
              platform === "all" ? "bg-white/10 text-white" : "text-zinc-400 hover:text-zinc-200",
            ].join(" ")}
          >
            {t("media.filterAll")}
          </button>
          {platforms.map((one) => (
            <button
              key={one}
              type="button"
              onClick={() => setPlatform(one)}
              className={[
                "focus-ring rounded-full px-2.5 py-0.5 text-[11px] transition-colors",
                platform === one ? "bg-white/10 text-white" : "text-zinc-400 hover:text-zinc-200",
              ].join(" ")}
            >
              {t("platformName." + one, { defaultValue: one })}
            </button>
          ))}
        </div>
        <select
          value={sort}
          onChange={(event) => setSort(event.target.value as "recent" | "duration" | "size")}
          className="focus-ring rounded-full border hairline bg-ink-900/60 px-2.5 py-1 text-[11px] text-zinc-300"
        >
          <option value="recent">{t("media.sortRecent")}</option>
          <option value="duration">{t("media.sortDuration")}</option>
          <option value="size">{t("media.sortSize")}</option>
        </select>
        <span className="ml-auto text-[11px] text-zinc-500">
          {t("media.summary", { n: view.length, size: formatSize(totalSize) })}
        </span>
      </div>

      {selected.length > 0 && (
        <div className="flex items-center justify-between gap-3 rounded-xl border border-amber-400/30 bg-amber-400/10 px-3 py-2 text-[12px] text-amber-200">
          <span>{t("media.selected", { n: selected.length })}</span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => setSelected([])}
              className="focus-ring rounded-full border border-amber-300/30 px-3 py-0.5 text-[11px] transition-colors hover:border-amber-200/60"
            >
              {t("media.clearSelection")}
            </button>
            <button
              type="button"
              disabled={busy !== null || selected.length < 2}
              onClick={() => void mergeSelected()}
              title={selected.length < 2 ? t("media.mergeHint") : undefined}
              className="focus-ring rounded-full border border-sky-300/40 bg-sky-400/10 px-3 py-0.5 text-[11px] text-sky-200 transition-colors hover:bg-sky-400/20 disabled:opacity-40"
            >
              {busy === "merge" ? (postPercent === null ? "…" : postPercent + "%") : t("media.merge")}
            </button>
            <button
              type="button"
              disabled={busy === "batch"}
              onClick={() => void removeSelected()}
              className="focus-ring rounded-full border border-red-400/50 bg-red-500/15 px-3 py-0.5 text-[11px] text-red-200 transition-colors hover:bg-red-500/25 disabled:opacity-40"
            >
              {busy === "batch" ? "…" : t("media.deleteSelected")}
            </button>
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
      ) : view.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 rounded-2xl border border-dashed hairline bg-ink-850/40 px-6 py-16 text-center">
          <div className="text-[14px] font-medium text-zinc-200">{t("media.noMatchTitle")}</div>
          <div className="max-w-[380px] text-[12px] leading-relaxed text-zinc-500">{t("media.noMatchBody")}</div>
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 auto-rows-max grid-cols-1 gap-3 overflow-auto pb-2 md:grid-cols-2 xl:grid-cols-3">
          {view.map((item) => {
            const tone = PLATFORM_TONE[item.platform] ?? "border-white/15 bg-white/5 text-zinc-300";
            const isPlaying = Boolean(playbacks[item.id]);
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
                  <label className="flex items-center gap-1.5">
                    <input
                      type="checkbox"
                      checked={selected.includes(item.id)}
                      onChange={() => toggleSelected(item.id)}
                      className="h-3.5 w-3.5 accent-amber-400"
                    />
                    <span className={"rounded-full border px-2 py-0.5 text-[10px] font-medium " + tone}>
                      {t("platformName." + item.platform, { defaultValue: item.platform })}
                    </span>
                  </label>
                  <span className="text-[11px] text-zinc-500">{formatDate(item.startedAt)}</span>
                </div>

                {isPlaying && (
                  <div className="overflow-hidden rounded-xl border hairline bg-black">
                    <div className="aspect-video w-full">
                      <Player
                        url={playbacks[item.id].url}
                        volume={volume}
                        onVolumeChange={changeVolume}
                        onFatal={(detail) => setError(t("media.playFailed") + "：" + detail)}
                        durationMs={item.durationMs}
                        startMs={playbacks[item.id].startMs}
                        leadMs={playbacks[item.id].leadMs}
                        startAtSec={playbacks[item.id].offsetMs / 1000}
                        onSeek={(positionMs) => void seekTo(item, positionMs)}
                      />
                    </div>
                  </div>
                )}

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

                <div className="flex items-center gap-1.5">
                  {(["mp4", "screenshot", "ass"] as const).map((kind) => {
                    const key = item.id + ":" + kind;
                    const label =
                      kind === "mp4" ? t("media.remux") : kind === "screenshot" ? t("media.screenshot") : t("media.exportAss");
                    return (
                      <button
                        key={kind}
                        type="button"
                        disabled={!item.hasFile || posting !== null}
                        onClick={() => void post(item, kind)}
                        className="focus-ring rounded-md border hairline px-2 py-0.5 text-[11px] text-zinc-400 transition-colors hover:border-white/25 hover:text-zinc-100 disabled:cursor-not-allowed disabled:opacity-40"
                      >
                        {posting === key ? (postPercent === null ? "…" : postPercent + "%") : label}
                      </button>
                    );
                  })}
                  <span className="ml-auto text-[11px] text-zinc-600">{isPlaying ? t("media.playing") : ""}</span>
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