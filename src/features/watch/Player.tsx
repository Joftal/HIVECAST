import Hls from "hls.js";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

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
      const vw = video.videoWidth;
      const vh = video.videoHeight;
      if (!cw || !ch || !vw || !vh) return;
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
  }, [url, reloadKey]);

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

  return (
    <div ref={shellRef} className="group relative h-full w-full overflow-hidden bg-black">
      <video
        ref={videoRef}
        muted={muted}
        playsInline
        className="h-full w-full object-contain"
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

      {vod ? (
        <div className="absolute inset-x-0 bottom-9 z-10 flex items-center gap-2 bg-gradient-to-t from-black/70 to-transparent px-2.5 pb-1 pt-6 opacity-0 transition-opacity duration-200 group-hover:opacity-100 focus-within:opacity-100">
          <span className="w-10 text-right font-mono text-[10px] text-white/55">
            {clock(scrubMs ?? positionMs)}
          </span>
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
            className="h-1 flex-1 cursor-pointer accent-accent"
          />
          <span className="w-10 font-mono text-[10px] text-white/55">{clock(durationMs ?? 0)}</span>
        </div>
      ) : null}

      <div className="absolute inset-x-0 bottom-0 z-10 flex items-center gap-2 bg-gradient-to-t from-black/85 via-black/40 to-transparent px-2.5 pb-1.5 pt-9 opacity-0 transition-opacity duration-200 group-hover:opacity-100 focus-within:opacity-100">
        <button
          type="button"
          onClick={toggle}
          aria-label={playing ? t("watch.paused") : t("watch.playing")}
          className="focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-white/15 text-white backdrop-blur-[2px] transition-colors hover:bg-white/25"
        >
          {playing ? (
            <svg width="10" height="10" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
              <rect x="2" y="1.5" width="3" height="9" rx="0.8" />
              <rect x="7" y="1.5" width="3" height="9" rx="0.8" />
            </svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
              <path d="M3 1.6v8.8a.6.6 0 0 0 .92.5l6.2-4.4a.6.6 0 0 0 0-1L3.92 1.1A.6.6 0 0 0 3 1.6Z" />
            </svg>
          )}
        </button>

        <button
          type="button"
          onClick={toggleMute}
          aria-label={muted ? t("watch.muted") : t("watch.audioOn")}
          className={[
            "focus-ring flex h-6 w-6 shrink-0 items-center justify-center rounded-[6px] transition-colors hover:bg-white/12",
            muted ? "text-white/55" : "text-accent",
          ].join(" ")}
        >
          {muted ? (
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
              <path d="M8.6 2.9 4.9 5.7H2.4v4.6h2.5l3.7 2.8V2.9Z" strokeLinejoin="round" />
              <path d="M11.4 6.2 14 8.8M14 6.2l-2.6 2.6" strokeLinecap="round" />
            </svg>
          ) : (
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
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
          className="h-1 w-12 shrink-0 cursor-pointer accent-accent"
        />

        <div className="ml-auto flex items-center gap-1">
          <button
            type="button"
            onClick={onToggleDanmaku}
            disabled={!onToggleDanmaku}
            title={danmakuEnabled ? t("watch.danmakuHide") : t("watch.danmakuShow")}
            aria-pressed={danmakuEnabled}
            className={[
              "focus-ring rounded-[5px] px-1 py-0.5 text-[9.5px] leading-4 transition-colors",
              danmakuEnabled
                ? "bg-white/12 text-white/85 hover:bg-white/20"
                : "text-white/40 hover:bg-white/10 hover:text-white/70",
              onToggleDanmaku ? "" : "cursor-not-allowed opacity-40",
            ].join(" ")}
          >
            {t("watch.danmakuShort")}
          </button>
          <button
            type="button"
            onClick={fullscreen}
            aria-label={t("watch.fullscreen")}
            className="focus-ring flex h-6 w-6 items-center justify-center rounded-[6px] text-white/55 transition-colors hover:bg-white/12 hover:text-white"
          >
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden>
              <path d="M1.5 4.2V1.5h2.7M10.5 4.2V1.5H7.8M1.5 7.8v2.7h2.7M10.5 7.8v2.7H7.8" strokeLinecap="round" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}