import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import Player from "../watch/Player";
import { Button, Select, cx } from "../../components/ui/Controls";
import {
  isTauri,
  mediaDelete,
  mediaHosts,
  mediaList,
  mediaPlay,
  mediaReveal,
  mediaStop,
  onMediaChanged,
  onMediaThumb,
  onPostProgress,
  sessionExportDanmaku,
  sessionRemux,
  sessionsMerge,
  type MediaHost,
  type MediaItem,
  type Playback,
  type PlatformCount,
} from "../../lib/ipc";

const PAGE_SIZE = 60;
const VOLUME_KEY = "hivecast.mediaVolume";

type SortKey = "recent" | "duration" | "size";
type HostSortKey = "name" | "latest" | "size" | "count";
type Range = 0 | 1 | 7 | 30;

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
    pad(date.getMinutes()) +
    ":" +
    pad(date.getSeconds())
  );
}

function describeError(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

function loadVolume(): number {
  try {
    const raw = window.localStorage.getItem(VOLUME_KEY);
    const value = raw === null ? 0 : Number(raw);
    return Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0;
  } catch {
    return 0;
  }
}

// 头像缺图时的占位底色：只用于字母圆底，不做平台色点。
const PLATFORM_TINT: Record<string, string> = {
  bilibili: "#1f6f9e",
  douyu: "#9a4a12",
  huya: "#8a6414",
  douyin: "#8d1f42",
};

// 主播墙：卡片最小 250，窗口变宽自动多列。
const HOST_GRID_STYLE = { gridTemplateColumns: "repeat(auto-fill, minmax(250px, 1fr))" };
// 录像网格：卡片最小 228。
const GRID_STYLE = { gridTemplateColumns: "repeat(auto-fill, minmax(228px, 1fr))" };

function IconSearch() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
      <circle cx="7" cy="7" r="4.4" />
      <path d="m10.4 10.4 3 3" strokeLinecap="round" />
    </svg>
  );
}

function IconPlay() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <path d="M5.2 3.4c0-.5.55-.8.97-.53l6.1 4.1c.4.26.4.84 0 1.1l-6.1 4.1a.62.62 0 0 1-.97-.53V3.4Z" />
    </svg>
  );
}

