import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import Player from "../watch/Player";
import DanmakuCanvas from "../watch/DanmakuCanvas";
import { Button, cx } from "../../components/ui/Controls";
import { SelectMenu } from "../../components/ui/SelectMenu";
import PageEmpty from "../../components/ui/PageEmpty";
import EmptyArt from "../../components/ui/EmptyArt";
import { useNotify } from "../../lib/notify";
import { PageBody, PageChip, PageHeader, PageSearch, PageShell, PageToolbar } from "../../components/ui/PageShell";
import {
  isTauri,
  mediaDeleteGroup,
  mediaDeleteSource,
  mediaDanmaku,
  mediaFiles,
  mediaGroupClips,
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
  type DanmakuMessage,
  type MediaFile,
  type MediaItem,
  type Playback,
  type PostProgress,
  type RoomRecord,
} from "../../lib/ipc";

const PAGE_SIZE = 60;
const VOLUME_KEY = "hivecast.mediaVolume";
// 回放弹幕按播放位置附近取窗口：长录像的整轨可达几十 MB，不能一次塞进 webview。
const DANMAKU_WINDOW_MS = 120_000;
const DANMAKU_MARGIN_MS = 30_000;

const FILE_ROLE: Record<string, string> = {
  main: "media.mainFile",
  source: "media.sourceFile",
  derived: "media.derivedFile",
};

type SortKey = "recent" | "duration" | "size";
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

