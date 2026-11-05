import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import Player from "../watch/Player";
import { Button, Chip } from "../../components/ui/Controls";
import {
  isTauri,
  mediaDelete,
  mediaList,
  mediaPlay,
  mediaReveal,
  mediaStop,
  onPostProgress,
  sessionsMerge,
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

// 平台只负责一个小圆点：色块在深浅两套主题下都能读，不用两套色class。
const PLATFORM_DOT: Record<string, string> = {
  bilibili: "#22a7f0",
  douyu: "#ff7a1a",
  huya: "#f5a623",
  douyin: "#ff2d6f",
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
  // 合并任务进度（0..99）：合并任务用 "merge"，与录像 id 不冲突。
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

  const allSelected = view.length > 0 && view.every((item) => selected.includes(item.id));

  return (
    <div className="flex min-h-0 flex-col gap-3.5">
      <div className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-[15px] font-semibold text-ink">{t("media.title")}</h1>
          <p className="mt-0.5 text-[11.5px] text-muted">{t("media.subtitle")}</p>
        </div>
        <div className="flex items-center gap-1.5">
          <span className="font-mono text-[11px] tabular-nums text-faint">
            {t("media.count", { n: items.length })}
          </span>
          <Button size="sm" variant="secondary" onClick={() => void reload()}>
            {t("common.refresh")}
          </Button>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <label className="relative">
          <svg
            width="12"
            height="12"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.4"
            aria-hidden
            className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint"
          >
            <circle cx="7" cy="7" r="4.2" />
            <path d="m10.2 10.2 3 3" strokeLinecap="round" />
          </svg>
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("media.search")}
            className="focus-ring h-8 w-56 rounded-ctl border border-line bg-surface pl-7 pr-2.5 text-[12px] text-ink placeholder:text-faint"
          />
        </label>

        <div className="flex items-center gap-0.5 rounded-ctl border border-line bg-surface p-[3px]">
          {[{ key: "all", label: t("media.filterAll") }, ...platforms.map((one) => ({
            key: one,
            label: t("platformName." + one, { defaultValue: one }),
          }))].map((option) => (
            <button
              key={option.key}
              type="button"
              onClick={() => setPlatform(option.key)}
              className={[
                "focus-ring h-6 rounded-[6px] px-2 text-[11px] transition-colors",
                platform === option.key ? "bg-raised text-ink shadow-[0_1px_2px_rgba(16,24,40,0.08)]" : "text-muted hover:text-ink",
              ].join(" ")}
            >
              {option.label}
            </button>
          ))}
        </div>

        <select
          value={sort}
          onChange={(event) => setSort(event.target.value as "recent" | "duration" | "size")}
          className="focus-ring h-8 cursor-pointer appearance-none rounded-ctl border border-line bg-surface px-2.5 pr-6 text-[11px] text-ink-soft"
        >
          <option value="recent">{t("media.sortRecent")}</option>
          <option value="duration">{t("media.sortDuration")}</option>
          <option value="size">{t("media.sortSize")}</option>
        </select>

        <span className="ml-auto font-mono text-[11px] tabular-nums text-faint">
          {t("media.summary", { n: view.length, size: formatSize(totalSize) })}
        </span>
      </div>

      {selected.length > 0 && (
        <div className="flex animate-fade-rise items-center justify-between gap-3 rounded-ctl border border-accent-line bg-accent-soft px-3 py-1.5 text-[11.5px] text-accent-text">
          <span>{t("media.selected", { n: selected.length })}</span>
          <div className="flex items-center gap-1.5">
            <Button size="sm" variant="ghost" onClick={() => setSelected([])}>
              {t("media.clearSelection")}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              disabled={busy !== null || selected.length < 2}
              onClick={() => void mergeSelected()}
              title={selected.length < 2 ? t("media.mergeHint") : undefined}
            >
              {busy === "merge" ? (postPercent === null ? "…" : postPercent + "%") : t("media.merge")}
            </Button>
            <Button size="sm" variant="danger" disabled={busy === "batch"} onClick={() => void removeSelected()}>
              {busy === "batch" ? "…" : t("media.deleteSelected")}
            </Button>
          </div>
        </div>
      )}

      {error && (
        <div className="animate-fade-rise rounded-ctl border border-rec/30 bg-rec/8 px-3 py-1.5 text-[11.5px] text-rec">
          {error}
        </div>
      )}
      {notice && (
        <div className="animate-fade-rise rounded-ctl border border-live/30 bg-live/8 px-3 py-1.5 text-[11.5px] text-live">
          {notice}
        </div>
      )}

      {loading ? (
        <div className="rounded-panel border border-line bg-surface px-4 py-12 text-center text-[12px] text-muted">
          {t("common.loading")}
        </div>
      ) : items.length === 0 ? (
        <div className="flex flex-col items-center justify-center gap-2 rounded-panel border border-dashed border-line-strong bg-surface/50 px-6 py-20 text-center">
          <div className="text-[13px] font-medium text-ink">{t("media.emptyTitle")}</div>
          <div className="max-w-[400px] text-[11.5px] leading-5 text-muted">{t("media.emptyBody")}</div>
        </div>
      ) : view.length === 0 ? (
        <div className="flex flex-col items-center justify-center gap-2 rounded-panel border border-dashed border-line-strong bg-surface/50 px-6 py-20 text-center">
          <div className="text-[13px] font-medium text-ink">{t("media.noMatchTitle")}</div>
          <div className="max-w-[360px] text-[11.5px] leading-5 text-muted">{t("media.noMatchBody")}</div>
        </div>
      ) : (
        <>
          <div className="flex items-center gap-2">
            <label className="flex cursor-pointer items-center gap-1.5 text-[11px] text-muted">
              <input
                type="checkbox"
                checked={allSelected}
                onChange={() =>
                  setSelected(allSelected ? [] : view.map((item) => item.id))
                }
                className="h-3.5 w-3.5 accent-accent"
              />
              {t("media.selectAll")}
            </label>
          </div>

          <div className="grid auto-rows-max grid-cols-1 gap-3 pb-2 md:grid-cols-2 xl:grid-cols-3">
            {view.map((item) => {
              const isPlaying = Boolean(playbacks[item.id]);
              const busyItem = busy === item.id;
              const dot = PLATFORM_DOT[item.platform] ?? "#8a8f99";
              return (
                <article
                  key={item.id}
                  className={[
                    "flex flex-col gap-2.5 rounded-panel border bg-surface p-3 shadow-[var(--shadow-card)] transition-colors",
                    isPlaying ? "border-accent/45" : "border-line hover:border-line-strong",
                    item.hasFile ? "" : "opacity-65",
                  ].join(" ")}
                >
                  <div className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={selected.includes(item.id)}
                      onChange={() => toggleSelected(item.id)}
                      aria-label={t("media.selected", { n: 1 })}
                      className="h-3.5 w-3.5 shrink-0 accent-accent"
                    />
                    <Chip tone="neutral">
                      <span className="mr-1 inline-block h-1.5 w-1.5 rounded-full" style={{ background: dot }} />
                      {t("platformName." + item.platform, { defaultValue: item.platform })}
                    </Chip>
                    {isPlaying && <Chip tone="accent" dot>{t("media.playing")}</Chip>}
                    <span className="ml-auto font-mono text-[10px] tabular-nums text-faint">{formatDate(item.startedAt)}</span>
                  </div>

                  {isPlaying && (
                    <div className="overflow-hidden rounded-ctl border border-line bg-black">
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

                  <div>
                    <div className="line-clamp-2 text-[13px] font-medium leading-snug text-ink">
                      {item.title || item.hostName || item.id}
                    </div>
                    <div className="mt-0.5 truncate text-[11px] text-muted">{item.hostName}</div>
                  </div>

                  <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 font-mono text-[10.5px] tabular-nums text-faint">
                    <span>{formatDuration(item.durationMs)}</span>
                    <span className="text-line-strong">·</span>
                    <span>{formatSize(item.size)}</span>
                    {item.danmakuCount > 0 && (
                      <>
                        <span className="text-line-strong">·</span>
                        <span className="font-sans">{t("media.danmakuCount", { n: item.danmakuCount })}</span>
                      </>
                    )}
                    {!item.hasFile && <span className="font-sans text-warn">{t("media.missing")}</span>}
                  </div>

                  <div className="mt-auto flex items-center gap-1.5 pt-0.5">
                    <Button
                      size="sm"
                      variant={isPlaying ? "secondary" : "primary"}
                      disabled={!item.hasFile || busyItem}
                      onClick={() => void play(item)}
                    >
                      {busyItem ? "…" : isPlaying ? t("media.stop") : t("media.play")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => void mediaReveal(item.id).catch((err) => setError(describeError(err)))}
                    >
                      {t("media.reveal")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      className={confirmId === item.id ? "ml-auto text-rec" : "ml-auto"}
                      disabled={busyItem}
                      onClick={() => void remove(item)}
                      onBlur={() => setConfirmId(null)}
                    >
                      {confirmId === item.id ? t("media.confirmDelete") : t("media.delete")}
                    </Button>
                  </div>
                </article>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}
