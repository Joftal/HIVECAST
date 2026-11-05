import Hls from "hls.js";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

type Props = {
  url: string;
  onFatal?: (detail: string) => void;
};

type Stats = {
  width: number;
  height: number;
  kbps: number;
  dropped: number;
  buffer: number;
};

const EMPTY_STATS: Stats = { width: 0, height: 0, kbps: 0, dropped: 0, buffer: 0 };

export default function Player({ url, onFatal }: Props) {
  const { t } = useTranslation();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const shellRef = useRef<HTMLDivElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const [playing, setPlaying] = useState(false);
  const [volume, setVolume] = useState(0);
  const [stats, setStats] = useState<Stats>(EMPTY_STATS);

  // 每个播放器实例独立持有自己的 hls 实例：这是「不打架」的最小单元。
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    setPlaying(false);
    setStats(EMPTY_STATS);

    if (Hls.isSupported()) {
      const hls = new Hls({
        lowLatencyMode: true,
        backBufferLength: 30,
        liveSyncDurationCount: 2,
        maxBufferLength: 8,
      });
      hlsRef.current = hls;
      hls.attachMedia(video);
      hls.loadSource(url);
      hls.on(Hls.Events.ERROR, (_event, data) => {
        if (data.fatal && onFatal) onFatal(data.details);
      });
      return () => {
        hls.destroy();
        hlsRef.current = null;
      };
    }

    // Safari 走原生 HLS。
    video.src = url;
    return () => {
      video.removeAttribute("src");
      video.load();
    };
  }, [url, onFatal]);

  // 统计信息每秒刷新一次，不参与 React 的高频路径。
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    const timer = window.setInterval(() => {
      const quality = video.getVideoPlaybackQuality();
      const level = hlsRef.current?.levels[hlsRef.current.currentLevel];
      setStats({
        width: video.videoWidth,
        height: video.videoHeight,
        kbps: Math.round((level?.bitrate ?? 0) / 1000),
        dropped: quality?.droppedVideoFrames ?? 0,
        buffer: Math.max(0, Math.round((video.buffered.length ? video.buffered.end(video.buffered.length - 1) - video.currentTime : 0) * 10) / 10),
      });
    }, 1000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const video = videoRef.current;
    if (video) video.volume = volume;
  }, [volume]);

  const toggle = () => {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) void video.play().catch(() => setPlaying(false));
    else video.pause();
  };

  const fullscreen = () => {
    const shell = shellRef.current;
    if (!shell) return;
    if (document.fullscreenElement) void document.exitFullscreen();
    else void shell.requestFullscreen();
  };

  return (
    <div ref={shellRef} className="group relative h-full w-full overflow-hidden rounded-xl bg-black">
      <video
        ref={videoRef}
        muted={volume === 0}
        playsInline
        className="h-full w-full object-contain"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
      />

      <div className="pointer-events-none absolute right-2 top-2 rounded-md bg-black/55 px-2 py-1 font-mono text-[10px] leading-4 text-zinc-300 opacity-70">
        {stats.width > 0 ? stats.width + "x" + stats.height : "--"}
        {stats.kbps > 0 ? " · " + stats.kbps + " kbps" : ""}
        {stats.dropped > 0 ? " · " + t("watch.stats.dropped") + " " + stats.dropped : ""}
      </div>

      <div className="absolute inset-x-0 bottom-0 flex items-center gap-3 bg-gradient-to-t from-black/85 via-black/40 to-transparent px-3 pb-2 pt-8 opacity-0 transition-opacity duration-200 group-hover:opacity-100 focus-within:opacity-100">
        <button
          type="button"
          onClick={toggle}
          aria-label={playing ? t("watch.paused") : t("watch.playing")}
          className="focus-ring flex h-7 w-7 items-center justify-center rounded-full bg-white/10 text-zinc-100 hover:bg-white/20"
        >
          {playing ? (
            <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
              <rect x="2" y="1.5" width="3" height="9" rx="0.8" />
              <rect x="7" y="1.5" width="3" height="9" rx="0.8" />
            </svg>
          ) : (
            <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
              <path d="M3 1.6v8.8a.6.6 0 0 0 .92.5l6.2-4.4a.6.6 0 0 0 0-1L3.92 1.1A.6.6 0 0 0 3 1.6Z" />
            </svg>
          )}
        </button>

        <div className="flex items-center gap-1.5">
          <input
            type="range"
            min={0}
            max={100}
            value={Math.round(volume * 100)}
            onChange={(event) => setVolume(Number(event.target.value) / 100)}
            aria-label={t("watch.audioOn")}
            className="h-1 w-16 cursor-pointer accent-hive-500"
          />
          <span className="w-8 font-mono text-[10px] text-zinc-400">
            {volume === 0 ? t("watch.muted") : Math.round(volume * 100)}
          </span>
        </div>

        <div className="ml-auto flex items-center gap-1">
          <button
            type="button"
            disabled
            title={t("watch.qualityLocked")}
            className="cursor-not-allowed rounded-md px-1.5 py-1 text-[10px] text-zinc-500"
          >
            AUTO
          </button>
          <button
            type="button"
            disabled
            title={t("watch.danmakuLocked")}
            className="cursor-not-allowed rounded-md px-1.5 py-1 text-[10px] text-zinc-500"
          >
            弹
          </button>
          <button
            type="button"
            onClick={fullscreen}
            aria-label="fullscreen"
            className="focus-ring rounded-md px-1.5 py-1 text-[10px] text-zinc-300 hover:bg-white/10"
          >
            ⤢
          </button>
        </div>
      </div>
    </div>
  );
}
