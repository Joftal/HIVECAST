import Hls from "hls.js";
import { Check } from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { DropdownMenu, type DropdownItem } from "../../components/arc/dropdown-menu/dropdown-menu";
import { cx } from "../../components/ui/Controls";
import { useDanmakuPrefs } from "../../lib/danmakuPrefs";
import "./player-controls.css";

type Props = {
  url: string;
  /** 0..1；0 表示静音。音量由外层持有，每个格子独立。 */
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
  /** 画质浮层展开时回调：外层借此按需拉线路列表。 */
  onMenuOpen?: () => void;
  /** 画面适配：contain 保留完整画面（默认），cover 铺满格子但会裁切。 */
  fit?: "contain" | "cover";
  /** 切换画面填充/适应；不传就不显示对应按钮。 */
  onToggleFit?: () => void;
};

type Stats = {
  width: number;
  height: number;
  kbps: number;
  dropped: number;
};

const EMPTY_STATS: Stats = { width: 0, height: 0, kbps: 0, dropped: 0 };

type PlayerButtonProps = {
  children: ReactNode;
  label: string;
  title?: string;
  active?: boolean;
  disabled?: boolean;
  size?: "sm" | "md";
  tone?: "default" | "danger";
  onClick?: () => void;
  className?: string;
};

/** B 站式播放器图标按钮：30px 命中区、白色图标、浅色 hover 底。 */
export function PlayerButton({
  children,
  label,
  title,
  active,
  disabled,
  size = "sm",
  tone = "default",
  onClick,
  className,
}: PlayerButtonProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={title ?? label}
      aria-pressed={active}
      disabled={disabled}
      className={cx(
        "focus-ring pointer-events-auto relative flex shrink-0 items-center justify-center rounded-[6px] transition-colors duration-150",
        size === "md" ? "h-9 w-9" : "h-[30px] w-[30px]",
        tone === "danger"
          ? "text-white/90 hover:bg-rec/15 hover:text-rec"
          : "text-white/90 hover:bg-white/10 hover:text-white",
        active && "bg-accent/20 text-white shadow-[inset_0_0_0_1px_rgba(255,255,255,0.06)]",
        disabled && "cursor-not-allowed opacity-40 hover:bg-transparent hover:text-white/90",
        className,
      )}
    >
      {children}
    </button>
  );
}

type PlayerRangeProps = {
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (value: number) => void;
  ariaLabel: string;
  ticks?: boolean;
};

