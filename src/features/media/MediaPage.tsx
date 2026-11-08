import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import Player from "../watch/Player";
import { Button, cx } from "../../components/ui/Controls";
import { SelectMenu } from "../../components/ui/SelectMenu";
import PageEmpty from "../../components/ui/PageEmpty";
import EmptyArt from "../../components/ui/EmptyArt";
import { Checkbox } from "../../components/arc/checkbox/checkbox";
import { useNotify } from "../../lib/notify";
import { PageBody, PageChip, PageHeader, PageSearch, PageShell, PageToolbar } from "../../components/ui/PageShell";
import {
  isTauri,
  mediaDeleteGroup,
  mediaDeleteSource,
  mediaFiles,
  mediaGroupClips,
  mediaHosts,
  mediaList,
  mediaPlay,
  mediaReveal,
  mediaRevealFile,
  mediaRevealSource,
  mediaStop,
  onMediaChanged,
  onMediaThumb,
  onPostProgress,
  postCancel,
  postRetry,
  sessionExportDanmaku,
  sessionRemux,
  sessionsMerge,
  type MediaFile,
  type MediaHost,
  type MediaItem,
  type Playback,
  type PlatformCount,
  type PostProgress,
} from "../../lib/ipc";

const PAGE_SIZE = 60;
const VOLUME_KEY = "hivecast.mediaVolume";

const FILE_ROLE: Record<string, string> = {
  main: "media.mainFile",
  source: "media.sourceFile",
  derived: "media.derivedFile",
};

type SortKey = "recent" | "duration" | "size";
type HostSortKey = "name" | "latest" | "size" | "count";
type Range = 0 | 1 | 7 | 30;

/** 媒体库卡片：一场直播下挂多个片段。 */
type MediaGroup = {
  id: string;
  clips: MediaItem[];
  primary: MediaItem;
  size: number;
  durationMs: number;
  danmakuCount: number;
  hasFile: boolean;
  clipCount: number;
};

function baseName(path: string): string {
  const index = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return index >= 0 ? path.slice(index + 1) : path;
}

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

