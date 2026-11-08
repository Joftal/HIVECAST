import Hls from "hls.js";
import { Check } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { DropdownMenu, type DropdownItem } from "../../components/arc/dropdown-menu/dropdown-menu";

type Props = {
  url: string;
  /** 0..1；0 表示静音。音量由外层持有，方便「一键全部静音」。 */
  volume: number;
  onVolumeChange: (volume: number) => void;
  onFatal?: (detail: string) => void;
  /** 叠在画面上的弹幕层（由外部注入，播放器本身不关心实现）。 */
  overlay?: ReactNode;
  danmakuEnabled?: boolean;
  onToggleDanmaku?: () => void;
  /** 整段时长（毫秒）。给了才显示进度条：直播没有总时长，不给拖动。 */
  durationMs?: number;
  /** 当前窗口从整段的第几毫秒开始（滑窗回放）。 */
  startMs?: number;
  /** 窗口起点比播放点提前了多少毫秒，续窗时用来衔接。 */
  leadMs?: number;
  /** 新窗口应该从流内第几秒开始播（不传就从头播）。 */
  startAtSec?: number;
  /** 请求跳到整段的某个位置；落在窗口外由外层重开一个窗口。 */
  onSeek?: (positionMs: number) => void;
  /** 状态角标位置。观看页顶部有槽位信息条，角标要挪到左下角，否则会叠字。 */
  statsCorner?: "top-right" | "bottom-left";
  /** 画质菜单（直播才有）。给了就在控制条右侧显示画质入口。 */
  quality?: {
    value: number;
    options: { value: number; label: string }[];
    actual?: string;
    onChange: (value: number) => void;
  };
  /** 线路菜单：与画质放在同一个浮层里，先选线路再选档位。 */
  lines?: {
    value: string | null;
    options: { value: string | null; label: string; hint?: string }[];
    onChange: (value: string | null) => void;
  };
  /** 重新解析并重连当前流；不传就只重建播放器。 */
  onReload?: () => void;
  /** 打开弹幕观感设置（外层抽屉）。 */
  onOpenDanmakuSettings?: () => void;
  /** 画质浮层展开时回调：外层借此按需拉线路列表。 */
  onMenuOpen?: () => void;
  /** 画面适配：contain 保留完整画面（默认），cover 铺满格子但会裁切。 */
  fit?: "contain" | "cover";
};

type Stats = {
  width: number;
  height: number;
  kbps: number;
  dropped: number;
};

const EMPTY_STATS: Stats = { width: 0, height: 0, kbps: 0, dropped: 0 };