/** B 站式水平滑杆：细轨道、圆形白色滑块、可选刻度。 */
function PlayerRange({ value, min, max, step, onChange, ariaLabel, ticks }: PlayerRangeProps) {
  const pct = max > min ? ((value - min) / (max - min)) * 100 : 0;
  return (
    <div className="hv-range flex-1">
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
        aria-label={ariaLabel}
        className="hv-range__input"
        style={{ "--progress": `${pct}%` } as CSSProperties}
      />
      {ticks ? (
        <div className="hv-range__ticks" aria-hidden>
          {Array.from({ length: 5 }, (_, index) => (
            <span key={index} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

type VolumeSliderProps = {
  volume: number;
  onChange: (volume: number) => void;
  label: string;
};

/** B 站式竖向音量滑杆：2px 轨道、4px 已填充段、12px 圆形滑块。 */
function VolumeSlider({ volume, onChange, label }: VolumeSliderProps) {
  const value = Math.min(1, Math.max(0, volume));
  const setFrom = (element: HTMLElement, clientY: number) => {
    const rect = element.getBoundingClientRect();
    if (!rect.height) return;
    onChange(Math.min(1, Math.max(0, 1 - (clientY - rect.top) / rect.height)));
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const jumps: Record<string, number> = {
      ArrowUp: value + 0.05,
      ArrowRight: value + 0.05,
      ArrowDown: value - 0.05,
      ArrowLeft: value - 0.05,
      PageUp: value + 0.2,
      PageDown: value - 0.2,
      Home: 0,
      End: 1,
    };
    if (!(event.key in jumps)) return;
    event.preventDefault();
    onChange(Math.min(1, Math.max(0, jumps[event.key])));
  };
  return (
    <div
      className="hv-volume"
      role="slider"
      tabIndex={0}
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(value * 100)}
      aria-valuetext={`${Math.round(value * 100)}%`}
      style={{ "--level": `${value * 100}%` } as CSSProperties}
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        setFrom(event.currentTarget, event.clientY);
      }}
      onPointerMove={(event) => {
        if (event.currentTarget.hasPointerCapture(event.pointerId)) setFrom(event.currentTarget, event.clientY);
      }}
      onPointerUp={(event) => {
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      onKeyDown={onKeyDown}
    >
      <div className="hv-volume__rail" />
      <div className="hv-volume__fill" />
      <div className="hv-volume__thumb" />
    </div>
  );
}

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
  onMenuOpen,
  fit = "contain",
  onToggleFit,
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
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [dmAdvanced, setDmAdvanced] = useState(false);
  // B 站式自动隐藏：移动时出现，静止约 2.8s 后收起；面板/菜单打开时保持显示。
  const [chromeVisible, setChromeVisible] = useState(false);
  const [qualityOpen, setQualityOpen] = useState(false);
  const chromeTimer = useRef<number | undefined>(undefined);
  const wakeChrome = () => {
    setChromeVisible(true);
    window.clearTimeout(chromeTimer.current);
    chromeTimer.current = window.setTimeout(() => setChromeVisible(false), 2800);
  };
  const hideChrome = () => {
    window.clearTimeout(chromeTimer.current);
    setChromeVisible(false);
  };
  useEffect(() => () => window.clearTimeout(chromeTimer.current), []);
  const prefs = useDanmakuPrefs();
  const chromeShown = chromeVisible || volOpen || prefsOpen || qualityOpen;
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
        icon: (lines.value ?? null) === option.value ? <Check size={14} className="text-accent-text" /> : <span />,
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
        icon: quality.value === option.value ? <Check size={14} className="text-accent-text" /> : <span />,
        onSelect: () => quality.onChange(option.value),
      });
      firstQuality = false;
    }
    qualityMenu.push({
      key: "qn:auto",
      label: quality.actual ? t("watch.autoQuality", { q: quality.actual }) : t("watch.qn.auto"),
      separatorBefore: !firstQuality,
      icon: quality.value === 0 ? <Check size={14} className="text-accent-text" /> : <span />,
      onSelect: () => quality.onChange(0),
    });
  }

  return (
    <div ref={shellRef} onPointerEnter={wakeChrome} onPointerMove={wakeChrome} onPointerLeave={hideChrome} className="relative h-full w-full overflow-hidden bg-black">
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
        onClick={() => {
          setPrefsOpen(false);
          toggle();
        }}
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
            statsCorner === "bottom-left"
              ? vod
                ? "bottom-[62px] left-2.5"
                : "bottom-[46px] left-2.5"
              : "right-2 top-2",
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

      {/* 弹幕设置：对齐 B 站官方面板（深色、粉色选中态、298px、底部主面板 + 高级设置）。 */}
      {prefsOpen ? (
        <div className="player-glass absolute bottom-[38px] right-3 z-30 w-[298px] max-w-[calc(100%-1.5rem)] overflow-hidden rounded-[4px] text-ink">
          {dmAdvanced ? (
            <>
              <button
                type="button"
                onClick={() => setDmAdvanced(false)}
                className="flex h-8 w-full items-center gap-2 px-5 text-left text-[12px] text-ink transition-colors hover:bg-raised"
              >
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
                  <path d="m10 3-5 5 5 5" strokeLinecap="round" strokeLinejoin="round" />
                </svg>
                {t("watch.danmakuAdvancedTitle")}
              </button>
              <div className="flex h-10 items-center border-t border-line px-5">
                <button
                  type="button"
                  onClick={() => prefs.reset()}
                  className="focus-ring text-[12px] text-muted transition-colors hover:text-ink"
                >
                  {t("watch.danmakuReset")}
                </button>
              </div>
            </>
          ) : (
            <>
              <div className="px-5 py-3">
                <div className="flex h-[37px] items-center gap-2.5 py-2.5">
                  <span className="w-12 shrink-0 text-[12px] text-ink-soft">{t("watch.danmakuArea")}</span>
                  <PlayerRange
                    value={prefs.area}
                    min={0.3}
                    max={1}
                    step={0.05}
                    onChange={(next) => prefs.set({ area: next })}
                    ariaLabel={t("watch.danmakuArea")}
                    ticks
                  />
                  <span className="w-9 shrink-0 text-right text-[12px] text-ink-soft">
                    {Math.round(prefs.area * 100)}%
                  </span>
                </div>

                <div className="flex h-[34px] items-center">
                  <div className="grid h-[30px] w-full grid-cols-3 overflow-hidden rounded-[3px] bg-raised text-[12px]">
                    {[
                      [t("watch.danmakuDensityNormal"), 1],
                      [t("watch.danmakuDensityMore"), 1.35],
                      [t("watch.danmakuDensityOverlap"), 1.8],
                    ].map(([label, value], index) => {
                      const active = Math.abs(prefs.density - (value as number)) < 0.05;
                      return (
                        <button
                          key={label as string}
                          type="button"
                          onClick={() => prefs.set({ density: value as number })}
                          className={[
                            "focus-ring transition-colors",
                            index > 0 ? "border-l border-line" : "",
                            active ? "font-medium text-accent-text" : "text-muted hover:text-ink",
                          ].join(" ")}
                        >
                          {label as string}
                        </button>
                      );
                    })}
                  </div>
                </div>

                <div className="flex h-[37px] items-center gap-2.5 py-2.5">
                  <span className="w-12 shrink-0 text-[12px] text-ink-soft">{t("watch.danmakuOpacity")}</span>
                  <PlayerRange
                    value={prefs.opacity}
                    min={0.3}
                    max={1}
                    step={0.05}
                    onChange={(next) => prefs.set({ opacity: next })}
                    ariaLabel={t("watch.danmakuOpacity")}
                  />
                  <span className="w-9 shrink-0 text-right text-[12px] text-ink-soft">
                    {Math.round(prefs.opacity * 100)}%
                  </span>
                </div>

                <div className="flex h-[37px] items-center gap-2.5 py-2.5">
                  <span className="w-12 shrink-0 text-[12px] text-ink-soft">{t("watch.danmakuFont")}</span>
                  <PlayerRange
                    value={prefs.fontScale}
                    min={0.75}
                    max={1.6}
                    step={0.05}
                    onChange={(next) => prefs.set({ fontScale: next })}
                    ariaLabel={t("watch.danmakuFont")}
                  />
                  <span className="w-9 shrink-0 text-right text-[12px] text-ink-soft">
                    {Math.round(prefs.fontScale * 100)}%
                  </span>
                </div>

                <div className="flex h-[37px] items-center gap-2.5 py-2.5">
                  <span className="w-12 shrink-0 text-[12px] text-ink-soft">{t("watch.danmakuSpeed")}</span>
                  <PlayerRange
                    value={prefs.speed}
                    min={0.5}
                    max={2}
                    step={0.1}
                    onChange={(next) => prefs.set({ speed: next })}
                    ariaLabel={t("watch.danmakuSpeed")}
                    ticks
                  />
                  <span className="w-9 shrink-0 text-right text-[12px] text-ink-soft">
                    {prefs.speed < 0.85
                      ? t("watch.danmakuSpeedSlow")
                      : prefs.speed > 1.15
                        ? t("watch.danmakuSpeedFast")
                        : t("watch.danmakuSpeedMedium")}
                  </span>
                </div>

                <div className="mt-2 flex">
                  <button
                    type="button"
                    onClick={() => prefs.set({ scaleWithScreen: !prefs.scaleWithScreen })}
                    aria-pressed={prefs.scaleWithScreen}
                    className="focus-ring flex items-center gap-1.5 text-[12px]"
                  >
                    <span
                      className={cx(
                        "flex h-3 w-3 shrink-0 items-center justify-center rounded-[2px]",
                        prefs.scaleWithScreen
                          ? "bg-accent text-[var(--c-accent-ink)]"
                          : "border border-line-strong bg-transparent",
                      )}
                    >
                      {prefs.scaleWithScreen ? <Check size={9} strokeWidth={3} /> : null}
                    </span>
                    <span className={prefs.scaleWithScreen ? "text-accent-text" : "text-ink-soft"}>
                      {t("watch.danmakuScaleWithScreen")}
                    </span>
                  </button>
                </div>
              </div>

              <button
                type="button"
                onClick={() => setDmAdvanced(true)}
                className="flex h-10 w-full items-center justify-between border-t border-line px-5 text-[12px] text-ink-soft transition-colors hover:bg-raised"
              >
                <span>{t("watch.danmakuAdvanced")}</span>
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
                  <path d="m6 3 5 5-5 5" strokeLinecap="round" strokeLinejoin="round" />
                </svg>
              </button>
            </>
          )}
        </div>
      ) : null}

      {/* 控制条：对齐 B 站网页直播：56px 渐变底、30px 图标、左右两组。 */}
      <div
        onPointerEnter={() => {
          window.clearTimeout(chromeTimer.current);
          setChromeVisible(true);
        }}
        onPointerLeave={wakeChrome}
        onPointerMove={(event) => event.stopPropagation()}
        className={cx(
          "absolute inset-x-0 bottom-0 z-20 h-14 select-none bg-gradient-to-t from-black/70 to-transparent transition-[opacity,transform] duration-200",
          chromeShown ? "pointer-events-auto translate-y-0 opacity-100" : "pointer-events-none translate-y-1 opacity-0",
        )}
      >
        {vod ? (
          <div className="absolute inset-x-3 top-1.5 flex h-3 items-center">
            <div className="absolute inset-x-0 h-[3px] overflow-hidden rounded-full bg-white/15">
              <div className="h-full rounded-full bg-accent transition-[width] duration-150" style={{ width: playbackPct + "%" }} />
            </div>
            <span
              className="pointer-events-none absolute top-1/2 h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-white shadow-[0_1px_6px_rgba(0,0,0,0.55)]"
              style={{ left: `${playbackPct}%` }}
            />
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
              className="absolute inset-x-0 h-3 w-full cursor-pointer appearance-none bg-transparent opacity-0"
            />
          </div>
        ) : null}

        <div className="absolute inset-x-0 bottom-[3px] flex h-9 items-center gap-1 px-3 [text-shadow:0_1px_2px_rgba(0,0,0,0.6)]">
          <PlayerButton
            label={playing ? t("watch.paused") : t("watch.playing")}
            onClick={toggle}
          >
            {playing ? (
              <svg width="20" height="20" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
                <rect x="2" y="1.5" width="3" height="9" rx="0.8" />
                <rect x="7" y="1.5" width="3" height="9" rx="0.8" />
              </svg>
            ) : (
              <svg width="20" height="20" viewBox="0 0 12 12" fill="currentColor" aria-hidden>
                <path d="M3 1.6v8.8a.6.6 0 0 0 .92.5l6.2-4.4a.6.6 0 0 0 0-1L3.92 1.1A.6.6 0 0 0 3 1.6Z" />
              </svg>
            )}
          </PlayerButton>

          <PlayerButton label={t("watch.reload")} onClick={reload}>
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
              <path d="M20 7.5A8.5 8.5 0 1 0 20.5 15" strokeLinecap="round" />
              <path d="M20.5 3.8v5.2h-5.2" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </PlayerButton>

          <div
            className="relative flex shrink-0 items-center"
            onMouseEnter={() => setVolOpen(true)}
            onMouseLeave={() => setVolOpen(false)}
            onFocusCapture={() => setVolOpen(true)}
            onBlurCapture={(event) => {
              if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setVolOpen(false);
            }}
          >
            <PlayerButton label={muted ? t("watch.muted") : t("watch.audioOn")} onClick={toggleMute}>
              {muted ? (
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                  <path d="M13 5.5 8.5 9H5v6h3.5l4.5 3.5V5.5Z" strokeLinejoin="round" />
                  <path d="m16.5 9.5 5 5M21.5 9.5l-5 5" strokeLinecap="round" />
                </svg>
              ) : (
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                  <path d="M13 5.5 8.5 9H5v6h3.5l4.5 3.5V5.5Z" strokeLinejoin="round" />
                  <path d="M16.2 9a4.5 4.5 0 0 1 0 6M18.8 6.6a8 8 0 0 1 0 10.8" strokeLinecap="round" />
                </svg>
              )}
            </PlayerButton>
            {volOpen ? (
              <div className="absolute bottom-full left-1/2 z-40 -translate-x-1/2 pb-1">
                <div className="player-glass flex h-[110px] w-8 flex-col items-center rounded-[10px] px-1 py-1.5 text-ink">
                  <span className="text-[10px] font-medium tabular-nums">{Math.round(volume * 100)}</span>
                  <VolumeSlider volume={volume} onChange={onVolumeChange} label={t("watch.audioOn")} />
                </div>
              </div>
            ) : null}
          </div>

          <span className="shrink-0 text-[12px] tabular-nums text-white/90">
            {vod
              ? clock(scrubMs ?? positionMs) + " / " + clock(durationMs ?? 0)
              : clock(elapsed * 1000)}
          </span>

          <div className="ml-auto flex items-center gap-1">
            {quality ? (
              <DropdownMenu
                variant="video"
                side="top"
                align="end"
                label={qualityLabel ?? t("watch.qn.auto")}
                ariaLabel={t("watch.quality")}
                items={qualityMenu}
                morphLabel={false}
                className="w-[108px]"
                onOpenChange={(open) => {
                  setQualityOpen(open);
                  if (open) onMenuOpen?.();
                }}
              />
            ) : null}

            <PlayerButton label={t("watch.pip")} onClick={togglePip} disabled={!pipSupported}>
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
                <rect x="12" y="12" width="7" height="5.5" rx="1" fill="currentColor" stroke="none" opacity="0.9" />
              </svg>
            </PlayerButton>

            <PlayerButton
              label={danmakuEnabled ? t("watch.danmakuHide") : t("watch.danmakuShow")}
              active={danmakuEnabled}
              disabled={!onToggleDanmaku}
              onClick={onToggleDanmaku}
            >
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                <rect x="3" y="5" width="18" height="12" rx="2.5" />
                <path d="M6.5 9.5h6M6.5 12.5h10" strokeLinecap="round" opacity="0.75" />
                <circle cx="17" cy="9.5" r="1" fill="currentColor" stroke="none" />
                {!danmakuEnabled ? <path d="M4 18.5 20 3.5" strokeLinecap="round" /> : null}
              </svg>
            </PlayerButton>

            <PlayerButton
              label={t("watch.danmakuSettings")}
              active={prefsOpen}
              onClick={() => {
                if (!prefsOpen) setDmAdvanced(false);
                setPrefsOpen(!prefsOpen);
              }}
            >
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                <path d="M4 7h16M4 12h16M4 17h16" strokeLinecap="round" opacity="0.65" />
                <circle cx="8.5" cy="7" r="2" fill="currentColor" stroke="none" />
                <circle cx="15.5" cy="12" r="2" fill="currentColor" stroke="none" />
                <circle cx="10.5" cy="17" r="2" fill="currentColor" stroke="none" />
              </svg>
            </PlayerButton>

            {onToggleFit ? (
              <PlayerButton
                label={fit === "cover" ? t("watch.fitContain") : t("watch.fitCover")}
                active={fit === "cover"}
                onClick={onToggleFit}
              >
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                  <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
                  {fit === "cover" ? (
                    <rect x="6.5" y="7.5" width="11" height="9" rx="1" fill="currentColor" stroke="none" opacity="0.85" />
                  ) : (
                    <rect x="7.5" y="8.5" width="9" height="7" rx="1" opacity="0.75" />
                  )}
                </svg>
              </PlayerButton>
            ) : null}

            <PlayerButton label={t("watch.fullscreen")} onClick={fullscreen}>
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden>
                <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </PlayerButton>
          </div>
        </div>
      </div>

    </div>
  );
}