function IconPlay() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <path d="M5.2 3.4c0-.5.55-.8.97-.53l6.1 4.1c.4.26.4.84 0 1.1l-6.1 4.1a.62.62 0 0 1-.97-.53V3.4Z" />
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
  const [groupLoading, setGroupLoading] = useState<string | null>(null);

  // —— 共用状态 ——
  const notify = useNotify();
  const [query, setQuery] = useState("");
  const [keyword, setKeyword] = useState("");
  const [platform, setPlatform] = useState("all");
  const [range, setRange] = useState<Range>(0);
  const [onlyDanmaku, setOnlyDanmaku] = useState(false);
  const [sort, setSort] = useState<SortKey>("recent");

  const [selected, setSelected] = useState<string[]>([]);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  // 后处理进度按 sessionId 存：自动收尾、手动转封装、重试互不覆盖。
  const [postJobs, setPostJobs] = useState<Record<string, PostProgress>>({});
  const [confirmSourceDelete, setConfirmSourceDelete] = useState(false);
  // 当前抽屉这条录像登记在册的全部文件；转封装/删源后重新拉。
  const [detailFiles, setDetailFiles] = useState<MediaFile[]>([]);

  const [playing, setPlaying] = useState<{
    item: MediaItem;
    playback: Playback;
    source: boolean;
  } | null>(null);
  const [playBusy, setPlayBusy] = useState<string | null>(null);
  const [volume, setVolume] = useState(loadVolume);

  // 缩略图事件可能比列表先到，先记在 ref 里，下次合并进 items。
  const thumbsRef = useRef<Record<string, string>>({});
  const playingRef = useRef(playing);
  playingRef.current = playing;

  useEffect(() => {
    const item = playing?.item;
    if (!item) {
      setDetailFiles([]);
      return;
    }
    let alive = true;
    void mediaFiles(item.id)
      .then((files) => {
        if (alive) setDetailFiles(files);
      })
      .catch(() => {
        if (alive) setDetailFiles([]);
      });
    return () => {
      alive = false;
    };
  }, [playing?.item.id, playing?.item.path, playing?.item.postState, playing?.item.sourceDeleted]);

  // 搜索防抖：每敲一个字都查一次库没必要。
  useEffect(() => {
    const timer = window.setTimeout(() => setKeyword(query.trim()), 220);
    return () => window.clearTimeout(timer);
  }, [query]);

  const loadHosts = useCallback(
    async (offset: number) => {
      if (!isTauri()) {
        setHostLoading(false);
        return true;
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
        return true;
      } catch (err) {
        notify.error(err);
        return false;
      } finally {
        setHostLoading(false);
        setHostLoadingMore(false);
      }
    },
    [keyword, platform, hostSort, notify],
  );

  const loadFiles = useCallback(
    async (offset: number) => {
      if (!hostId) return false;
      if (!isTauri()) {
        setLoading(false);
        return true;
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
        return true;
      } catch (err) {
        notify.error(err);
        return false;
      } finally {
        setLoading(false);
        setLoadingMore(false);
      }
    },
    [hostId, keyword, range, onlyDanmaku, sort, notify],
  );

  // 分页只按片段返回，一场直播可能只出现在本页一部分；点“+N”补齐整组。
  const loadGroupClips = useCallback(
    async (group: MediaGroup) => {
      setGroupLoading(group.id);
      try {
        const clips = await mediaGroupClips(group.id);
        const withThumbs = clips.map((clip) => ({
          ...clip,
          thumbnailUrl: clip.thumbnailUrl ?? thumbsRef.current[clip.id] ?? null,
        }));
        setItems((prev) => {
          const byId = new Map(prev.map((item) => [item.id, item]));
          for (const clip of withThumbs) byId.set(clip.id, clip);
          return Array.from(byId.values());
        });
      } catch (err) {
        notify.error(err);
      } finally {
        setGroupLoading(null);
      }
    },
    [notify],
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
    const timers = new Set<number>();
    void onPostProgress((one) => {
      setPostJobs((prev) => ({ ...prev, [one.sessionId]: one }));
      if (one.state === "done" || one.state === "cancelled") {
        setPostJobs((prev) => {
          const next = { ...prev };
          delete next[one.sessionId];
          return next;
        });
      }
      // 事件比 DB 提交早：等落库后再刷新，避免读到旧的 post_state。
      if (one.state === "done" || one.state === "failed" || one.state === "cancelled") {
        const timer = window.setTimeout(() => {
          timers.delete(timer);
          if (hostId) void loadFiles(0);
          else void loadHosts(0);
        }, 500);
        timers.add(timer);
      }
    })
      .then((off) => {
        dispose = off;
      })
      .catch(() => undefined);
    return () => {
      dispose?.();
      for (const timer of timers) window.clearTimeout(timer);
    };
  }, [hostId, loadFiles, loadHosts]);

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
    setHostId(host.roomId);
  };

  const backToHosts = () => {
    setHostId(null);
    setSelected([]);
    setConfirmDelete(false);
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

  // 后端按片段返回；同一场的片段聚成一张卡片，避免媒体库被同标题刷屏。
  const groups = useMemo(() => {
    const map = new Map<string, MediaItem[]>();
    for (const item of items) {
      const key = item.groupId || item.id;
      const list = map.get(key);
      if (list) list.push(item);
      else map.set(key, [item]);
    }
    return [...map.entries()].map(([id, clips]) => {
      const ordered = [...clips].sort(
        (a, b) => a.clipIndex - b.clipIndex || a.startedAt - b.startedAt,
      );
      const primary = ordered[0];
      return {
        id,
        clips: ordered,
        primary,
        size: ordered.reduce((sum, clip) => sum + Math.max(0, clip.size), 0),
        durationMs: ordered.reduce((sum, clip) => sum + Math.max(0, clip.durationMs), 0),
        danmakuCount: ordered.reduce((sum, clip) => sum + Math.max(0, clip.danmakuCount), 0),
        hasFile: ordered.some((clip) => clip.hasFile),
        clipCount: Math.max(1, primary.clipCount || ordered.length),
      } satisfies MediaGroup;
    });
  }, [items]);

  // 选中一个片段等于选中整个场次；提示数量要按整场算，不能只算当前已加载的片段。
  const deleteGroups = useMemo(
    () => groups.filter((group) => group.clips.some((clip) => selected.includes(clip.id))),
    [groups, selected],
  );
  const deleteClipCount = useMemo(
    () => deleteGroups.reduce((sum, group) => sum + group.clipCount, 0),
    [deleteGroups],
  );

  const toggleGroup = (group: MediaGroup) => {
    setConfirmDelete(false);
    const ids = group.clips.map((clip) => clip.id);
    setSelected((prev) => {
      const all = ids.every((id) => prev.includes(id));
      return all ? prev.filter((id) => !ids.includes(id)) : [...new Set([...prev, ...ids])];
    });
  };

  const stopPlayback = async (id: string) => {
    await mediaStop(id).catch(() => undefined);
  };

  const closePlay = async () => {
    const current = playing;
    setPlaying(null);
    setConfirmSourceDelete(false);
    if (current) await stopPlayback(current.item.id);
  };

  const openPlay = async (item: MediaItem, source = false) => {
    if (playing && playing.item.id === item.id && playing.source === source) {
      await closePlay();
      return;
    }
    if (playing) {
      const previous = playing.item.id;
      setPlaying(null);
      await stopPlayback(previous);
    }
    setPlayBusy(item.id);
    setConfirmSourceDelete(false);
    try {
      const playback = await mediaPlay(item.id, null, source);
      setPlaying({ item, playback, source });
    } catch (err) {
      notify.error(err, t("media.playFailed"));
    } finally {
      setPlayBusy(null);
    }
  };

  // 同一个抽屉里切主文件/源封装：先停旧变体，再按新变体起播。
  const switchVariant = async (source: boolean) => {
    if (!playing || playing.source === source) return;
    const item = playing.item;
    setPlayBusy(item.id);
    setConfirmSourceDelete(false);
    try {
      await stopPlayback(item.id);
      const playback = await mediaPlay(item.id, null, source);
      setPlaying({ item, playback, source });
    } catch (err) {
      notify.error(err, t("media.playFailed"));
    } finally {
      setPlayBusy(null);
    }
  };

  const revealSource = async (item: MediaItem) => {
    try {
      await mediaRevealSource(item.id);
    } catch (err) {
      notify.error(err);
    }
  };

  const revealFile = async (file: MediaFile) => {
    try {
      await mediaRevealFile(file.sessionId, file.path);
    } catch (err) {
      notify.error(err);
    }
  };

  // 拖到当前窗口外：后端换一个窗口重开，播放器从新窗口的偏移起播。
  const seekTo = async (item: MediaItem, positionMs: number) => {
    try {
      const source = playing?.source ?? false;
      const playback = await mediaPlay(item.id, positionMs, source);
      setPlaying({ item, playback, source });
    } catch (err) {
      notify.error(err, t("media.playFailed"));
    }
  };

  // Esc 先关回放抽屉，其次退回主播墙。
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (playingRef.current) {
        void mediaStop(playingRef.current.item.id).catch(() => undefined);
        setPlaying(null);
        setConfirmSourceDelete(false);
        return;
      }
      if (hostId) backToHosts();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hostId]);

  const refreshAll = async () => {
    if (hostId) {
      const files = await loadFiles(0);
      const hosts = await loadHosts(0);
      return files && hosts;
    }
    return loadHosts(0);
  };

  const refreshView = async () => {
    if (await refreshAll()) notify.success(t("media.refreshed"));
  };

  const runBatch = async (kind: "remux" | "ass" | "merge" | "reveal" | "delete") => {
    if (selectedItems.length === 0 || busy) return;

    if (kind === "reveal") {
      let opened = 0;
      for (const item of selectedItems) {
        try {
          await mediaReveal(item.id);
          opened += 1;
        } catch (err) {
          notify.error(err);
        }
      }
      if (opened > 0) notify.info(t("media.revealed"));
      return;
    }

    if (kind === "delete" && !confirmDelete) {
      setConfirmDelete(true);
      return;
    }

    setBusy(kind);
    setProgress(null);
    try {
      if (kind === "merge") {
        if (selectedItems.length < 2) {
          notify.warning(t("media.mergeHint"));
          return;
        }
        const result = await sessionsMerge(selectedItems.map((item) => item.id));
        notify.success(t("media.mergeDone", { size: formatSize(result.size) }));
        setSelected([]);
        await refreshAll();
        return;
      }

      if (kind === "delete") {
        for (const group of deleteGroups) {
          if (playing && group.clips.some((clip) => clip.id === playing.item.id)) await closePlay();
          await mediaDeleteGroup(group.id, group.hasFile);
        }
        notify.success(t("media.deletedBatch", { n: deleteClipCount }));
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
      notify.success(
        kind === "remux"
          ? t("media.remuxDone", { n: done })
          : t("media.exportDone", { n: done }),
      );
      setSelected([]);
      await refreshAll();
    } catch (err) {
      // 整组删除是逐段进行的：中途失败时前面的片段可能已经删掉，
      // 刷新一下列表，避免界面还留着已经不在的卡片。
      if (kind === "delete") await refreshAll();
      notify.error(err, t("media.postFailed"));
    } finally {
      setBusy(null);
      setProgress(null);
    }
  };

  // 抽屉里的单条后处理：只作用于当前正在回放的这条。
  const postOne = async (kind: "remux" | "ass", item: MediaItem) => {
    if (busy) return;
    setBusy(kind);
    try {
      if (kind === "remux") {
        const result = await sessionRemux(item.id);
        const switched = result.path !== item.path;
        const sourcePath = item.sourcePath ?? (switched ? item.path : null);
        // 源已经进过回收站就保持已删状态；不要把「仅删源」的结果覆盖回可见。
        const sourceDeleted = Boolean(item.sourcePath) && item.sourceDeleted;
        const updated: MediaItem = {
          ...item,
          path: result.path,
          size: result.size,
          sourcePath,
          hasSource: sourcePath ? item.hasSource || (switched && !item.sourcePath) : false,
          postMode: "convert",
          postState: "done",
          postError: null,
          sourceDeleted,
        };
        setItems((prev) => prev.map((current) => (current.id === item.id ? updated : current)));
        setPlaying((prev) => (prev && prev.item.id === item.id ? { ...prev, item: updated } : prev));
        notify.success(t("media.remuxOne", { size: formatSize(result.size) }));
      } else {
        await sessionExportDanmaku(item.id);
        notify.success(t("media.exportOne"));
      }
      await refreshAll();
    } catch (err) {
      notify.error(err, t("media.postFailed"));
      await refreshAll();
    } finally {
      setBusy(null);
    }
  };

  // 后处理失败/取消后重试；后端会用当前设置重新跑，并把结果写回这条录像。
  const retryOne = async (item: MediaItem) => {
    if (busy) return;
    setBusy("retry:" + item.id);
    try {
      const result = await postRetry(item.id);
      notify.success(t("media.retryDone", { size: formatSize(result.size) }));
      await refreshAll();
    } catch (err) {
      notify.error(err, t("media.postFailed"));
      await refreshAll();
    } finally {
      setBusy(null);
    }
  };

  const cancelOne = async (item: MediaItem) => {
    try {
      const count = await postCancel(item.id);
      notify.info(count > 0 ? t("media.cancelRequested") : t("media.cancelIdle"));
    } catch (err) {
      notify.error(err);
    }
  };

  // 只删源封装：后端会先 ffprobe 校验主文件，失败时不会动源文件。
  const deleteSource = async (item: MediaItem) => {
    if (busy) return;
    if (!confirmSourceDelete) {
      setConfirmSourceDelete(true);
      return;
    }
    setBusy("delete-source:" + item.id);
    try {
      await mediaDeleteSource(item.id);
      setConfirmSourceDelete(false);
      // 正在放的就是被删的源封装：先关抽屉，避免播放器停在已删除的流上。
      if (playing?.item.id === item.id && playing.source) await closePlay();
      const updated = { ...item, sourceDeleted: true, hasSource: false };
      setItems((prev) => prev.map((current) => (current.id === item.id ? updated : current)));
      setPlaying((prev) =>
        prev && prev.item.id === item.id ? { ...prev, item: updated } : prev,
      );
      notify.success(t("media.sourceDeleteDone"));
      await refreshAll();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusy(null);
    }
  };

  const removeOne = async (item: MediaItem) => {
    setBusy(item.id);
    try {
      // 片段不是独立录像：只删其中一段会让整场缺号。抽屉里的删除也按整场处理。
      const owner = groups.find((group) => group.clips.some((clip) => clip.id === item.id));
      const groupId = owner?.id ?? item.groupId ?? item.id;
      const deleteFile = owner?.hasFile ?? item.hasFile;
      if (
        playing &&
        (playing.item.id === item.id || (playing.item.groupId || playing.item.id) === groupId)
      ) {
        await closePlay();
      }
      await mediaDeleteGroup(groupId, deleteFile);
      notify.success(t("media.deletedBatch", { n: owner?.clipCount ?? item.clipCount ?? 1 }));
      await refreshAll();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusy(null);
    }
  };

  const allSelected =
    groups.length > 0 &&
    groups.every((group) => group.clips.every((clip) => selected.includes(clip.id)));
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

  const platformMenu = (
    <SelectMenu
      variant="bare"
      align="start"
      active={platform !== "all"}
      value={platform}
      onChange={setPlatform}
      placeholder={
        platform === "all"
          ? t("media.platformFilter")
          : t("platformName." + platform, { defaultValue: platform })
      }
      options={platforms.map((one) => ({
        value: one,
        label: one === "all" ? t("media.filterAll") : t("platformName." + one, { defaultValue: one }),
        hint: String(countOf(one)),
      }))}
    />
  );

  return (
    <PageShell>
      {hostId ? (
        <PageHeader
          leading={
            <button
              type="button"
              onClick={backToHosts}
              className="focus-ring -ml-1 inline-flex w-fit items-center gap-1 rounded-[4px] px-1 py-0.5 text-[11.5px] text-muted transition-colors hover:text-ink"
            >
              <IconBack />
              {t("media.hosts.back")}
            </button>
          }
          prefix={
            active ? (
              <span className="block overflow-hidden rounded-[10px]">
                <HostAvatar host={active} size={36} onCover={false} />
              </span>
            ) : undefined
          }
          title={active ? active.hostName || active.roomTitle || active.roomId : t("media.title")}
          subtitle={
            active ? (
              <span className="flex flex-wrap items-center gap-2">
                <span className="font-mono tabular-nums">{t("media.hosts.count", { n: active.count })}</span>
                <span className="text-line">·</span>
                <span className="font-mono tabular-nums">{formatSize(active.bytes)}</span>
                <span className="text-line">·</span>
                <span className="font-mono tabular-nums">
                  {t("media.hosts.latest", { time: formatDate(active.latestAt) })}
                </span>
                <span className="text-line">·</span>
                <span className="truncate">{active.roomTitle}</span>
              </span>
            ) : (
              t("media.subtitle")
            )
          }
          actions={
            active ? (
              <>
                <Button
                  size="lg"
                  variant="secondary"
                  disabled={items.length === 0}
                  onClick={() => setSelected(allSelected ? [] : items.map((one) => one.id))}
                >
                  {t("media.selectAll")}
                </Button>
                <Button size="lg" variant="secondary" disabled={loading} onClick={() => void refreshView()}>
                  {t("common.refresh")}
                </Button>
              </>
            ) : undefined
          }
        />
      ) : (
        <PageHeader
          title={t("media.title")}
          subtitle={t("media.subtitle")}
          actions={
            <>
              <PageSearch value={query} onChange={setQuery} placeholder={t("media.hosts.search")} />
              <Button size="lg" variant="secondary" disabled={hostLoading} onClick={() => void refreshView()}>
                {t("common.refresh")}
              </Button>
            </>
          }
        />
      )}

      {hostId ? (
        <PageToolbar>
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
          <PageChip active={onlyDanmaku} onClick={() => setOnlyDanmaku((on) => !on)}>
            {t("media.onlyDanmaku")}
          </PageChip>
          <SelectMenu
            size="md"
            value={sort}
            onChange={(next) => setSort(next as SortKey)}
            options={[
              { value: "recent", label: t("media.sortRecent") },
              { value: "duration", label: t("media.sortDuration") },
              { value: "size", label: t("media.sortSize") },
            ]}
            ariaLabel={t("media.sortLabel")}
            className="ml-auto w-[140px] justify-between"
          />
        </PageToolbar>
      ) : (
        <PageToolbar>
          {platformMenu}
          <SelectMenu
            size="md"
            value={hostSort}
            onChange={(next) => setHostSort(next as HostSortKey)}
            options={[
              { value: "name", label: t("media.hosts.sortName") },
              { value: "latest", label: t("media.hosts.sortLatest") },
              { value: "size", label: t("media.hosts.sortSize") },
              { value: "count", label: t("media.hosts.sortCount") },
            ]}
            ariaLabel={t("media.sortLabel")}
            className="ml-auto w-[140px] justify-between"
          />
        </PageToolbar>
      )}
      {statusLine ? (
        <div className="mx-5 mb-2 flex items-center justify-between rounded-ctl border border-line bg-raised px-3 py-2 text-[11.5px] text-ink-soft">
          <span className="truncate">{statusLine}</span>
        </div>
      ) : null}

      <PageBody className="pb-5">
      {hostId ? (
        loading ? (
          <div className="flex flex-1 items-center justify-center text-[12px] text-faint">{t("common.loading")}</div>
        ) : !active ? (
          <PageEmpty
            className="flex-1"
            title={t("media.hosts.goneTitle")}
            description={t("media.hosts.goneBody")}
            icon={<EmptyArt kind="gone" />}
          />
        ) : items.length === 0 ? (
          <PageEmpty
            className="flex-1"
            title={fileFiltersActive ? t("media.noMatchTitle") : t("media.emptyTitle")}
            description={fileFiltersActive ? t("media.noMatchBody") : t("media.emptyBody")}
            icon={<EmptyArt kind={fileFiltersActive ? "search" : "media"} />}
          />
        ) : (
          <div className="grid gap-3" style={GRID_STYLE}>
            {groups.map((group) => {
              const item = group.primary;
              // 封面优先播放第一个真的有文件的片段：首片段被单独删掉/文件丢失时，
              // 后面仍在的片段不能因为 primary 不可播而整组都点不开。
              const playable = group.clips.find((clip) => clip.hasFile) ?? group.primary;
              const isSelected = group.clips.every((clip) => selected.includes(clip.id));
              const isPlaying = group.clips.some((clip) => clip.id === playing?.item.id);
              const isBusy = group.clips.some((clip) => clip.id === playBusy);
              const job = postJobs[playable.id] ?? postJobs[item.id];
              const jobRunning = job?.state === "queued" || job?.state === "running";
              const retryable = item.postState === "failed" || item.postState === "cancelled";
              const missingClips = Math.max(0, group.clipCount - group.clips.length);
              return (
                <article
                  key={group.id}
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
                        {group.hasFile ? t("media.thumbsPending") : t("media.missing")}
                      </div>
                    )}

                    <button
                      type="button"
                      onClick={() => void openPlay(playable)}
                      disabled={!group.hasFile}
                      aria-label={t("media.play")}
                      className="absolute inset-0 flex items-center justify-center disabled:cursor-not-allowed"
                    >
                      <span className="flex h-10 w-10 items-center justify-center rounded-full bg-black/55 text-white opacity-0 backdrop-blur-[2px] transition-opacity duration-150 group-hover:opacity-100">
                        {isBusy ? (
                          <span className="h-3.5 w-3.5 animate-spin rounded-full border-[1.5px] border-white/40 border-t-white" />
                        ) : (
                          <IconPlay />
                        )}
                      </span>
                    </button>

                    <span className="pointer-events-none absolute left-2 top-2 rounded-[4px] bg-black/55 px-1.5 py-[3px] text-[10px] font-medium leading-none text-white/90 backdrop-blur-[2px]">
                      {t("platformName." + item.platform, { defaultValue: item.platform })}
                    </span>

                    {jobRunning ? (
                      <button
                        type="button"
                        onClick={() => void cancelOne(playable)}
                        className="focus-ring absolute left-2 top-[26px] z-10 rounded-[4px] bg-black/60 px-1.5 py-[3px] text-[10px] leading-none text-white/90 backdrop-blur-[2px] hover:bg-black/80"
                      >
                        {job.stage === "queued"
                          ? t("media.postQueued")
                          : job.percent + "% · " + t("media.cancelPost")}
                      </button>
                    ) : retryable ? (
                      <button
                        type="button"
                        onClick={() => void retryOne(playable)}
                        title={item.postError ?? undefined}
                        className="focus-ring absolute left-2 top-[26px] z-10 rounded-[4px] bg-black/60 px-1.5 py-[3px] text-[10px] leading-none text-red-200 backdrop-blur-[2px] hover:bg-black/80"
                      >
                        {item.postState === "cancelled" ? t("media.postCancelled") : t("media.postFailed")} ·{" "}
                        {t("media.retry")}
                      </button>
                    ) : (
                      <span className="pointer-events-none absolute left-2 top-[26px] rounded-[4px] bg-black/55 px-1.5 py-[3px] text-[10px] leading-none text-white/85 backdrop-blur-[2px]">
                        {item.postMode === "convert"
                          ? t("media.postConverted")
                          : item.postMode === "custom"
                            ? t("media.postCustom")
                            : t("media.postSource")}
                        {item.sourceDeleted ? " · " + t("media.sourceDeleted") : ""}
                      </span>
                    )}

                    <div className="absolute right-0 top-0 z-10">
                      <Checkbox
                        checked={isSelected}
                        onCheckedChange={() => toggleGroup(group)}
                        aria-label={t("media.selectSession")}
                        tone="overlay"
                      />
                    </div>

                    <span className="pointer-events-none absolute bottom-2 left-2 rounded-[4px] bg-black/60 px-1.5 py-[3px] font-mono text-[10px] tabular-nums leading-none text-white/90">
                      {formatDuration(group.durationMs)}
                    </span>
                    {group.danmakuCount > 0 ? (
                      <span className="pointer-events-none absolute bottom-2 right-2 rounded-[4px] bg-black/60 px-1.5 py-[3px] text-[10px] leading-none text-white/85">
                        {t("media.danmakuCount", { n: group.danmakuCount })}
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
                    {item.sourcePath && item.sourcePath !== item.path ? (
                      <div className="mt-0.5 space-y-0.5 rounded-[5px] bg-raised/60 px-1.5 py-1 text-[10px] leading-4">
                        <div className="flex min-w-0 items-center gap-1">
                          <span className="shrink-0 text-faint">{t("media.mainFile")}</span>
                          <span className="truncate font-mono text-muted" title={item.path}>
                            {baseName(item.path)}
                          </span>
                        </div>
                        <div className="flex min-w-0 items-center gap-1">
                          <span className="shrink-0 text-faint">{t("media.sourceFile")}</span>
                          {item.hasSource ? (
                            <button
                              type="button"
                              onClick={() => void revealSource(item)}
                              title={item.sourcePath ?? undefined}
                              className="focus-ring truncate text-left font-mono text-accent-text hover:underline"
                            >
                              {baseName(item.sourcePath)}
                            </button>
                          ) : (
                            <span className="truncate text-faint">{t("media.sourceDeleted")}</span>
                          )}
                        </div>
                      </div>
                    ) : null}
                    {group.clipCount > 1 ? (
                      <div className="flex flex-wrap items-center gap-1">
                        {group.clips.map((clip) => (
                          <button
                            key={clip.id}
                            type="button"
                            disabled={!clip.hasFile}
                            onClick={() => void openPlay(clip)}
                            title={t("media.clipIndex", { n: clip.clipIndex + 1 })}
                            className={cx(
                              "focus-ring rounded-[4px] border px-1.5 py-[1px] font-mono text-[10px] tabular-nums transition-colors disabled:cursor-not-allowed disabled:opacity-40",
                              playing?.item.id === clip.id
                                ? "border-accent bg-accent-soft text-accent-text"
                                : "border-line text-faint hover:border-line-strong hover:text-ink",
                            )}
                          >
                            {clip.clipIndex + 1}
                          </button>
                        ))}
                        {missingClips > 0 ? (
                          <button
                            type="button"
                            disabled={groupLoading === group.id}
                            onClick={() => void loadGroupClips(group)}
                            title={t("media.loadClips", { n: missingClips })}
                            className="focus-ring rounded-[4px] border border-dashed border-line px-1.5 py-[1px] font-mono text-[10px] text-faint transition-colors hover:border-line-strong hover:text-ink disabled:opacity-50"
                          >
                            {groupLoading === group.id ? "…" : "+" + missingClips}
                          </button>
                        ) : null}
                      </div>
                    ) : null}
                    <div className="mt-auto flex items-center justify-between gap-2 pt-0.5 text-[10.5px] text-faint">
                      <span className="font-mono tabular-nums">{formatDate(item.startedAt)}</span>
                      <span className="shrink-0 font-mono tabular-nums">{formatSize(group.size)}</span>
                    </div>
                    {group.clipCount > 1 && !isPlaying ? (
                      <div className="text-[10.5px] text-faint">
                        {t("media.clipCount", { n: group.clipCount })}
                      </div>
                    ) : null}
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
        <PageEmpty
          className="flex-1"
          title={hostFiltersActive ? t("media.hosts.noMatchTitle") : t("media.hosts.emptyTitle")}
          description={hostFiltersActive ? t("media.hosts.noMatchBody") : t("media.hosts.emptyBody")}
          icon={<EmptyArt kind={hostFiltersActive ? "search" : "media"} />}
        />
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
      </PageBody>

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
                {confirmDelete ? t("media.deleteConfirm", { n: deleteClipCount }) : t("media.deleteSelected")}
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
                        onFatal={(detail) => notify.error(detail, t("media.playFailed"))}
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

                    {detailFiles.length > 1 ? (
                      <div className="mt-4 rounded-[7px] border border-line bg-raised/40 px-3 py-2">
                        <div className="flex items-baseline gap-2">
                          <div className="text-[10px] uppercase tracking-[0.08em] text-faint">
                            {t("media.files")}
                          </div>
                          <span className="text-[10px] tabular-nums text-faint">{detailFiles.length}</span>
                        </div>
                        <div className="mt-1.5 space-y-1 font-mono text-[10.5px] leading-4">
                          {detailFiles.map((file) => {
                            const active = file.state === "present" && file.exists;
                            return (
                              <div key={file.path} className="flex min-w-0 items-center gap-2">
                                <span className="w-[52px] shrink-0 font-sans text-faint">
                                  {t(FILE_ROLE[file.role] ?? "media.derivedFile")}
                                </span>
                                {active ? (
                                  <button
                                    type="button"
                                    onClick={() => void revealFile(file)}
                                    title={file.path}
                                    className="focus-ring truncate text-left text-accent-text hover:underline"
                                  >
                                    {baseName(file.path)}
                                  </button>
                                ) : (
                                  <span className="truncate text-faint">
                                    {file.state === "deleted" ? t("media.fileDeleted") : t("media.fileMissing")}
                                  </span>
                                )}
                                <span className="ml-auto shrink-0 font-sans text-[10px] tabular-nums text-faint">
                                  {file.format ? file.format.toUpperCase() + " · " : ""}
                                  {formatSize(file.size)}
                                </span>
                              </div>
                            );
                          })}
                        </div>
                        <div className="mt-2 flex flex-wrap items-center gap-1.5">
                          <Button
                            size="sm"
                            variant={playing.source ? "ghost" : "secondary"}
                            disabled={playBusy !== null}
                            onClick={() => void switchVariant(false)}
                          >
                            {t("media.playMain")}
                          </Button>
                          <Button
                            size="sm"
                            variant={playing.source ? "secondary" : "ghost"}
                            disabled={playBusy !== null || !playing.item.hasSource}
                            onClick={() => void switchVariant(true)}
                          >
                            {t("media.playSource")}
                          </Button>
                          {playing.item.hasSource && playing.item.sourcePath !== playing.item.path ? (
                            <Button
                              size="sm"
                              variant="ghost"
                              className="text-rec"
                              disabled={busy !== null}
                              onClick={() => void deleteSource(playing.item)}
                            >
                              {confirmSourceDelete ? t("media.confirmDeleteSource") : t("media.deleteSource")}
                            </Button>
                          ) : null}
                        </div>
                      </div>
                    ) : null}

                    <div className="mt-4 flex flex-wrap items-center gap-1.5 border-t border-line pt-3">
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy !== null || playing.item.path.endsWith(".mp4")}
                        onClick={() => void postOne("remux", playing.item)}
                      >
                        {t("media.remux")}
                      </Button>
                      {playing.item.postState === "failed" || playing.item.postState === "cancelled" ? (
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={busy !== null}
                          onClick={() => void retryOne(playing.item)}
                        >
                          {t("media.retry")}
                        </Button>
                      ) : null}
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
                        onClick={() => void mediaReveal(playing.item.id).catch((err) => notify.error(err))}
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

                    {postJobs[playing.item.id] ? (
                      <div className="mt-2 flex items-center gap-2 font-mono text-[10.5px] tabular-nums text-faint">
                        <span>
                          {postJobs[playing.item.id].stage === "queued"
                            ? t("media.postQueued")
                            : postJobs[playing.item.id].percent + "%"}
                        </span>
                        {postJobs[playing.item.id].state === "queued" ||
                        postJobs[playing.item.id].state === "running" ? (
                          <button
                            type="button"
                            onClick={() => void cancelOne(playing.item)}
                            className="focus-ring font-sans text-accent-text hover:underline"
                          >
                            {t("media.cancelPost")}
                          </button>
                        ) : null}
                        {postJobs[playing.item.id].message ? (
                          <span className="truncate font-sans text-rec">
                            {postJobs[playing.item.id].message}
                          </span>
                        ) : null}
                      </div>
                    ) : null}
                  </div>
                </div>
              </aside>
            </div>,
            document.body,
          )
        : null}
    </PageShell>
  );
}