function IconCheck() {
  return (
    <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden>
      <path d="M2.4 6.2 4.7 8.5 9.6 3.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function IconClose() {
  return (
    <svg width="13" height="13" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <path d="m3.4 3.4 7.2 7.2M10.6 3.4l-7.2 7.2" strokeLinecap="round" />
    </svg>
  );
}

function IconBack() {
  return (
    <svg width="12" height="12" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <path d="M8.4 3 4.4 7l4 4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

// 主播头像：有图用图，没图用平台底色的首字母，不再额外画色点。
function HostAvatar({ host, size, onCover = true }: { host: MediaHost; size: number; onCover?: boolean }) {
  const name = host.hostName || host.roomTitle || host.roomId;
  if (host.avatarUrl) {
    return (
      <img
        src={host.avatarUrl}
        alt=""
        draggable={false}
        className={
          "shrink-0 rounded-full border object-cover " + (onCover ? "border-white/70" : "border-line")
        }
        style={{ width: size, height: size }}
      />
    );
  }
  return (
    <span
      className={
        "grid shrink-0 place-items-center rounded-full font-semibold text-white/90 " +
        (onCover ? "border border-white/25" : "border border-line")
      }
      style={{
        width: size,
        height: size,
        fontSize: Math.round(size * 0.4),
        background: PLATFORM_TINT[host.platform] ?? "#3a3f47",
      }}
    >
      {name.slice(0, 1).toUpperCase()}
    </span>
  );
}

function PlatformChip({ platform, dark }: { platform: string; dark?: boolean }) {
  const { t } = useTranslation();
  return (
    <span
      className={
        "shrink-0 rounded-[4px] px-1.5 py-0.5 text-[10px] leading-none " +
        (dark ? "bg-black/50 text-white/85 backdrop-blur-sm" : "bg-raised text-muted")
      }
    >
      {t("platformName." + platform, { defaultValue: platform })}
    </span>
  );
}

export default function MediaPage() {
  const { t } = useTranslation();

  // —— 主播墙 ——
  const [hosts, setHosts] = useState<MediaHost[]>([]);
  const [hostTotal, setHostTotal] = useState(0);
  const [hostCounts, setHostCounts] = useState<PlatformCount[]>([]);
  const [hostId, setHostId] = useState<string | null>(null);
  const [hostSort, setHostSort] = useState<HostSortKey>("name");
  const [hostLoading, setHostLoading] = useState(true);
  const [hostLoadingMore, setHostLoadingMore] = useState(false);

  // —— 某个主播的录像 ——
  const [items, setItems] = useState<MediaItem[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);

  // —— 共用状态 ——
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [keyword, setKeyword] = useState("");
  const [platform, setPlatform] = useState("all");
  const [platformOpen, setPlatformOpen] = useState(false);
  const [range, setRange] = useState<Range>(0);
  const [onlyDanmaku, setOnlyDanmaku] = useState(false);
  const [sort, setSort] = useState<SortKey>("recent");

  const [selected, setSelected] = useState<string[]>([]);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [postPercent, setPostPercent] = useState<number | null>(null);

  const [playing, setPlaying] = useState<{ item: MediaItem; playback: Playback } | null>(null);
  const [playBusy, setPlayBusy] = useState<string | null>(null);
  const [volume, setVolume] = useState(loadVolume);

  // 缩略图事件可能比列表先到，先记在 ref 里，下次合并进 items。
  const thumbsRef = useRef<Record<string, string>>({});
  const playingRef = useRef(playing);
  playingRef.current = playing;

  // 搜索防抖：每敲一个字都查一次库没必要。
  useEffect(() => {
    const timer = window.setTimeout(() => setKeyword(query.trim()), 220);
    return () => window.clearTimeout(timer);
  }, [query]);

  const loadHosts = useCallback(
    async (offset: number) => {
      if (!isTauri()) {
        setHostLoading(false);
        return;
      }
      if (offset > 0) setHostLoadingMore(true);
      else setHostLoading(true);
      try {
        const page = await mediaHosts({
          query: keyword || null,
          platform: platform === "all" ? null : platform,
          sort: hostSort,
          limit: PAGE_SIZE,
          offset,
        });
        setHosts((prev) => (offset > 0 ? [...prev, ...page.items] : page.items));
        setHostTotal(page.total);
        setHostCounts(page.platformCounts);
        setError(null);
      } catch (err) {
        setError(describeError(err));
      } finally {
        setHostLoading(false);
        setHostLoadingMore(false);
      }
    },
    [keyword, platform, hostSort],
  );

  const loadFiles = useCallback(
    async (offset: number) => {
      if (!hostId) return;
      if (!isTauri()) {
        setLoading(false);
        return;
      }
      if (offset > 0) setLoadingMore(true);
      else setLoading(true);
      try {
        const page = await mediaList({
          query: keyword || null,
          roomId: hostId,
          days: range || null,
          onlyDanmaku,
          sort,
          limit: PAGE_SIZE,
          offset,
        });
        const withThumbs = page.items.map((item) => ({
          ...item,
          thumbnailUrl: item.thumbnailUrl ?? thumbsRef.current[item.id] ?? null,
        }));
        setItems((prev) => (offset > 0 ? [...prev, ...withThumbs] : withThumbs));
        setTotal(page.total);
        setError(null);
      } catch (err) {
        setError(describeError(err));
      } finally {
        setLoading(false);
        setLoadingMore(false);
      }
    },
    [hostId, keyword, range, onlyDanmaku, sort],
  );

  useEffect(() => {
    void loadHosts(0);
  }, [loadHosts]);

  useEffect(() => {
    if (!hostId) return;
    setSelected([]);
    setConfirmDelete(false);
    void loadFiles(0);
  }, [loadFiles, hostId]);

  // 启动后的崩溃恢复合并在后台跑完时会改文件路径/大小，重新拉一遍列表（I2）。
  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    void onMediaChanged(() => {
      void loadHosts(0);
      if (hostId) void loadFiles(0);
    })
      .then((off) => {
        dispose = off;
      })
      .catch(() => undefined);
    return () => dispose?.();
  }, [hostId, loadFiles, loadHosts]);

  // 生成好的九宫格封面按 id 替换，不整页重载。
  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    void onMediaThumb((thumb) => {
      thumbsRef.current[thumb.sessionId] = thumb.url;
      setItems((prev) =>
        prev.map((item) => (item.id === thumb.sessionId ? { ...item, thumbnailUrl: thumb.url } : item)),
      );
    })
      .then((off) => {
        dispose = off;
      })
      .catch(() => undefined);
    return () => dispose?.();
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    void onPostProgress((one) => setPostPercent(one.percent))
      .then((off) => {
        dispose = off;
      })
      .catch(() => undefined);
    return () => dispose?.();
  }, []);

  // 离开媒体库时把回放停掉：缓存目录要清，不能留 ffmpeg 在后台。
  useEffect(
    () => () => {
      const current = playingRef.current;
      if (current) void mediaStop(current.item.id).catch(() => undefined);
    },
    [],
  );

  const changeVolume = (next: number) => {
    const value = Math.min(1, Math.max(0, next));
    setVolume(value);
    try {
      window.localStorage.setItem(VOLUME_KEY, String(value));
    } catch {
      // 隐私模式下写不进去，只影响下次启动的默认值。
    }
  };

  const enterHost = (host: MediaHost) => {
    setQuery("");
    setKeyword("");
    setSelected([]);
    setConfirmDelete(false);
    setError(null);
    setNotice(null);
    setHostId(host.roomId);
  };

  const backToHosts = () => {
    setHostId(null);
    setSelected([]);
    setConfirmDelete(false);
    setError(null);
    setNotice(null);
    void loadHosts(0);
  };

  const active = hostId ? hosts.find((one) => one.roomId === hostId) ?? null : null;


  const platforms = useMemo(() => {
    const order = ["bilibili", "douyu", "huya", "douyin"];
    const known = hostCounts
      .map((one) => one.platform)
      .sort((a, b) => {
        const left = order.indexOf(a);
        const right = order.indexOf(b);
        if (left === -1 && right === -1) return a.localeCompare(b);
        if (left === -1) return 1;
        if (right === -1) return -1;
        return left - right;
      });
    return ["all", ...known];
  }, [hostCounts]);

  const countOf = (one: string) => {
    // 「全部」用各平台计数之和：total 会跟着平台筛选变小，拿它当全部会自相矛盾。
    if (one === "all") return hostCounts.reduce((sum, entry) => sum + entry.count, 0);
    return hostCounts.find((entry) => entry.platform === one)?.count ?? 0;
  };

  const selectedItems = useMemo(
    () => items.filter((item) => selected.includes(item.id)),
    [items, selected],
  );
  const selectedSize = selectedItems.reduce((sum, item) => sum + Math.max(0, item.size), 0);

  const toggleSelected = (id: string) => {
    setConfirmDelete(false);
    setSelected((prev) => (prev.includes(id) ? prev.filter((one) => one !== id) : [...prev, id]));
  };

  const stopPlayback = async (id: string) => {
    await mediaStop(id).catch(() => undefined);
  };

  const closePlay = async () => {
    const current = playing;
    setPlaying(null);
    if (current) await stopPlayback(current.item.id);
  };

  const openPlay = async (item: MediaItem) => {
    if (playing && playing.item.id === item.id) {
      await closePlay();
      return;
    }
    if (playing) {
      const previous = playing.item.id;
      setPlaying(null);
      await stopPlayback(previous);
    }
    setPlayBusy(item.id);
    setError(null);
    setNotice(null);
    try {
      const playback = await mediaPlay(item.id);
      setPlaying({ item, playback });
    } catch (err) {
      setError(t("media.playFailed") + "：" + describeError(err));
    } finally {
      setPlayBusy(null);
    }
  };

  // 拖到当前窗口外：后端换一个窗口重开，播放器从新窗口的偏移起播。
  const seekTo = async (item: MediaItem, positionMs: number) => {
    try {
      const playback = await mediaPlay(item.id, positionMs);
      setPlaying({ item, playback });
    } catch (err) {
      setError(t("media.playFailed") + "：" + describeError(err));
    }
  };

  // Esc 先关回放抽屉，其次退回主播墙。
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (playingRef.current) {
        void mediaStop(playingRef.current.item.id).catch(() => undefined);
        setPlaying(null);
        return;
      }
      if (hostId) backToHosts();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hostId]);

  const refreshAll = async () => {
    if (hostId) await loadFiles(0);
    await loadHosts(0);
  };

  const runBatch = async (kind: "remux" | "ass" | "merge" | "reveal" | "delete") => {
    if (selectedItems.length === 0 || busy) return;

    if (kind === "reveal") {
      for (const item of selectedItems) {
        await mediaReveal(item.id).catch((err) => setError(describeError(err)));
      }
      return;
    }

    if (kind === "delete" && !confirmDelete) {
      setConfirmDelete(true);
      return;
    }

    setBusy(kind);
    setError(null);
    setNotice(null);
    setProgress(null);
    try {
      if (kind === "merge") {
        if (selectedItems.length < 2) {
          setError(t("media.mergeHint"));
          return;
        }
        setPostPercent(0);
        const result = await sessionsMerge(selectedItems.map((item) => item.id));
        setNotice(t("media.mergeDone", { size: formatSize(result.size) }));
        setSelected([]);
        await refreshAll();
        return;
      }

      if (kind === "delete") {
        for (const item of selectedItems) {
          if (playing && playing.item.id === item.id) await closePlay();
          await mediaDelete(item.id, item.hasFile);
        }
        setNotice(t("media.deletedBatch", { n: selectedItems.length }));
        setSelected([]);
        setConfirmDelete(false);
        await refreshAll();
        return;
      }

      let done = 0;
      for (const item of selectedItems) {
        setProgress({ done, total: selectedItems.length });
        if (kind === "remux") await sessionRemux(item.id);
        else await sessionExportDanmaku(item.id);
        done += 1;
      }
      setNotice(
        kind === "remux"
          ? t("media.remuxDone", { n: done })
          : t("media.exportDone", { n: done }),
      );
      setSelected([]);
      await refreshAll();
    } catch (err) {
      setError(t("media.postFailed") + "：" + describeError(err));
    } finally {
      setBusy(null);
      setProgress(null);
      setPostPercent(null);
    }
  };

  // 抽屉里的单条后处理：只作用于当前正在回放的这条。
  const postOne = async (kind: "remux" | "ass", item: MediaItem) => {
    if (busy) return;
    setBusy(kind);
    setError(null);
    setNotice(null);
    setPostPercent(0);
    try {
      if (kind === "remux") {
        const result = await sessionRemux(item.id);
        setNotice(t("media.remuxOne", { size: formatSize(result.size) }));
      } else {
        await sessionExportDanmaku(item.id);
        setNotice(t("media.exportOne"));
      }
      await refreshAll();
    } catch (err) {
      setError(t("media.postFailed") + "：" + describeError(err));
    } finally {
      setBusy(null);
      setPostPercent(null);
    }
  };

  const removeOne = async (item: MediaItem) => {
    setBusy(item.id);
    setError(null);
    try {
      if (playing && playing.item.id === item.id) await closePlay();
      await mediaDelete(item.id, item.hasFile);
      setNotice(t("media.deleted"));
      await refreshAll();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(null);
    }
  };

  const allSelected = items.length > 0 && items.every((item) => selected.includes(item.id));
  const hostFiltersActive = keyword !== "" || platform !== "all";
  const fileFiltersActive = keyword !== "" || range !== 0 || onlyDanmaku;

  const rangeItems: Array<{ key: Range; label: string }> = [
    { key: 0, label: t("media.rangeAll") },
    { key: 1, label: t("media.rangeToday") },
    { key: 7, label: t("media.range7") },
    { key: 30, label: t("media.range30") },
  ];

  const statusLine = busy
    ? progress
      ? t("media.batchRunning", { done: progress.done, total: progress.total })
      : t("common.loading")
    : null;

  const searchBox = (placeholder: string) => (
    <label className="relative">
      <span className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint">
        <IconSearch />
      </span>
      <input
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        className="focus-ring h-9 w-[228px] rounded-ctl border border-line bg-surface pl-8 pr-2.5 text-[13px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
      />
    </label>
  );

  const platformMenu = (
    <div className="relative">
      <button
        type="button"
        onClick={() => setPlatformOpen((open) => !open)}
        className={cx(
          "focus-ring inline-flex h-9 items-center gap-1.5 rounded-ctl border px-2.5 text-[12.5px] transition-colors",
          platform === "all"
            ? "border-line bg-surface text-ink-soft hover:border-line-strong"
            : "border-accent/40 bg-accent-soft text-accent-text",
        )}
      >
        {platform === "all"
          ? t("media.platformFilter")
          : t("platformName." + platform, { defaultValue: platform })}
        <svg width="10" height="10" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
          <path d="M3 4.5 6 7.5 9 4.5" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
      {platformOpen ? (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setPlatformOpen(false)} />
          <div className="absolute left-0 top-9 z-40 w-[168px] animate-pop rounded-[8px] border border-line bg-surface py-1 shadow-pop">
            {platforms.map((one) => (
              <button
                key={one}
                type="button"
                role="menuitemradio"
                aria-checked={platform === one}
                onClick={() => {
                  setPlatform(one);
                  setPlatformOpen(false);
                }}
                className={cx(
                  "flex w-full items-center justify-between px-3 py-1.5 text-left text-[11.5px] transition-colors",
                  platform === one ? "bg-accent-soft text-accent-text" : "text-ink-soft hover:bg-raised",
                )}
              >
                <span>{one === "all" ? t("media.filterAll") : t("platformName." + one, { defaultValue: one })}</span>
                <span className="font-mono text-[10px] tabular-nums text-faint">{countOf(one)}</span>
              </button>
            ))}
          </div>
        </>
      ) : null}
    </div>
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 px-5 pb-5 pt-4">
      {hostId ? (
        <div className="flex flex-col gap-3">
          <button
            type="button"
            onClick={backToHosts}
            className="focus-ring -mb-1 inline-flex w-fit items-center gap-1 rounded-[4px] px-1 py-0.5 text-[11.5px] text-muted transition-colors hover:text-ink"
          >
            <IconBack />
            {t("media.hosts.back")}
          </button>

          {active ? (
            <div className="flex items-center gap-3">
              <span className="overflow-hidden rounded-[10px]">
                <HostAvatar host={active} size={64} onCover={false} />
              </span>
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="truncate text-[18px] font-semibold leading-tight text-ink">
                    {active.hostName || active.roomTitle || active.roomId}
                  </span>
                  <PlatformChip platform={active.platform} />
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-2 text-[11px] text-muted">
                  <span className="font-mono tabular-nums">{t("media.hosts.count", { n: active.count })}</span>
                  <span className="text-line">·</span>
                  <span className="font-mono tabular-nums">{formatSize(active.bytes)}</span>
                  <span className="text-line">·</span>
                  <span className="font-mono tabular-nums">
                    {t("media.hosts.latest", { time: formatDate(active.latestAt) })}
                  </span>
                  <span className="text-line">·</span>
                  <span className="truncate">{active.roomTitle}</span>
                </div>
              </div>
              <div className="ml-auto flex shrink-0 items-center gap-2">
                <Button
                  size="lg"
                  variant="secondary"
                  disabled={items.length === 0}
                  onClick={() => setSelected(allSelected ? [] : items.map((one) => one.id))}
                >
                  {t("media.selectAll")}
                </Button>
                <Button size="lg" variant="secondary" disabled={loading} onClick={() => void refreshAll()}>
                  {t("common.refresh")}
                </Button>
              </div>
            </div>
          ) : null}

          <div className="flex flex-wrap items-center gap-2">
            <div className="flex h-9 items-center gap-0.5 rounded-ctl bg-raised p-[3px]">
              {rangeItems.map((one) => (
                <button
                  key={one.key}
                  type="button"
                  onClick={() => setRange(one.key)}
                  className={cx(
                    "focus-ring rounded-[5px] px-2.5 py-[5px] text-[12px] font-medium transition-colors",
                    range === one.key ? "bg-surface text-ink shadow-[0_1px_2px_rgba(16,24,40,0.1)]" : "text-muted hover:text-ink",
                  )}
                >
                  {one.label}
                </button>
              ))}
            </div>
            <button
              type="button"
              onClick={() => setOnlyDanmaku((on) => !on)}
              className={cx(
                "focus-ring h-9 rounded-ctl border px-3 text-[12px] transition-colors",
                onlyDanmaku
                  ? "border-accent/40 bg-accent-soft text-accent-text"
                  : "border-line bg-surface text-muted hover:border-line-strong hover:text-ink",
              )}
            >
              {t("media.onlyDanmaku")}
            </button>
            <Select
              size="md"
              value={sort}
              onChange={(next) => setSort(next as SortKey)}
              options={[
                { value: "recent", label: t("media.sortRecent") },
                { value: "duration", label: t("media.sortDuration") },
                { value: "size", label: t("media.sortSize") },
              ]}
              title={t("media.sortLabel")}
              className="ml-auto w-[140px]"
            />
          </div>
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          {searchBox(t("media.hosts.search"))}
          {platformMenu}
          <div className="ml-auto flex items-center gap-2">
            <Select
              size="md"
              value={hostSort}
              onChange={(next) => setHostSort(next as HostSortKey)}
              options={[
                { value: "name", label: t("media.hosts.sortName") },
                { value: "latest", label: t("media.hosts.sortLatest") },
                { value: "size", label: t("media.hosts.sortSize") },
                { value: "count", label: t("media.hosts.sortCount") },
              ]}
              title={t("media.sortLabel")}
              className="w-[140px]"
            />
            <Button size="lg" variant="secondary" disabled={hostLoading} onClick={() => void loadHosts(0)}>
              {t("common.refresh")}
            </Button>
          </div>
        </div>
      )}
      {error ? (
        <div className="flex items-center justify-between rounded-ctl border border-rec/30 bg-rec/10 px-3 py-2 text-[11.5px] text-rec">
          <span className="truncate">{error}</span>
          <button type="button" onClick={() => setError(null)} className="focus-ring shrink-0 px-1 text-[11px]">
            {t("media.cancel")}
          </button>
        </div>
      ) : null}
      {notice || statusLine ? (
        <div className="flex items-center justify-between rounded-ctl border border-line bg-raised px-3 py-2 text-[11.5px] text-ink-soft">
          <span className="truncate">{statusLine ?? notice}</span>
          {notice && !statusLine ? (
            <button type="button" onClick={() => setNotice(null)} className="focus-ring shrink-0 px-1 text-[11px] text-faint">
              {t("media.cancel")}
            </button>
          ) : null}
        </div>
      ) : null}

      {hostId ? (
        loading ? (
          <div className="flex flex-1 items-center justify-center text-[12px] text-faint">{t("common.loading")}</div>
        ) : !active ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-1.5 rounded-panel border border-dashed border-line bg-surface/60 py-16 text-center">
            <div className="text-[13px] font-medium text-ink">{t("media.hosts.goneTitle")}</div>
            <div className="max-w-[360px] text-[11.5px] leading-relaxed text-muted">{t("media.hosts.goneBody")}</div>
            <Button size="sm" variant="secondary" className="mt-2" onClick={backToHosts}>
              {t("media.hosts.back")}
            </Button>
          </div>
        ) : items.length === 0 ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-1.5 rounded-panel border border-dashed border-line bg-surface/60 py-16 text-center">
            <div className="text-[13px] font-medium text-ink">
              {fileFiltersActive ? t("media.noMatchTitle") : t("media.emptyTitle")}
            </div>
            <div className="max-w-[360px] text-[11.5px] leading-relaxed text-muted">
              {fileFiltersActive ? t("media.noMatchBody") : t("media.emptyBody")}
            </div>
          </div>
        ) : (
          <div className="grid gap-3" style={GRID_STYLE}>
            {items.map((item) => {
              const isSelected = selected.includes(item.id);
              const isPlaying = playing?.item.id === item.id;
              return (
                <article
                  key={item.id}
                  className={cx(
                    "group flex flex-col overflow-hidden rounded-panel border bg-surface transition-colors duration-150",
                    isSelected ? "border-accent/60 shadow-[0_0_0_1px_var(--accent-soft)]" : "border-line hover:border-line-strong",
                  )}
                >
                  <div className="relative aspect-video w-full overflow-hidden bg-raised">
                    {item.thumbnailUrl ? (
                      <img
                        src={item.thumbnailUrl}
                        alt=""
                        draggable={false}
                        className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.02]"
                      />
                    ) : (
                      <div className="flex h-full w-full items-center justify-center text-[11px] text-faint">
                        {item.hasFile ? t("media.thumbsPending") : t("media.missing")}
                      </div>
                    )}

                    <button
                      type="button"
                      onClick={() => void openPlay(item)}
                      disabled={!item.hasFile}
                      aria-label={t("media.play")}
                      className="absolute inset-0 flex items-center justify-center disabled:cursor-not-allowed"
                    >
                      <span className="flex h-10 w-10 items-center justify-center rounded-full bg-black/55 text-white opacity-0 backdrop-blur-[2px] transition-opacity duration-150 group-hover:opacity-100">
                        {playBusy === item.id ? (
                          <span className="h-3.5 w-3.5 animate-spin rounded-full border-[1.5px] border-white/40 border-t-white" />
                        ) : (
                          <IconPlay />
                        )}
                      </span>
                    </button>

                    <span className="pointer-events-none absolute left-2 top-2 rounded-[4px] bg-black/55 px-1.5 py-[3px] text-[10px] font-medium leading-none text-white/90 backdrop-blur-[2px]">
                      {t("platformName." + item.platform, { defaultValue: item.platform })}
                    </span>

                    <button
                      type="button"
                      onClick={() => toggleSelected(item.id)}
                      aria-label={t("media.selectAll")}
                      aria-pressed={isSelected}
                      className={cx(
                        "focus-ring absolute right-2 top-2 z-10 flex h-[18px] w-[18px] items-center justify-center rounded-[4px] border transition-colors",
                        isSelected
                          ? "border-accent bg-accent text-accent-ink"
                          : "border-white/45 bg-black/35 text-transparent hover:border-white/80 hover:text-white/70",
                      )}
                    >
                      <IconCheck />
                    </button>

                    <span className="pointer-events-none absolute bottom-2 left-2 rounded-[4px] bg-black/60 px-1.5 py-[3px] font-mono text-[10px] tabular-nums leading-none text-white/90">
                      {formatDuration(item.durationMs)}
                    </span>
                    {item.danmakuCount > 0 ? (
                      <span className="pointer-events-none absolute bottom-2 right-2 rounded-[4px] bg-black/60 px-1.5 py-[3px] text-[10px] leading-none text-white/85">
                        {t("media.danmakuCount", { n: item.danmakuCount })}
                      </span>
                    ) : null}
                    {isPlaying ? (
                      <span className="pointer-events-none absolute inset-x-0 top-0 h-[2px] bg-accent" />
                    ) : null}
                  </div>

                  <div className="flex min-h-0 flex-1 flex-col gap-1 px-3 py-2.5">
                    <h3 className="line-clamp-2 text-[12.5px] font-medium leading-[1.45] text-ink">
                      {item.title || item.hostName || item.id}
                    </h3>
                    <div className="mt-auto flex items-center justify-between gap-2 pt-0.5 text-[10.5px] text-faint">
                      <span className="font-mono tabular-nums">{formatDate(item.startedAt)}</span>
                      <span className="shrink-0 font-mono tabular-nums">{formatSize(item.size)}</span>
                    </div>
                    {isPlaying ? <div className="text-[10.5px] text-accent-text">{t("media.playing")}</div> : null}
                  </div>
                </article>
              );
            })}
          </div>
        )
      ) : hostLoading ? (
        <div className="flex flex-1 items-center justify-center text-[12px] text-faint">{t("common.loading")}</div>
      ) : hosts.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-1.5 rounded-panel border border-dashed border-line bg-surface/60 py-16 text-center">
          <div className="text-[13px] font-medium text-ink">
            {hostFiltersActive ? t("media.hosts.noMatchTitle") : t("media.hosts.emptyTitle")}
          </div>
          <div className="max-w-[360px] text-[11.5px] leading-relaxed text-muted">
            {hostFiltersActive ? t("media.hosts.noMatchBody") : t("media.hosts.emptyBody")}
          </div>
        </div>
      ) : (
        <div className="grid gap-3" style={HOST_GRID_STYLE}>
          {hosts.map((host) => (
            <button
              key={host.roomId}
              type="button"
              onClick={() => enterHost(host)}
              className="group flex flex-col overflow-hidden rounded-panel border border-line bg-surface text-left transition-colors hover:border-line-strong"
            >
              <div className="relative w-full overflow-hidden bg-raised" style={{ aspectRatio: "16 / 10" }}>
                {host.coverUrl ? (
                  <img
                    src={host.coverUrl}
                    alt=""
                    draggable={false}
                    className="h-full w-full object-cover transition-transform duration-500 group-hover:scale-[1.04]"
                  />
                ) : (
                  <div className="grid h-full w-full place-items-center bg-raised text-[12px] text-faint">
                    {t("platformName." + host.platform, { defaultValue: host.platform })}
                  </div>
                )}
                <div className="pointer-events-none absolute inset-0 bg-gradient-to-t from-black/75 via-black/10 to-transparent" />
                <div className="absolute left-2 top-2">
                  <PlatformChip platform={host.platform} dark />
                </div>
                <div className="pointer-events-none absolute inset-x-2.5 bottom-2 flex items-center gap-2">
                  <HostAvatar host={host} size={36} />
                  <div className="min-w-0">
                    <div className="truncate text-[13px] font-medium text-white drop-shadow">
                      {host.hostName || host.roomTitle || host.roomId}
                    </div>
                    <div className="truncate text-[10px] text-white/60">{host.roomTitle}</div>
                  </div>
                </div>
              </div>
              <div className="flex flex-col gap-0.5 px-2.5 py-2 text-[10.5px] text-muted">
                <div className="flex items-center gap-2">
                  <span className="font-mono tabular-nums">{t("media.hosts.count", { n: host.count })}</span>
                  <span className="text-line">·</span>
                  <span className="font-mono tabular-nums">{formatSize(host.bytes)}</span>
                </div>
                <div className="truncate font-mono tabular-nums text-faint">
                  {t("media.hosts.latest", { time: formatDate(host.latestAt) })}
                </div>
              </div>
            </button>
          ))}
        </div>
      )}

      {hostId && !loading && items.length > 0 && items.length < total ? (
        <div className="flex justify-center pb-2">
          <Button size="sm" variant="secondary" disabled={loadingMore} onClick={() => void loadFiles(items.length)}>
            {loadingMore ? t("common.loading") : t("media.loadMore")}
          </Button>
        </div>
      ) : null}

      {!hostId && !hostLoading && hosts.length > 0 && hosts.length < hostTotal ? (
        <div className="flex justify-center pb-2">
          <Button size="sm" variant="secondary" disabled={hostLoadingMore} onClick={() => void loadHosts(hosts.length)}>
            {hostLoadingMore ? t("common.loading") : t("media.loadMore")}
          </Button>
        </div>
      ) : null}

      {selected.length > 0
        ? createPortal(
            <div className="fixed bottom-5 left-1/2 z-40 flex -translate-x-1/2 animate-pop items-center gap-1.5 rounded-[10px] border border-line bg-surface px-3 py-2 shadow-pop">
              <span className="mr-1 whitespace-nowrap text-[11.5px] text-ink">
                {t("media.selectedSize", { n: selected.length, size: formatSize(selectedSize) })}
              </span>
              <span className="h-4 w-px bg-line" />
              <Button size="sm" variant="ghost" disabled={!!busy} onClick={() => void runBatch("remux")}>
                {t("media.remux")}
              </Button>
              <Button size="sm" variant="ghost" disabled={!!busy} onClick={() => void runBatch("ass")}>
                {t("media.exportAss")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={!!busy || selected.length < 2}
                onClick={() => void runBatch("merge")}
              >
                {t("media.merge")}
              </Button>
              <Button size="sm" variant="ghost" disabled={!!busy} onClick={() => void runBatch("reveal")}>
                {t("media.reveal")}
              </Button>
              <Button size="sm" variant="danger" disabled={!!busy} onClick={() => void runBatch("delete")}>
                {confirmDelete ? t("media.deleteConfirm", { n: selected.length }) : t("media.deleteSelected")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={!!busy}
                onClick={() => {
                  setSelected([]);
                  setConfirmDelete(false);
                }}
              >
                {t("media.clearSelection")}
              </Button>
            </div>,
            document.body,
          )
        : null}

      {playing
        ? createPortal(
            <div className="fixed inset-0 z-50 flex justify-end">
              <div
                className="absolute inset-0 animate-fade-in bg-black/35 backdrop-blur-[2px]"
                onClick={() => void closePlay()}
              />
              <aside className="relative my-auto flex max-h-[94vh] w-[min(760px,94vw)] animate-sheet flex-col overflow-hidden rounded-l-[10px] border border-r-0 border-line bg-surface shadow-2xl">
                <header className="flex h-14 shrink-0 items-center justify-between gap-3 border-b border-line px-5">
                  <div className="min-w-0">
                    <div className="text-[10.5px] uppercase tracking-[0.08em] text-faint">
                      {t("media.playingNow")}
                    </div>
                    <h2 className="truncate text-[13.5px] font-semibold text-ink">
                      {playing.item.title || playing.item.hostName}
                    </h2>
                  </div>
                  <button
                    type="button"
                    onClick={() => void closePlay()}
                    aria-label={t("media.close")}
                    className="focus-ring flex h-8 w-8 shrink-0 items-center justify-center rounded-ctl text-muted transition-colors hover:bg-raised hover:text-ink"
                  >
                    <IconClose />
                  </button>
                </header>

                <div className="min-h-0 flex-1 overflow-auto">
                  <div className="bg-black">
                    <div className="aspect-video w-full">
                      <Player
                        url={playing.playback.url}
                        volume={volume}
                        onVolumeChange={changeVolume}
                        onFatal={(detail) => setError(t("media.playFailed") + "：" + detail)}
                        durationMs={playing.item.durationMs}
                        startMs={playing.playback.startMs}
                        leadMs={playing.playback.leadMs}
                        startAtSec={playing.playback.offsetMs / 1000}
                        onSeek={(positionMs) => void seekTo(playing.item, positionMs)}
                      />
                    </div>
                  </div>

                  <div className="px-5 py-4">
                    <div className="grid grid-cols-2 gap-x-6 gap-y-3">
                      <div>
                        <div className="text-[10px] uppercase tracking-[0.08em] text-faint">{t("media.colPlatform")}</div>
                        <div className="mt-0.5 text-[12px] text-ink">
                          {t("platformName." + playing.item.platform, { defaultValue: playing.item.platform })}
                        </div>
                      </div>
                      <div>
                        <div className="text-[10px] uppercase tracking-[0.08em] text-faint">{t("media.colDuration")}</div>
                        <div className="mt-0.5 font-mono text-[12px] tabular-nums text-ink">
                          {formatDuration(playing.item.durationMs)}
                        </div>
                      </div>
                      <div>
                        <div className="text-[10px] uppercase tracking-[0.08em] text-faint">{t("media.colSize")}</div>
                        <div className="mt-0.5 font-mono text-[12px] tabular-nums text-ink">
                          {formatSize(playing.item.size)}
                        </div>
                      </div>
                      <div>
                        <div className="text-[10px] uppercase tracking-[0.08em] text-faint">{t("media.colDate")}</div>
                        <div className="mt-0.5 font-mono text-[12px] tabular-nums text-ink">
                          {formatDate(playing.item.startedAt)}
                        </div>
                      </div>
                    </div>

                    <div className="mt-4 flex flex-wrap items-center gap-1.5 border-t border-line pt-3">
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy !== null || playing.item.path.endsWith(".mp4")}
                        onClick={() => void postOne("remux", playing.item)}
                      >
                        {t("media.remux")}
                      </Button>
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy !== null || playing.item.danmakuCount === 0}
                        onClick={() => void postOne("ass", playing.item)}
                      >
                        {t("media.exportAss")}
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => void mediaReveal(playing.item.id).catch((err) => setError(describeError(err)))}
                      >
                        {t("media.reveal")}
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        className="ml-auto text-rec"
                        disabled={busy === playing.item.id}
                        onClick={() => void removeOne(playing.item)}
                      >
                        {t("media.delete")}
                      </Button>
                    </div>

                    {postPercent !== null ? (
                      <div className="mt-2 font-mono text-[10.5px] tabular-nums text-faint">{postPercent}%</div>
                    ) : null}
                  </div>
                </div>
              </aside>
            </div>,
            document.body,
          )
        : null}
    </div>
  );
}