/** 单个直播间的录制文件：从场次列表进入，不再有全局主播墙。 */
export default function RoomRecordingsPage({
  room,
  onClose,
}: {
  room: RoomRecord;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const hostId = room.id;

  // —— 这个房间的录像 ——
  const [items, setItems] = useState<MediaItem[]>([]);
  const [total, setTotal] = useState(0);
  // 已从服务端消费的片段数。点「+N」补组会往 items 里塞额外片段，
  // 不能用 items.length 当分页 offset，否则会跳过或重复后面的片段。
  const [loadedRows, setLoadedRows] = useState(0);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [groupLoading, setGroupLoading] = useState<string | null>(null);

  // —— 共用状态 ——
  const notify = useNotify();
  const [query, setQuery] = useState("");
  const [keyword, setKeyword] = useState("");
  const [range, setRange] = useState<Range>(0);
  const [onlyDanmaku, setOnlyDanmaku] = useState(false);
  const [sort, setSort] = useState<SortKey>("recent");

  const [busy, setBusy] = useState<string | null>(null);
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
  // 录像弹幕轨：从 JSONL 读进内存，由播放位置驱动，不订阅直播流。
  const [danmakuTrack, setDanmakuTrack] = useState<DanmakuMessage[] | null>(null);
  const [danmakuWindow, setDanmakuWindow] = useState<{
    sessionId: string;
    from: number;
    to: number;
  } | null>(null);
  const danmakuSeqRef = useRef(0);
  const danmakuLoadingRef = useRef(false);
  const [danmakuLoading, setDanmakuLoading] = useState(false);
  const [danmakuOn, setDanmakuOn] = useState(true);
  const [playPosition, setPlayPosition] = useState(0);

  // 缩略图事件可能比列表先到，先记在 ref 里，下次合并进 items。
  const thumbsRef = useRef<Record<string, string>>({});
  // 筛选/排序连续变化时只认最后一次请求，避免慢响应覆盖新结果。
  const loadSeq = useRef(0);
  const playingRef = useRef(playing);
  playingRef.current = playing;
  // 离开页面或请求被后一次点击取代时，迟到的 mediaPlay 结果必须自己停掉，
  // 否则后端回放槽位会一直被占着（文档 196-12）。
  const mountedRef = useRef(true);
  const playSeqRef = useRef(0);
  const desiredPlayRef = useRef<{ id: string; source: boolean } | null>(null);

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

  const loadDanmakuWindow = useCallback((item: MediaItem, positionMs: number) => {
    if (danmakuLoadingRef.current) return;
    const seq = ++danmakuSeqRef.current;
    // 播放器上报的是毫秒浮点数；IPC 的 fromMs/toMs 是 i64，
    // 直接传小数会被 Tauri 拒绝，窗口就永远停在加载中。
    const position = Math.round(positionMs);
    const from = Math.max(0, position - DANMAKU_WINDOW_MS);
    const to = position + DANMAKU_WINDOW_MS;
    danmakuLoadingRef.current = true;
    setDanmakuLoading(true);
    void mediaDanmaku(item.id, from, to)
      .then((messages) => {
        if (seq !== danmakuSeqRef.current) return;
        setDanmakuTrack(messages);
        setDanmakuWindow({ sessionId: item.id, from, to });
      })
      .catch(() => {
        if (seq === danmakuSeqRef.current) {
          setDanmakuTrack([]);
          // 失败也要推进窗口，否则同一段位置会让每个播放事件都重试一次。
          setDanmakuWindow({ sessionId: item.id, from, to });
        }
      })
      .finally(() => {
        danmakuLoadingRef.current = false;
        if (seq === danmakuSeqRef.current) setDanmakuLoading(false);
      });
  }, []);

  useEffect(() => {
    const item = playing?.item;
    const offset = playing?.playback.offsetMs ?? 0;
    danmakuSeqRef.current += 1;
    danmakuLoadingRef.current = false;
    setPlayPosition(offset);
    setDanmakuTrack(null);
    setDanmakuWindow(null);
    if (!item?.danmakuPath) {
      setDanmakuLoading(false);
      return;
    }
    loadDanmakuWindow(item, offset);
  }, [playing?.item.id, playing?.item.danmakuPath, loadDanmakuWindow]);

  useEffect(() => {
    if (playing) setPlayPosition(playing.playback.offsetMs);
  }, [playing?.playback.offsetMs]);

  // 接近当前窗口的边缘才换窗口；注意 from=0 时不能再用 from+margin
  // 判断，否则开场前 30 秒每一帧都会重载同一段。
  useEffect(() => {
    const item = playing?.item;
    if (!item?.danmakuPath || !danmakuWindow || danmakuLoading) return;
    if (danmakuWindow.sessionId !== item.id) return;
    const needsEarlier =
      danmakuWindow.from > 0 && playPosition < danmakuWindow.from + DANMAKU_MARGIN_MS;
    const needsLater = playPosition > danmakuWindow.to - DANMAKU_MARGIN_MS;
    if (!needsEarlier && !needsLater) return;
    loadDanmakuWindow(item, playPosition);
  }, [playPosition, playing?.item.id, playing?.item.danmakuPath, danmakuWindow, danmakuLoading, loadDanmakuWindow]);

  // 搜索防抖：每敲一个字都查一次库没必要。
  useEffect(() => {
    const timer = window.setTimeout(() => setKeyword(query.trim()), 220);
    return () => window.clearTimeout(timer);
  }, [query]);

  const loadFiles = useCallback(
    async (offset: number) => {
      if (!hostId) return false;
      if (!isTauri()) {
        setLoading(false);
        return true;
      }
      const seq = ++loadSeq.current;
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
        if (seq !== loadSeq.current) return false;
        const withThumbs = page.items.map((item) => ({
          ...item,
          thumbnailUrl: item.thumbnailUrl ?? thumbsRef.current[item.id] ?? null,
        }));
        setItems((prev) => {
          if (offset === 0) return withThumbs;
          // 「+N」可能已经提前补进了这一页的片段，按 id 去重，避免卡片重复。
          const byId = new Map(prev.map((item) => [item.id, item]));
          for (const item of withThumbs) byId.set(item.id, item);
          return Array.from(byId.values());
        });
        setLoadedRows((prev) => (offset > 0 ? prev + page.items.length : page.items.length));
        setTotal(page.total);
        return true;
      } catch (err) {
        notify.error(err);
        return false;
      } finally {
        if (seq === loadSeq.current) {
          setLoading(false);
          setLoadingMore(false);
        }
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
    if (!hostId) return;
    void loadFiles(0);
  }, [loadFiles, hostId]);

  // 启动后的崩溃恢复合并在后台跑完时会改文件路径/大小，重新拉一遍列表（I2）。
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let dispose: (() => void) | undefined;
    void onMediaChanged(() => {
      if (disposed) return;
      void loadFiles(0);
    })
      .then((off) => {
        // 订阅应答可能晚于卸载：此时清理函数还没拿到 off，必须在这里补解绑。
        if (disposed) off();
        else dispose = off;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      dispose?.();
    };
  }, [hostId, loadFiles]);

  // 生成好的九宫格封面按 id 替换，不整页重载。
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let dispose: (() => void) | undefined;
    void onMediaThumb((thumb) => {
      if (disposed) return;
      thumbsRef.current[thumb.sessionId] = thumb.url;
      setItems((prev) =>
        prev.map((item) => (item.id === thumb.sessionId ? { ...item, thumbnailUrl: thumb.url } : item)),
      );
    })
      .then((off) => {
        if (disposed) off();
        else dispose = off;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      dispose?.();
    };
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let dispose: (() => void) | undefined;
    const timers = new Set<number>();
    void onPostProgress((one) => {
      if (disposed) return;
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
          void loadFiles(0);
        }, 500);
        timers.add(timer);
      }
    })
      .then((off) => {
        if (disposed) off();
        else dispose = off;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      dispose?.();
      for (const timer of timers) window.clearTimeout(timer);
    };
  }, [hostId, loadFiles]);

  // 离开房间录制文件页时把回放停掉：缓存目录要清，不能留 ffmpeg 在后台。
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      desiredPlayRef.current = null;
      playSeqRef.current += 1;
      const current = playingRef.current;
      if (current) void mediaStop(current.item.id).catch(() => undefined);
    };
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

  const stopPlayback = async (id: string) => {
    await mediaStop(id).catch(() => undefined);
  };

  const closePlay = async () => {
    const current = playing;
    playSeqRef.current += 1;
    desiredPlayRef.current = null;
    setPlaying(null);
    setConfirmSourceDelete(false);
    if (current) await stopPlayback(current.item.id);
  };

  // 迟到/失效的回放结果只允许停自己，不能覆盖后一次点击打开的播放器。
  const acceptPlayback = (
    item: MediaItem,
    source: boolean,
    seq: number,
  ): boolean => {
    const desired = desiredPlayRef.current;
    const stale =
      !mountedRef.current ||
      seq !== playSeqRef.current ||
      desired?.id !== item.id ||
      desired?.source !== source;
    if (!stale) return true;
    if (!mountedRef.current || !desired || desired.id !== item.id) {
      void mediaStop(item.id).catch(() => undefined);
    }
    return false;
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
    const seq = ++playSeqRef.current;
    desiredPlayRef.current = { id: item.id, source };
    try {
      const playback = await mediaPlay(item.id, null, source);
      if (!acceptPlayback(item, source, seq)) return;
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
    const seq = ++playSeqRef.current;
    desiredPlayRef.current = { id: item.id, source };
    try {
      await stopPlayback(item.id);
      const playback = await mediaPlay(item.id, null, source);
      if (!acceptPlayback(item, source, seq)) return;
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
      const seq = ++playSeqRef.current;
      desiredPlayRef.current = { id: item.id, source };
      const playback = await mediaPlay(item.id, positionMs, source);
      if (!acceptPlayback(item, source, seq)) return;
      setPlaying({ item, playback, source });
    } catch (err) {
      notify.error(err, t("media.playFailed"));
    }
  };

  // Esc 先关回放抽屉，其次退回监控列表。
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (playingRef.current) {
        playSeqRef.current += 1;
        desiredPlayRef.current = null;
        void mediaStop(playingRef.current.item.id).catch(() => undefined);
        setPlaying(null);
        setConfirmSourceDelete(false);
        return;
      }
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hostId]);

  const refreshAll = async () => {
    return loadFiles(0);
  };

  const refreshView = async () => {
    if (await refreshAll()) notify.success(t("media.refreshed"));
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
      // 整场删除是逐段进行的：某一段失败时前面可能已经删掉，重拉一次避免列表残留。
      await refreshAll();
    } finally {
      setBusy(null);
    }
  };

  // 单场多段合并：把这一场的片段交给后端合并成一个文件，不再需要底部批量条。
  const mergeGroup = async (group: MediaGroup) => {
    if (busy) return;
    setBusy("merge:" + group.id);
    try {
      // 列表按片段分页，一场可能只加载了一部分；先补齐整组再合并，
      // 否则只合并当前页的几段，成品会缺中间片段（文档 196-06）。
      const clips =
        group.clips.length < group.clipCount
          ? await mediaGroupClips(group.id)
          : group.clips;
      const ordered = [...clips].sort(
        (a, b) => a.clipIndex - b.clipIndex || a.startedAt - b.startedAt,
      );
      if (ordered.length < 2) {
        notify.error(t("media.mergeFailed"));
        return;
      }
      const result = await sessionsMerge(ordered.map((clip) => clip.id));
      notify.success(t("media.mergeDone", { size: formatSize(result.size) }));
      await refreshAll();
    } catch (err) {
      notify.error(err, t("media.mergeFailed"));
    } finally {
      setBusy(null);
    }
  };

  const fileFiltersActive = keyword !== "" || range !== 0 || onlyDanmaku;

  const rangeItems: Array<{ key: Range; label: string }> = [
    { key: 0, label: t("media.rangeAll") },
    { key: 1, label: t("media.rangeToday") },
    { key: 7, label: t("media.range7") },
    { key: 30, label: t("media.range30") },
  ];

  const statusLine = busy ? t("common.loading") : null;

  return (
    <PageShell>
      <PageHeader
        leading={
          <button
            type="button"
            onClick={onClose}
            className="focus-ring -ml-1 inline-flex w-fit items-center gap-1 rounded-[4px] px-1 py-0.5 text-[11.5px] text-muted transition-colors hover:text-ink"
          >
            <IconBack />
            {t("media.backToMonitor")}
          </button>
        }
        prefix={
          room.avatarUrl ? (
            <img
              src={room.avatarUrl}
              alt=""
              draggable={false}
              className="h-9 w-9 rounded-[10px] border border-line object-cover"
            />
          ) : (
            <span
              className="grid h-9 w-9 place-items-center rounded-[10px] border border-line text-[13px] font-semibold text-white/90"
              style={{ background: PLATFORM_TINT[room.platform] ?? "#3a3f47" }}
            >
              {(room.hostName || room.roomKey).slice(0, 1).toUpperCase()}
            </span>
          )
        }
        title={room.hostName || room.roomKey}
        subtitle={
          <span className="flex flex-wrap items-center gap-2">
            <PlatformChip platform={room.platform} />
            <span className="truncate">{room.title || room.canonicalUrl}</span>
            <span className="text-line">·</span>
            <span className="font-mono tabular-nums">{room.roomKey}</span>
          </span>
        }
        actions={
          <Button size="lg" variant="secondary" disabled={loading} onClick={() => void refreshView()}>
            {t("common.refresh")}
          </Button>
        }
      />

      <PageToolbar>
        <PageSearch
          value={query}
          onChange={setQuery}
          placeholder={t("media.roomSearch")}
          clearLabel={t("common.clear")}
        />
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
      {statusLine ? (
        <div className="mx-5 mb-2 flex items-center justify-between rounded-ctl border border-line bg-raised px-3 py-2 text-[11.5px] text-ink-soft">
          <span className="truncate">{statusLine}</span>
        </div>
      ) : null}

      <PageBody className="pb-5">
      {loading ? (
          <div className="flex flex-1 items-center justify-center text-[12px] text-faint">{t("common.loading")}</div>
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
              // 分段场次里首片段的缩略图可能还没生成；封面取第一个有图的片段，避免整组一直显示占位。
              const cover = group.clips.find((clip) => clip.thumbnailUrl)?.thumbnailUrl ?? item.thumbnailUrl;
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
                    "border-line hover:border-line-strong",
                  )}
                >
                  <div className="relative aspect-video w-full overflow-hidden bg-raised">
                    {cover ? (
                      <img
                        src={cover}
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
                    {group.clipCount > 1 ? (
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={!!busy}
                        onClick={() => void mergeGroup(group)}
                        className="mt-1 w-full justify-center"
                      >
                        {t("media.mergeThisSession", { n: group.clipCount })}
                      </Button>
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
      }

      {!loading && items.length > 0 && loadedRows < total ? (
        <div className="flex justify-center pb-2">
          <Button size="sm" variant="secondary" disabled={loadingMore} onClick={() => void loadFiles(loadedRows)}>
            {loadingMore ? t("common.loading") : t("media.loadMore")}
          </Button>
        </div>
      ) : null}

      </PageBody>

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
                        onPositionChange={setPlayPosition}
                        danmakuEnabled={
                          danmakuOn &&
                          ((playing.item.danmakuCount ?? 0) > 0 || (danmakuTrack?.length ?? 0) > 0)
                        }
                        onToggleDanmaku={
                          (playing.item.danmakuCount ?? 0) > 0 || (danmakuTrack?.length ?? 0) > 0
                            ? () => setDanmakuOn((value) => !value)
                            : undefined
                        }
                        overlay={
                          danmakuTrack && danmakuTrack.length > 0 ? (
                            <div className="pointer-events-none absolute inset-0">
                              <DanmakuCanvas
                                enabled={danmakuOn}
                                messages={danmakuTrack}
                                positionMs={playPosition}
                              />
                            </div>
                          ) : null
                        }
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

                    {playing.item.danmakuPath ? (
                      <div className="mt-4 flex items-center gap-2 rounded-[7px] border border-line bg-raised/40 px-3 py-2">
                        <div className="text-[10px] uppercase tracking-[0.08em] text-faint">
                          {t("media.danmakuTrack")}
                        </div>
                        <button
                          type="button"
                          onClick={() =>
                            void mediaRevealFile(playing.item.id, playing.item.danmakuPath!).catch(() => undefined)
                          }
                          title={playing.item.danmakuPath}
                          className="focus-ring min-w-0 truncate font-mono text-[10.5px] text-accent-text hover:underline"
                        >
                          {baseName(playing.item.danmakuPath)}
                        </button>
                        <span className="ml-auto shrink-0 font-sans text-[10px] tabular-nums text-faint">
                          {danmakuLoading
                            ? t("media.danmakuLoading")
                            : (playing.item.danmakuCount ?? 0) > 0
                              ? t("media.danmakuTrackCount", { n: playing.item.danmakuCount })
                              : danmakuTrack && danmakuTrack.length > 0
                                ? t("media.danmakuTrackCount", { n: danmakuTrack.length })
                                : t("media.danmakuEmpty")}
                        </span>
                      </div>
                    ) : null}

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