export default function Player({
  url,
  volume,
  onVolumeChange,
  onFatal,
  overlay,
  danmakuEnabled = true,
  onToggleDanmaku,
  durationMs,
  startMs = 0,
  leadMs = 0,
  startAtSec,
  onSeek,
  statsCorner = "top-right",
  quality,
  lines,
  onReload,
  onOpenDanmakuSettings,
  onMenuOpen,
  fit = "contain",
}: Props) {
  const { t } = useTranslation();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const shellRef = useRef<HTMLDivElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  // 卡死时的自救次数：先整体重载播放器，仍不行才交给用户点重试。
  const reloadsRef = useRef(0);
  const [reloadKey, setReloadKey] = useState(0);
  // 回调放在 ref 里：外层每次渲染都会传新函数，不能让它触发 hls 重建。
  const fatalRef = useRef(onFatal);
  fatalRef.current = onFatal;

  const [playing, setPlaying] = useState(false);
  const [stats, setStats] = useState<Stats>(EMPTY_STATS);
  // 视频按 object-contain 居中，黑边不属于画面；弹幕要贴着画面走，不能画在黑边上。
  const [box, setBox] = useState<{ left: number; top: number; width: number; height: number } | null>(null);
  // 点播（回放）才有进度条；拖动时先用 scrubMs 显示预览位置，松手再决定要不要换窗口。
  const vod = durationMs !== undefined && durationMs > 0;
  const [positionMs, setPositionMs] = useState(startMs);
  const [scrubMs, setScrubMs] = useState<number | null>(null);
  // 控制条：音量滑杆展开、观看时长（对齐 B 站直播控制条的信息位）。
  const [volOpen, setVolOpen] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const startAtRef = useRef(startAtSec ?? 0);
  startAtRef.current = startAtSec ?? 0;

  // 量出画面在容器里的实际矩形（含 16:9 黑边），供弹幕层与进度条对齐。
  useEffect(() => {
    const shell = shellRef.current;
    const video = videoRef.current;
    if (!shell || !video) return;
    const measure = () => {
      const cw = shell.clientWidth;
      const ch = shell.clientHeight;
      if (!cw || !ch) return;
      // 铺满模式没有黑边，弹幕直接铺满整个格子。
      if (fit === "cover") {
        setBox({ left: 0, top: 0, width: cw, height: ch });
        return;
      }
      const vw = video.videoWidth;
      const vh = video.videoHeight;
      if (!vw || !vh) return;
      const scale = Math.min(cw / vw, ch / vh);
      const width = Math.round(vw * scale);
      const height = Math.round(vh * scale);
      setBox({ left: Math.round((cw - width) / 2), top: Math.round((ch - height) / 2), width, height });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(shell);
    video.addEventListener("loadedmetadata", measure);
    video.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      video.removeEventListener("loadedmetadata", measure);
      video.removeEventListener("resize", measure);
    };
  }, [url, reloadKey, fit]);

  const volumeRef = useRef(volume);
  volumeRef.current = volume;
  // 起播瞬间先静音：未静音的 play() 会被浏览器自动播放策略拒绝，
  // 表现就是"一直缓冲"。起播成功后再把音量恢复回去。
  const startingRef = useRef(false);

  const startPlayback = (video: HTMLVideoElement) => {
    startingRef.current = true;
    video.muted = true;
    void video
      .play()
      .then(() => {
        startingRef.current = false;
        video.muted = volumeRef.current === 0;
        video.volume = volumeRef.current;
      })
      .catch(() => {
        startingRef.current = false;
        setPlaying(false);
      });
  };

  // 换流（切画质/换房间/换窗口）时重置自救预算和进度显示。
  useEffect(() => {
    reloadsRef.current = 0;
    setScrubMs(null);
    setPositionMs(startMs);
  }, [url, startMs]);

  // 每个播放器实例独立持有自己的 hls 实例：这是「互不打架」的最小单元。
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    setPlaying(false);
    setStats(EMPTY_STATS);

    if (Hls.isSupported()) {
      const hls = new Hls({
        // 我们产出的不是 LL-HLS（没有 EXT-X-PART），开低延迟模式只会让 hls.js
        // 一直去蹭直播边缘，切画质后容易卡在分片边界上。
        lowLatencyMode: false,
        backBufferLength: 30,
        liveSyncDurationCount: 3,
        maxBufferLength: 12,
        // 点播窗口从窗口内的指定位置起播；直播保持 -1（贴直播边缘）。
        startPosition: startAtRef.current > 0 ? startAtRef.current : -1,
      });
      let recovered = 0;
      hlsRef.current = hls;
      hls.attachMedia(video);
      hls.loadSource(url);
      hls.on(Hls.Events.MANIFEST_PARSED, () => {
        startPlayback(video);
      });
      hls.on(Hls.Events.ERROR, (_event, data) => {
        if (!data.fatal) return;
        // 媒体/网络类的致命错误先就地恢复，别一有抖动就把整格判死。
        if (recovered < 3) {
          recovered += 1;
          if (data.type === Hls.ErrorTypes.MEDIA_ERROR) hls.recoverMediaError();
          else if (data.type === Hls.ErrorTypes.NETWORK_ERROR) hls.startLoad();
          else fatalRef.current?.(data.details);
          return;
        }
        fatalRef.current?.(data.details);
      });
      // 卡死看门狗：直播流偶尔会停在分片边界，进度不动就拉回直播边缘，连续卡就报错让用户重试。
      let lastTime = -1;
      let stalls = 0;
      const wd = window.setInterval(() => {
        if (video.paused || video.seeking || video.readyState < 2) return;
        if (video.currentTime !== lastTime) {
          lastTime = video.currentTime;
          stalls = 0;
          return;
        }
        stalls += 1;
        // 点播没有「直播边缘」可回，卡住只能重载分片。
        const live = vod ? null : hls.liveSyncPosition;
        if (stalls === 2 || stalls === 4) {
          hls.startLoad();
          if (live !== null && Math.abs(live - video.currentTime) > 2) video.currentTime = live;
        } else if (stalls >= 6) {
          if (reloadsRef.current < 2) {
            reloadsRef.current += 1;
            setReloadKey((value) => value + 1);
          } else {
            fatalRef.current?.("stalled");
          }
        }
      }, 2000);
      return () => {
        window.clearInterval(wd);
        hls.destroy();
        hlsRef.current = null;
      };
    }

    // Safari 走原生 HLS。
    video.src = url;
    const onLoaded = () => startPlayback(video);
    video.addEventListener("loadedmetadata", onLoaded);
    return () => {
      video.removeEventListener("loadedmetadata", onLoaded);
      video.removeAttribute("src");
      video.load();
    };
  }, [url, reloadKey, vod]);

  // 统计信息每秒刷新一次，不参与 React 的高频路径。
  useEffect(() => {
    const timer = window.setInterval(() => {
      const video = videoRef.current;
      if (!video) return;
      const quality = video.getVideoPlaybackQuality();
      const level = hlsRef.current?.levels[hlsRef.current.currentLevel];
      setStats({
        width: video.videoWidth,
        height: video.videoHeight,
        kbps: Math.round((level?.bitrate ?? 0) / 1000),
        dropped: quality?.droppedVideoFrames ?? 0,
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const video = videoRef.current;
    if (!video || startingRef.current) return;
    video.muted = volume === 0;
    video.volume = volume;
  }, [volume]);

  const clock = (value: number) => {
    const total = Math.max(0, Math.round(value / 1000));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const sec = total % 60;
    const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
    return (h > 0 ? h + ":" : "") + mm + ":" + String(sec).padStart(2, "0");
  };

  const withinWindow = (targetMs: number) => {
    const video = videoRef.current;
    if (!video) return false;
    const local = (targetMs - startMs) / 1000;
    const total = Number.isFinite(video.duration) ? video.duration : 0;
    return local >= 0 && (total <= 0 || local <= total);
  };

  const scrub = (targetMs: number) => {
    setScrubMs(targetMs);
    const video = videoRef.current;
    // 窗口内的位置直接就地拖动，不用惊动后端。
    if (video && withinWindow(targetMs)) video.currentTime = Math.max(0, (targetMs - startMs) / 1000);
  };

  const commitScrub = () => {
    const target = scrubMs;
    setScrubMs(null);
    if (target !== null && onSeek && !withinWindow(target)) onSeek(target);
  };

  const toggle = () => {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) void video.play().catch(() => setPlaying(false));
    else video.pause();
  };

  const toggleMute = () => {
    onVolumeChange(volume > 0 ? 0 : 0.8);
  };

  const fullscreen = () => {
    const shell = shellRef.current;
    if (!shell) return;
    if (document.fullscreenElement) void document.exitFullscreen();
    else void shell.requestFullscreen();
  };

  const muted = volume === 0;
  const pipSupported = typeof document !== "undefined" && document.pictureInPictureEnabled !== false;

  useEffect(() => {
    setElapsed(0);
    const timer = window.setInterval(() => setElapsed((value) => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [url, reloadKey]);

  const reload = () => {
    if (onReload) onReload();
    else setReloadKey((value) => value + 1);
  };

  const togglePip = () => {
    const video = videoRef.current;
    if (!video || !pipSupported) return;
    if (document.pictureInPictureElement) void document.exitPictureInPicture();
    else void video.requestPictureInPicture().catch(() => undefined);
  };

  const qualityLabel = quality?.options.find((option) => option.value === quality.value)?.label;
  const playbackPct = durationMs && durationMs > 0 ? Math.min(100, ((scrubMs ?? positionMs) / durationMs) * 100) : 0;

  // 线路与画质合成一个菜单：线路在前，画质在后，自动档单独放最后。
  const qualityMenu: DropdownItem[] = [];
  const hasLines = Boolean(lines && lines.options.length > 0);
  if (lines && hasLines) {
    for (const option of lines.options) {
      qualityMenu.push({
        key: "line:" + (option.value ?? "auto"),
        label: `${t("watch.line")} · ${option.label}`,
        icon: (lines.value ?? null) === option.value ? <Check size={14} /> : <span />,
        onSelect: () => lines.onChange(option.value),
      });
    }
  }
  let firstQuality = true;
  if (quality) {
    for (const option of quality.options) {
      if (option.value === 0) continue;
      qualityMenu.push({
        key: "qn:" + option.value,
        label: option.label,
        separatorBefore: firstQuality && hasLines,
        icon: quality.value === option.value ? <Check size={14} /> : <span />,
        onSelect: () => quality.onChange(option.value),
      });
      firstQuality = false;
    }
    qualityMenu.push({
      key: "qn:auto",
      label: quality.actual ? t("watch.autoQuality", { q: quality.actual }) : t("watch.qn.auto"),
      separatorBefore: !firstQuality,
      icon: quality.value === 0 ? <Check size={14} /> : <span />,
      onSelect: () => quality.onChange(0),
    });
  }

  return (
    <div ref={shellRef} className="group relative h-full w-full overflow-hidden bg-black">
      <video
        ref={videoRef}
        muted={muted}
        playsInline
        className={fit === "cover" ? "h-full w-full object-cover" : "h-full w-full object-contain"}
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onTimeUpdate={(event) => setPositionMs(startMs + event.currentTarget.currentTime * 1000)}
        onEnded={() => {
          if (!vod || !onSeek || durationMs === undefined) return;
          const video = videoRef.current;
          const end = startMs + (video?.duration ?? 0) * 1000;
          // 窗口放完自动续下一窗：多带一个 lead 的提前量，衔接处不重复。
          if (end < durationMs - 800) onSeek(Math.min(durationMs, end + leadMs));
        }}
        onClick={toggle}
      />

      {/* 弹幕层在视频之上、控件之下：不拦截点击，全屏时也保留；只覆盖画面本身。 */}
      {overlay ? (
        <div
          className="pointer-events-none absolute z-[5]"
          style={box ?? { left: 0, top: 0, width: "100%", height: "100%" }}
        >
          {overlay}
        </div>
      ) : null}

      {stats.width > 0 ? (
        <div
          className={[
            "pointer-events-none absolute z-10 rounded-[5px] bg-black/40 px-1.5 py-0.5 font-mono text-[9.5px] leading-4 text-white/55 backdrop-blur-[2px]",
            statsCorner === "bottom-left" ? "bottom-11 left-2" : "right-2 top-2",
          ].join(" ")}
        >
          {stats.width + "x" + stats.height}
          {stats.kbps > 0 ? " · " + stats.kbps + " kbps" : ""}
          {stats.dropped > 0 ? " · " + t("watch.stats.dropped") + " " + stats.dropped : ""}
        </div>
      ) : (
        <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/30">
          <span className="h-5 w-5 animate-spin rounded-full border-2 border-white/20 border-t-accent" />
          <span className="text-[11px] text-white/55">{t("watch.buffering")}</span>
        </div>
      )}

      {/* 控制条：结构对齐 B 站直播——细进度线在上、单行图标在下，悬停才浮出。 */}
      <div className="absolute inset-x-0 bottom-0 z-20 select-none opacity-0 transition-opacity duration-200 group-hover:opacity-100 focus-within:opacity-100">
        <div className="bg-gradient-to-t from-black/85 via-black/55 to-transparent pb-0.5 pt-10">
          {vod ? (
            <div className="relative mx-2.5 flex h-4 items-center">
              <div className="absolute inset-x-0 h-[3px] overflow-hidden rounded-full bg-white/20">
                <div className="h-full rounded-full bg-accent transition-[width] duration-150" style={{ width: playbackPct + "%" }} />
              </div>
              <input
                type="range"
                min={0}
                max={durationMs ?? 0}
                step={1000}
                value={Math.round(Math.min(scrubMs ?? positionMs, durationMs ?? 0))}
                onChange={(event) => scrub(Number(event.target.value))}
                onPointerUp={commitScrub}
                onKeyUp={commitScrub}
                aria-label={t("media.progress")}
                className="absolute inset-x-0 h-4 w-full cursor-pointer appearance-none bg-transparent opacity-0"
              />
            </div>
          ) : (
            <div className="mx-2.5 flex h-4 items-center">
              <div className="relative h-[3px] w-full overflow-hidden rounded-full bg-white/[0.14]">
                <div className="absolute inset-y-0 right-0 w-1/4 bg-gradient-to-l from-accent/80 via-accent/30 to-transparent" />
              </div>
            </div>
          )}

          <div className="flex h-9 items-center gap-0.5 px-1.5 pb-1">
            <button
              type="button"
              onClick={toggle}
              aria-label={playing ? t("watch.paused") : t("watch.playing")}
              className="focus-ring flex h-8 w-8 shrink-0 items-center justify-center rounded-[7px] text-white/85 transition-colors hover:bg-white/15 hover:text-white"
            >
              {playing ? (
                <svg width="11" height="11" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
                  <rect x="2" y="1.5" width="3" height="9" rx="0.8" />
                  <rect x="7" y="1.5" width="3" height="9" rx="0.8" />
                </svg>
              ) : (
                <svg width="11" height="11" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
                  <path d="M3 1.6v8.8a.6.6 0 0 0 .92.5l6.2-4.4a.6.6 0 0 0 0-1L3.92 1.1A.6.6 0 0 0 3 1.6Z" />
                </svg>
              )}
            </button>

            <button
              type="button"
              onClick={reload}
              aria-label={t("watch.reload")}
              title={t("watch.reload")}
              className="focus-ring flex h-8 w-8 shrink-0 items-center justify-center rounded-[7px] text-white/70 transition-colors hover:bg-white/15 hover:text-white"
            >
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                <path d="M13.2 6.4A5.4 5.4 0 1 0 13 9.6" strokeLinecap="round" />
                <path d="M13.4 2.9v3.7H9.7" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </button>

            <div
              className="flex shrink-0 items-center"
              onMouseEnter={() => setVolOpen(true)}
              onMouseLeave={() => setVolOpen(false)}
            >
              <button
                type="button"
                onClick={toggleMute}
                aria-label={muted ? t("watch.muted") : t("watch.audioOn")}
                className={[
                  "focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-[6px] transition-colors hover:bg-white/15",
                  muted ? "text-white/55" : "text-white/85 hover:text-white",
                ].join(" ")}
              >
                {muted ? (
                  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                    <path d="M8.6 2.9 4.9 5.7H2.4v4.6h2.5l3.7 2.8V2.9Z" strokeLinejoin="round" />
                    <path d="M11.4 6.2 14 8.8M14 6.2l-2.6 2.6" strokeLinecap="round" />
                  </svg>
                ) : (
                  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                    <path d="M8.6 2.9 4.9 5.7H2.4v4.6h2.5l3.7 2.8V2.9Z" strokeLinejoin="round" />
                    <path d="M11.2 5.4a3.7 3.7 0 0 1 0 5.2M13.2 3.6a6.2 6.2 0 0 1 0 8.8" strokeLinecap="round" />
                  </svg>
                )}
              </button>
              <input
                type="range"
                min={0}
                max={100}
                value={Math.round(volume * 100)}
                onChange={(event) => onVolumeChange(Number(event.target.value) / 100)}
                aria-label={t("watch.audioOn")}
                className={[
                  "h-1 cursor-pointer accent-accent transition-all duration-200",
                  volOpen ? "w-16 opacity-100" : "w-0 opacity-0",
                ].join(" ")}
              />
            </div>

            <span className="ml-1.5 shrink-0 font-mono text-[10.5px] tabular-nums text-white/65">
              {vod
                ? clock(scrubMs ?? positionMs) + " / " + clock(durationMs ?? 0)
                : clock(elapsed * 1000)}
            </span>

            <div className="ml-auto flex items-center gap-0.5">
              {quality ? (
                <DropdownMenu
                  variant="dark"
                  align="end"
                  label={qualityLabel ?? t("watch.qn.auto")}
                  ariaLabel={t("watch.quality")}
                  items={qualityMenu}
                  morphLabel={false}
                  className="max-w-[110px]"
                  onOpenChange={(open) => {
                    if (open) onMenuOpen?.();
                  }}
                />
              ) : null}

              <button
                type="button"
                onClick={togglePip}
                disabled={!pipSupported}
                aria-label={t("watch.pip")}
                title={t("watch.pip")}
                className="focus-ring flex h-7 w-7 items-center justify-center rounded-[6px] text-white/70 transition-colors hover:bg-white/15 hover:text-white disabled:cursor-not-allowed disabled:opacity-35"
              >
                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
                  <rect x="1.8" y="2.6" width="12.4" height="10.8" rx="1.6" />
                  <rect x="8.2" y="8.2" width="5.2" height="4.4" rx="0.9" fill="currentColor" stroke="none" opacity="0.85" />
                </svg>
              </button>

              <button
                type="button"
                onClick={onToggleDanmaku}
                disabled={!onToggleDanmaku}
                title={danmakuEnabled ? t("watch.danmakuHide") : t("watch.danmakuShow")}
                aria-pressed={danmakuEnabled}
                className={[
                  "focus-ring flex h-7 items-center justify-center rounded-[6px] px-1.5 text-[10px] font-medium leading-none transition-colors",
                  danmakuEnabled
                    ? "bg-white/15 text-white hover:bg-white/25"
                    : "text-white/40 hover:bg-white/10 hover:text-white/70",
                  onToggleDanmaku ? "" : "cursor-not-allowed opacity-40",
                ].join(" ")}
              >
                {t("watch.danmakuShort")}
              </button>

              {onOpenDanmakuSettings ? (
                <button
                  type="button"
                  onClick={onOpenDanmakuSettings}
                  aria-label={t("watch.danmakuSettings")}
                  title={t("watch.danmakuSettings")}
                  className="focus-ring flex h-8 w-8 items-center justify-center rounded-[7px] text-white/70 transition-colors hover:bg-white/15 hover:text-white"
                >
                  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                    <path d="M2 4.6h12M2 8h12M2 11.4h12" strokeLinecap="round" opacity="0.55" />
                    <circle cx="5.6" cy="4.6" r="1.5" fill="currentColor" stroke="none" />
                    <circle cx="10.4" cy="8" r="1.5" fill="currentColor" stroke="none" />
                    <circle cx="7" cy="11.4" r="1.5" fill="currentColor" stroke="none" />
                  </svg>
                </button>
              ) : null}

              <button
                type="button"
                onClick={fullscreen}
                aria-label={t("watch.fullscreen")}
                title={t("watch.fullscreen")}
                className="focus-ring flex h-8 w-8 items-center justify-center rounded-[7px] text-white/70 transition-colors hover:bg-white/15 hover:text-white"
              >
                <svg width="14" height="14" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
                  <path d="M1.5 4.2V1.5h2.7M10.5 4.2V1.5H7.8M1.5 7.8v2.7h2.7M10.5 7.8v2.7H7.8" strokeLinecap="round" strokeLinejoin="round" />
                </svg>
              </button>
            </div>
          </div>
        </div>
      </div>

    </div>
  );
}