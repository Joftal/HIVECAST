import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  ArrowUpCircle,
  Bug,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  Copy,
  ExternalLink,
  FileText,
  FolderOpen,
  Heart,
  Loader2,
  RefreshCw,
} from "lucide-react";
import appIcon from "../../assets/app-icon.png";
import { cx } from "../../components/ui/Controls";
import {
  appCheckUpdate,
  appOpenDataDir,
  describeError,
  isTauri,
  logsOpen,
  openUrl,
  type AppInfo,
  type FfmpegStatus,
  type ReleaseEntry,
  type UpdateCheckResult,
} from "../../lib/ipc";
import { useNotify } from "../../lib/notify";

const GITHUB_REPO_URL = "https://github.com/Joftal/HIVECAST";
const GITHUB_RELEASES_URL = "https://github.com/Joftal/HIVECAST/releases";
const GITHUB_ISSUES_URL = "https://github.com/Joftal/HIVECAST/issues";
const CHANGELOG_SITE_URL = "https://joftal.github.io/HIVECAST/";

// 会话级缓存：切换设置子页时不重复发起远端请求，点击「检查更新」仍强制刷新。
let cachedUpdateResult: UpdateCheckResult | null = null;

interface AboutSectionProps {
  info: AppInfo | null;
  ffmpeg: FfmpegStatus | null;
  highlighted?: boolean;
}

const CREDITS_LIST: Array<{ name: string; license: string; url: string }> = [
  { name: "Tauri", license: "Apache-2.0 / MIT", url: "https://tauri.app" },
  { name: "Rust", license: "Apache-2.0 / MIT", url: "https://www.rust-lang.org" },
  { name: "React", license: "MIT", url: "https://react.dev" },
  { name: "Vite", license: "MIT", url: "https://vite.dev" },
  { name: "Tailwind CSS", license: "MIT", url: "https://tailwindcss.com" },
  { name: "FFmpeg", license: "LGPL / GPL", url: "https://ffmpeg.org" },
  { name: "SQLite / rusqlite", license: "Public Domain / MIT", url: "https://sqlite.org" },
  { name: "Tokio & Axum", license: "MIT", url: "https://tokio.rs" },
  { name: "hls.js", license: "Apache-2.0", url: "https://github.com/video-dev/hls.js" },
  { name: "Lucide Icons", license: "ISC", url: "https://lucide.dev" },
];

function formatPlatformLabel(platform?: string): string {
  if (!platform) return "Desktop";
  const lower = platform.toLowerCase();
  if (lower.includes("mac") || lower.includes("darwin")) return "macOS";
  if (lower.includes("win")) return "Windows";
  if (lower.includes("linux")) return "Linux";
  return platform;
}

function extractShortFfmpegVersion(raw?: string | null): string | null {
  if (!raw) return null;
  const match = /ffmpeg\s+version\s+([^\s]+)/i.exec(raw);
  return match ? match[1] : raw.slice(0, 24);
}

export function AboutSection({ info, ffmpeg, highlighted }: AboutSectionProps) {
  const { t } = useTranslation();
  const notify = useNotify();

  const [updateResult, setUpdateResult] = useState<UpdateCheckResult | null>(cachedUpdateResult);
  const [checking, setChecking] = useState(() => isTauri() && !cachedUpdateResult);
  const [changelogOpen, setChangelogOpen] = useState(true);
  const [creditsOpen, setCreditsOpen] = useState(false);

  const version = info?.version || "0.1.0";
  const platformLabel = formatPlatformLabel(info?.platform);
  const archLabel = info?.arch || "";

  const runCheckUpdate = useCallback(
    async (manual: boolean) => {
      if (!isTauri()) return;
      setChecking(true);
      try {
        const res = await appCheckUpdate();
        cachedUpdateResult = res;
        setUpdateResult(res);
        if (manual) {
          if (res.source === "offline") {
            notify.warning(t("settings.aboutUpdateOffline"));
          } else if (!res.latestVersion) {
            notify.warning(t("settings.aboutNoRelease"));
          } else if (res.hasUpdate && res.latestVersion) {
            notify.info(
              t("settings.aboutUpdateAvailable", {
                version: res.latestVersion,
                current: res.currentVersion,
              }),
            );
          } else {
            notify.success(t("settings.aboutUpToDate"));
          }
        }
      } catch (err) {
        if (manual) {
          notify.error(describeError(err));
        }
      } finally {
        setChecking(false);
      }
    },
    [notify, t],
  );

  useEffect(() => {
    if (!isTauri() || cachedUpdateResult) return;
    void runCheckUpdate(false);
  }, [runCheckUpdate]);

  const openExternal = useCallback(
    async (url: string) => {
      try {
        if (isTauri()) {
          await openUrl(url);
        } else {
          window.open(url, "_blank", "noopener,noreferrer");
        }
      } catch (err) {
        notify.error(describeError(err));
      }
    },
    [notify],
  );

  const ffmpegFound = Boolean(ffmpeg?.path && ffmpeg.source !== "missing");
  const ffmpegShortVersion = extractShortFfmpegVersion(ffmpeg?.version);

  const handleCopyEnv = useCallback(async () => {
    const ffText = ffmpegShortVersion
      ? `FFmpeg ${ffmpegShortVersion}`
      : ffmpegFound
        ? "FFmpeg detected"
        : "FFmpeg missing";
    const summary = `HIVECAST v${version} | ${platformLabel}${archLabel ? " " + archLabel : ""} | ${ffText}`;
    try {
      await navigator.clipboard.writeText(summary);
      notify.success(t("settings.aboutCopiedEnv"));
    } catch {
      notify.info(summary);
    }
  }, [archLabel, ffmpegFound, ffmpegShortVersion, notify, platformLabel, t, version]);

  const handleOpenDataDir = useCallback(async () => {
    if (!isTauri()) return;
    try {
      await appOpenDataDir();
    } catch (err) {
      notify.error(describeError(err));
    }
  }, [notify]);

  const handleOpenLogDir = useCallback(async () => {
    if (!isTauri()) return;
    try {
      await logsOpen();
    } catch (err) {
      notify.error(describeError(err));
    }
  }, [notify]);

  const releases: ReleaseEntry[] = useMemo(() => {
    if (updateResult) {
      return updateResult.releases.slice(0, 3);
    }
    return [
      {
        version,
        publishedAt: "",
        url: GITHUB_RELEASES_URL,
        prerelease: version.includes("-"),
        notes: [
          {
            kind: "feat",
            text: t("settings.aboutHeroDesc"),
          },
        ],
      },
    ];
  }, [t, updateResult, version]);

  const changelogSiteUrl = updateResult?.changelogUrl || CHANGELOG_SITE_URL;
  const downloadUrl = updateResult?.downloadUrl || GITHUB_RELEASES_URL;

  const techStack = useMemo(
    () => [
      {
        name: "Tauri 2",
        role: t("settings.aboutRoleTauri"),
        color: "#24c8db",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1">
            <circle cx="8" cy="8" r="3" />
            <circle cx="16" cy="16" r="3" />
            <path d="M13.5 6.5A6.5 6.5 0 0 1 18 13" />
            <path d="M10.5 17.5A6.5 6.5 0 0 1 6 11" />
          </svg>
        ),
      },
      {
        name: "Rust",
        role: t("settings.aboutRoleRust"),
        color: "#ea580c",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="7" />
            <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9 17 7M7 17l-2.1 2.1" />
            <text x="12" y="15" textAnchor="middle" fontSize="8" fontWeight="800" fill="currentColor" stroke="none">
              R
            </text>
          </svg>
        ),
      },
      {
        name: "React 19",
        role: t("settings.aboutRoleReact"),
        color: "#0ea5e9",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6">
            <circle cx="12" cy="12" r="2.2" fill="currentColor" />
            <ellipse cx="12" cy="12" rx="9.5" ry="3.8" />
            <ellipse cx="12" cy="12" rx="9.5" ry="3.8" transform="rotate(60 12 12)" />
            <ellipse cx="12" cy="12" rx="9.5" ry="3.8" transform="rotate(120 12 12)" />
          </svg>
        ),
      },
      {
        name: "TypeScript",
        role: t("settings.aboutRoleTs"),
        color: "#3178c6",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24">
            <rect x="2" y="2" width="20" height="20" rx="4" fill="currentColor" />
            <text x="12" y="16" textAnchor="middle" fontSize="10" fontWeight="800" fill="#fff">
              TS
            </text>
          </svg>
        ),
      },
      {
        name: "Vite 8",
        role: t("settings.aboutRoleVite"),
        color: "#8b5cf6",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />
          </svg>
        ),
      },
      {
        name: "Tailwind 4",
        role: t("settings.aboutRoleTailwind"),
        color: "#06b6d4",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 6c-2.7 0-4.3 1.3-5 4 1-1.3 2.2-1.8 3.5-1.5.8.2 1.3.7 1.9 1.3 1 1 2.1 2.2 4.6 2.2 2.7 0 4.3-1.3 5-4-1 1.3-2.2 1.8-3.5 1.5-.8-.2-1.3-.7-1.9-1.3C15.6 7.2 14.5 6 12 6zm-5 6c-2.7 0-4.3 1.3-5 4 1-1.3 2.2-1.8 3.5-1.5.8.2 1.3.7 1.9 1.3 1 1 2.1 2.2 4.6 2.2 2.7 0 4.3-1.3 5-4-1 1.3-2.2 1.8-3.5 1.5-.8-.2-1.3-.7-1.9-1.3C10.6 13.2 9.5 12 7 12z" />
          </svg>
        ),
      },
      {
        name: "FFmpeg",
        role: t("settings.aboutRoleFfmpeg"),
        color: "#16a34a",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.1">
            <path d="M4 6h5l-5 6h5l-5 6h6l9-12h-5l-9 12M15 12l5-6v12h-5" />
          </svg>
        ),
      },
      {
        name: "SQLite",
        role: t("settings.aboutRoleSqlite"),
        color: "#2563eb",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <ellipse cx="12" cy="5" rx="8" ry="3" />
            <path d="M4 5v14c0 1.7 3.6 3 8 3s8-1.3 8-3V5" />
            <path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3" />
          </svg>
        ),
      },
      {
        name: "Tokio",
        role: t("settings.aboutRoleTokio"),
        color: "#f59e0b",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M12 2v4M12 18v4M4.9 4.9l2.8 2.8M16.3 16.3l2.8 2.8M2 12h4M18 12h4M4.9 19.1l2.8-2.8M16.3 7.7l2.8-2.8" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        ),
      },
      {
        name: "hls.js",
        role: t("settings.aboutRoleHls"),
        color: "#e11d48",
        icon: (
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="4" width="18" height="14" rx="2" />
            <polygon points="10 8.5 15.5 11 10 13.5 10 8.5" fill="currentColor" stroke="none" />
            <path d="M7 21h10" />
          </svg>
        ),
      },
    ],
    [t],
  );

  const renderNoteBadge = (kind: string) => {
    switch (kind) {
      case "feat":
        return (
          <span className="shrink-0 rounded bg-accent-soft px-1.5 py-0.5 text-[10px] font-semibold text-accent-text">
            {t("settings.aboutTagFeat")}
          </span>
        );
      case "fix":
        return (
          <span className="shrink-0 rounded bg-ok/12 px-1.5 py-0.5 text-[10px] font-semibold text-ok">
            {t("settings.aboutTagFix")}
          </span>
        );
      case "perf":
        return (
          <span className="shrink-0 rounded bg-warn/12 px-1.5 py-0.5 text-[10px] font-semibold text-warn">
            {t("settings.aboutTagPerf")}
          </span>
        );
      default:
        return (
          <span className="shrink-0 rounded bg-raised px-1.5 py-0.5 text-[10px] font-semibold text-muted">
            {t("settings.aboutTagOther")}
          </span>
        );
    }
  };

  return (
    <div id="set-about" className={cx("space-y-4", highlighted && "rounded-xl ring-2 ring-accent/40")}>
      {/* 1. Hero Card */}
      <section className="relative overflow-hidden rounded-[14px] border border-line bg-surface shadow-[var(--shadow-card)]">
        <div
          className="pointer-events-none absolute inset-0"
          style={{
            background:
              "radial-gradient(280px 200px at 92% 0%, color-mix(in oklab, var(--c-accent) 14%, transparent), transparent 70%), radial-gradient(240px 180px at 26% 100%, color-mix(in oklab, var(--c-rec) 10%, transparent), transparent 72%)",
          }}
        />
        <div className="relative z-10 flex items-center justify-between gap-5 px-6 pb-5 pt-5">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2.5">
              <img
                src={appIcon}
                alt="HIVECAST"
                width={44}
                height={44}
                draggable={false}
                className="h-11 w-11 shrink-0 select-none rounded-[11px] shadow-[0_6px_16px_-6px_rgba(47,107,255,0.35)]"
              />
              <span className="text-[20px] font-bold tracking-[-0.02em] text-ink">HIVECAST</span>
              <span className="rounded-full border border-accent-line bg-accent-soft px-2.5 py-0.5 text-[11px] font-semibold text-accent-text">
                {t("settings.aboutOssBadge")}
              </span>
            </div>
            <h2 className="mt-3 text-[21px] font-bold leading-snug tracking-[-0.02em] text-ink">
              <span className="text-accent">{t("settings.aboutSloganHighlight")}</span>
              {t("settings.aboutSloganRest")}
            </h2>
            <p className="mt-2 max-w-[420px] text-[12.5px] leading-relaxed text-muted">
              {t("settings.aboutHeroDesc")}
            </p>
          </div>

          {/* Decorative Live Studio Card */}
          <div
            aria-hidden="true"
            className="hidden w-[216px] shrink-0 rounded-xl border border-line bg-surface/95 p-2.5 shadow-[0_12px_28px_-16px_rgba(16,24,40,0.22)] sm:block"
          >
            <div className="mb-2 flex items-center justify-between text-[10px] text-faint">
              <span className="font-semibold text-ink-soft">LIVE STUDIO</span>
              <span className="inline-flex items-center gap-1 rounded-full bg-rec/12 px-1.5 py-0.5 text-[9.5px] font-bold text-rec">
                <span className="h-1.5 w-1.5 rounded-full bg-rec" />
                REC
              </span>
            </div>
            <div className="mb-2 grid grid-cols-2 gap-1.5">
              <div className="flex h-11 flex-col justify-between rounded-[7px] border border-accent-line bg-accent-soft p-1.5">
                <span className="text-[9px] font-bold text-accent-text">Bilibili · 4K</span>
                <div className="h-1 w-3/4 rounded-full bg-accent/60" />
              </div>
              <div className="flex h-11 flex-col justify-between rounded-[7px] border border-line bg-raised p-1.5">
                <span className="text-[9px] font-semibold text-muted">Douyu · 60fps</span>
                <div className="h-1 w-1/2 rounded-full bg-line-strong" />
              </div>
            </div>
            <div className="space-y-1 rounded-[7px] bg-raised px-2 py-1.5 text-[9.5px] text-muted">
              <div className="flex items-center gap-1.5">
                <span className="h-1 w-3.5 shrink-0 rounded-full bg-accent" />
                <span className="truncate">FLV / TS · Danmaku Sync</span>
              </div>
              <div className="flex items-center gap-1.5">
                <span className="h-1 w-3.5 shrink-0 rounded-full bg-ok" />
                <span className="truncate">MP4 Remux · Local Only</span>
              </div>
            </div>
          </div>
        </div>

        {/* 6-Item Capability Strip */}
        <div className="relative z-10 grid grid-cols-3 border-t border-line bg-surface sm:grid-cols-6">
          {[
            {
              label: t("settings.aboutFeatMultiPlatform"),
              color: "#3b82f6",
              icon: (
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9">
                  <rect x="2" y="4" width="20" height="14" rx="2" />
                  <path d="M8 21h8M12 18v3" />
                </svg>
              ),
            },
            {
              label: t("settings.aboutFeatAutoRecord"),
              color: "#ef4444",
              icon: (
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9">
                  <circle cx="12" cy="12" r="9" />
                  <circle cx="12" cy="12" r="3.5" fill="currentColor" />
                </svg>
              ),
            },
            {
              label: t("settings.aboutFeatDanmaku"),
              color: "#8b5cf6",
              icon: (
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9">
                  <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
                  <path d="M8 9h8M8 13h5" />
                </svg>
              ),
            },
            {
              label: t("settings.aboutFeatMerge"),
              color: "#f59e0b",
              icon: (
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9">
                  <path d="m16 3 4 4-4 4" />
                  <path d="M20 7H4" />
                  <path d="m8 21-4-4 4-4" />
                  <path d="M4 17h16" />
                </svg>
              ),
            },
            {
              label: t("settings.aboutFeatPlayback"),
              color: "#10b981",
              icon: (
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9">
                  <polygon points="6 4 20 12 6 20 6 4" />
                </svg>
              ),
            },
            {
              label: t("settings.aboutFeatPrivacy"),
              color: "#06b6d4",
              icon: (
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9">
                  <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
                </svg>
              ),
            },
          ].map((feat, idx) => (
            <div
              key={feat.label}
              className={cx(
                "flex flex-col items-center gap-1.5 px-2 py-3 text-[11.5px] font-medium text-ink-soft",
                idx > 0 && "sm:border-l sm:border-line",
              )}
            >
              <span style={{ color: feat.color }}>{feat.icon}</span>
              <span className="truncate">{feat.label}</span>
            </div>
          ))}
        </div>
      </section>

      {/* 2. Version, Update Check & Embedded Changelog Panel */}
      <section className="overflow-hidden rounded-[12px] border border-line bg-surface shadow-[var(--shadow-card)]">
        <div className="flex flex-wrap items-center justify-between gap-3 px-[18px] pb-3.5 pt-4">
          <div>
            <div className="text-[11.5px] text-faint">
              {t("settings.aboutCurrentVersion")} · {t("settings.aboutVersionSynced")}
            </div>
            <div className="mt-0.5 flex items-baseline gap-2">
              <span className="font-mono text-[20px] font-bold tracking-[-0.02em] text-ink">
                v{version}
              </span>
              <span className="rounded-full border border-line bg-raised px-2 py-0.5 text-[11px] font-medium text-muted">
                {platformLabel}
                {archLabel ? ` ${archLabel}` : ""} · OSS
              </span>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={() => void openExternal(changelogSiteUrl)}
              className="focus-ring inline-flex items-center gap-1 rounded-[7px] px-2 py-1.5 text-[12.5px] font-semibold text-accent-text transition-colors hover:bg-accent-soft"
            >
              <span>{t("settings.aboutChangelogLink")}</span>
              <ExternalLink className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              onClick={() => void handleCopyEnv()}
              className="focus-ring inline-flex h-[31px] items-center gap-1.5 rounded-[7px] border border-line-strong bg-surface px-2.5 text-[12px] font-medium text-ink transition-colors hover:bg-raised"
            >
              <Copy className="h-3.5 w-3.5 text-muted" />
              <span>{t("settings.aboutCopyEnv")}</span>
            </button>
          </div>
        </div>

        {/* Update Status Bar */}
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line bg-canvas px-[18px] py-2.5">
          <div className="flex min-w-0 items-center gap-2 text-[12.5px] font-medium">
            {checking ? (
              <>
                <Loader2 className="h-4 w-4 shrink-0 animate-spin text-muted" />
                <span className="text-muted">{t("settings.aboutCheckingUpdate")}</span>
              </>
            ) : !updateResult ? (
              <>
                <RefreshCw className="h-4 w-4 shrink-0 text-muted" />
                <span className="text-muted">{t("settings.aboutVersionSynced")}</span>
              </>
            ) : updateResult.hasUpdate && updateResult.latestVersion ? (
              <>
                <ArrowUpCircle className="h-4 w-4 shrink-0 text-accent-text" />
                <span className="text-accent-text">
                  {t("settings.aboutUpdateAvailable", {
                    version: updateResult.latestVersion,
                    current: updateResult.currentVersion,
                  })}
                </span>
              </>
            ) : !updateResult.latestVersion ? (
              <>
                <AlertTriangle className="h-4 w-4 shrink-0 text-warn" />
                <span className="text-warn">
                  {t(
                    updateResult.source === "offline"
                      ? "settings.aboutUpdateOffline"
                      : "settings.aboutNoRelease",
                  )}
                </span>
              </>
            ) : (
              <>
                <CheckCircle2 className="h-4 w-4 shrink-0 text-ok" />
                <span className="text-ok">{t("settings.aboutUpToDate")}</span>
                {updateResult.source ? (
                  <span className="ml-1 hidden font-mono text-[10.5px] text-faint sm:inline">
                    ({t("settings.aboutUpdateSource", { source: updateResult.source })})
                  </span>
                ) : null}
              </>
            )}
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={checking}
              onClick={() => void runCheckUpdate(true)}
              className="focus-ring inline-flex h-[30px] items-center gap-1.5 rounded-[7px] border border-line-strong bg-surface px-2.5 text-[12px] font-medium text-ink transition-colors hover:bg-raised disabled:cursor-default disabled:opacity-60"
            >
              <RefreshCw className={cx("h-3.5 w-3.5 text-muted", checking && "animate-spin")} />
              <span>
                {checking
                  ? t("settings.aboutCheckingBtn")
                  : updateResult?.source === "offline"
                    ? t("settings.aboutRetryCheck")
                    : t("settings.aboutCheckUpdate")}
              </span>
            </button>
            {updateResult?.hasUpdate ? (
              <button
                type="button"
                onClick={() => void openExternal(downloadUrl)}
                className="focus-ring inline-flex h-[30px] items-center gap-1.5 rounded-[7px] bg-accent px-3 text-[12px] font-semibold text-accent-ink transition-opacity hover:opacity-95"
              >
                <span>{t("settings.aboutDownloadUpdate")}</span>
                <ExternalLink className="h-3.5 w-3.5" />
              </button>
            ) : null}
          </div>
        </div>

        {/* Collapsible Embedded Changelog Panel */}
        <div className="border-t border-line bg-surface">
          <button
            type="button"
            onClick={() => setChangelogOpen((v) => !v)}
            className="focus-ring flex w-full items-center justify-between px-[18px] py-2.5 text-left transition-colors hover:bg-canvas"
          >
            <div className="flex items-center gap-2 text-[12.5px] font-semibold text-ink">
              <FileText className="h-3.5 w-3.5 text-muted" />
              <span>{t("settings.aboutChangelogTitle")}</span>
              <span className="rounded-full bg-raised px-2 py-0.5 text-[10.5px] font-medium text-faint">
                {t(!updateResult || updateResult.source === "offline"
                  ? "settings.aboutBundledNotes"
                  : "settings.aboutChangelogSync")}
              </span>
            </div>
            <span className="inline-flex items-center gap-1 text-[11.5px] text-faint">
              {changelogOpen ? t("settings.aboutChangelogCollapse") : t("settings.aboutChangelogExpand")}
              {changelogOpen ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
            </span>
          </button>

          {changelogOpen ? (
            <div className="space-y-2.5 px-[18px] pb-4">
              {releases.length > 0 ? (
                releases.map((rel, idx) => (
                  <div
                    key={`${rel.version}-${idx}`}
                    className="rounded-[9px] border border-line bg-canvas px-3.5 py-3"
                  >
                    <div className="mb-2 flex items-center justify-between gap-2">
                      <div className="flex items-center gap-2 font-mono text-[13px] font-bold text-ink">
                        <span>v{rel.version}</span>
                        {updateResult?.latestVersion === rel.version ? (
                          <span className="rounded-full bg-accent-soft px-1.5 py-0.5 font-sans text-[10px] font-semibold text-accent-text">
                            Latest
                          </span>
                        ) : null}
                      </div>
                      {rel.publishedAt ? (
                        <span className="font-mono text-[11px] text-faint">{rel.publishedAt}</span>
                      ) : null}
                    </div>
                    {rel.notes.length > 0 ? (
                      <ul className="space-y-1.5 text-[12px] leading-relaxed text-ink-soft">
                        {rel.notes.map((note, noteIdx) => (
                          <li key={noteIdx} className="flex items-baseline gap-2">
                            {renderNoteBadge(note.kind)}
                            <span>{note.text}</span>
                          </li>
                        ))}
                      </ul>
                    ) : (
                      <p className="text-[12px] text-faint">{t("settings.aboutChangelogEmpty")}</p>
                    )}
                  </div>
                ))
              ) : (
                <p className="rounded-[9px] border border-line bg-canvas px-3.5 py-3 text-[12px] text-faint">
                  {t("settings.aboutChangelogEmpty")}
                </p>
              )}
            </div>
          ) : null}
        </div>
      </section>

      {/* 3. Two Quick Links (GitHub Releases / Bug Issues — single clear entry each) */}
      <section className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <button
          type="button"
          onClick={() => void openExternal(GITHUB_RELEASES_URL)}
          className="focus-ring flex items-center gap-3 rounded-[11px] border border-line bg-surface px-4 py-3 text-left transition-all hover:-translate-y-px hover:border-accent-line hover:bg-canvas"
        >
          <span className="flex h-[36px] w-[36px] shrink-0 items-center justify-center rounded-[9px] border border-line bg-raised text-ink">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="currentColor">
              <path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.57.1.78-.25.78-.55v-1.95c-3.2.7-3.87-1.36-3.87-1.36-.52-1.33-1.28-1.69-1.28-1.69-1.05-.72.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.25.73-1.54-2.55-.29-5.24-1.28-5.24-5.68 0-1.25.45-2.28 1.18-3.08-.12-.29-.51-1.46.11-3.04 0 0 .97-.31 3.16 1.18a10.9 10.9 0 0 1 5.76 0c2.2-1.49 3.16-1.18 3.16-1.18.63 1.58.23 2.75.11 3.04.74.8 1.18 1.83 1.18 3.08 0 4.41-2.69 5.39-5.25 5.67.41.36.78 1.06.78 2.14v3.17c0 .31.21.66.79.55A11.5 11.5 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5z" />
            </svg>
          </span>
          <span className="min-w-0 flex-1">
            <span className="flex items-center justify-between gap-1 text-[13px] font-semibold text-ink">
              <span className="truncate">{t("settings.aboutCardReleasesTitle")}</span>
              <ExternalLink className="h-3.5 w-3.5 shrink-0 text-faint" />
            </span>
            <span className="mt-0.5 block truncate text-[11.5px] text-muted">
              {t("settings.aboutCardReleasesDesc")}
            </span>
          </span>
        </button>

        <button
          type="button"
          onClick={() => void openExternal(GITHUB_ISSUES_URL)}
          className="focus-ring flex items-center gap-3 rounded-[11px] border border-line bg-surface px-4 py-3 text-left transition-all hover:-translate-y-px hover:border-accent-line hover:bg-canvas"
        >
          <span className="flex h-[36px] w-[36px] shrink-0 items-center justify-center rounded-[9px] border border-line bg-raised text-ink">
            <Bug className="h-[17px] w-[17px]" />
          </span>
          <span className="min-w-0 flex-1">
            <span className="flex items-center justify-between gap-1 text-[13px] font-semibold text-ink">
              <span className="truncate">{t("settings.aboutCardIssuesTitle")}</span>
              <ExternalLink className="h-3.5 w-3.5 shrink-0 text-faint" />
            </span>
            <span className="mt-0.5 block truncate text-[11.5px] text-muted">
              {t("settings.aboutCardIssuesDesc")}
            </span>
          </span>
        </button>
      </section>

      {/* 4. Runtime Diagnostics & Quick Folder Shortcuts */}
      <section className="flex flex-wrap items-center justify-between gap-2.5 rounded-[11px] border border-line bg-canvas px-3.5 py-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <span className="inline-flex items-center gap-1.5 rounded-[7px] border border-line bg-surface px-2.5 py-1 text-[11.5px] text-ink-soft">
            <span
              className={cx(
                "h-1.5 w-1.5 rounded-full",
                ffmpegFound ? "bg-ok" : "bg-warn",
              )}
            />
            {ffmpegFound
              ? t("settings.aboutFfmpegReady", { version: ffmpegShortVersion || "OK" })
              : t("settings.aboutFfmpegMissing")}
          </span>
          <span className="rounded-[7px] border border-line bg-surface px-2.5 py-1 text-[11.5px] text-ink-soft">
            SQLite
          </span>
          <span className="rounded-[7px] border border-line bg-surface px-2.5 py-1 font-mono text-[11.5px] text-ink-soft">
            {platformLabel}
            {archLabel ? ` · ${archLabel}` : ""}
          </span>
        </div>

        <div className="flex items-center gap-1.5">
          <button
            type="button"
            onClick={() => void handleOpenDataDir()}
            className="focus-ring inline-flex h-[27px] items-center gap-1 rounded-[7px] border border-line-strong bg-surface px-2.5 text-[11.5px] font-medium text-ink transition-colors hover:bg-raised"
          >
            <FolderOpen className="h-3 w-3 text-muted" />
            <span>{t("settings.aboutOpenDataDir")}</span>
          </button>
          <button
            type="button"
            onClick={() => void handleOpenLogDir()}
            className="focus-ring inline-flex h-[27px] items-center gap-1 rounded-[7px] border border-line-strong bg-surface px-2.5 text-[11.5px] font-medium text-ink transition-colors hover:bg-raised"
          >
            <FileText className="h-3 w-3 text-muted" />
            <span>{t("settings.aboutOpenLogDir")}</span>
          </button>
        </div>
      </section>

      {/* 5. Tech Stack Grid */}
      <section>
        <div className="mb-2.5 flex items-baseline justify-between gap-2">
          <h3 className="text-[12.5px] font-semibold text-ink">{t("settings.aboutTechTitle")}</h3>
          <span className="text-[11.5px] text-faint">{t("settings.aboutTechSub")}</span>
        </div>
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-5">
          {techStack.map((item) => (
            <div
              key={item.name}
              className="flex items-center gap-2 rounded-[10px] border border-line bg-surface px-2.5 py-2.5 transition-colors hover:border-line-strong hover:bg-canvas"
            >
              <span
                className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-[7px] bg-raised"
                style={{ color: item.color }}
              >
                {item.icon}
              </span>
              <div className="min-w-0">
                <div className="truncate text-[12px] font-semibold leading-tight text-ink">
                  {item.name}
                </div>
                <div className="truncate text-[10.5px] leading-tight text-faint">{item.role}</div>
              </div>
            </div>
          ))}
        </div>
      </section>

      {/* 6. Footer */}
      <footer className="flex flex-wrap items-end justify-between gap-4 border-t border-line pt-3 text-[11.5px] leading-relaxed text-faint">
        <div>
          <div className="flex items-center gap-1.5 font-medium text-ink-soft">
            <Heart className="h-3 w-3 fill-accent text-accent" />
            <span>{t("settings.aboutFooterSign")}</span>
          </div>
          <div>{t("settings.aboutFooterPrivacy")}</div>
          <div className="mt-1 flex items-center gap-3.5">
            <button
              type="button"
              onClick={() => void openExternal(GITHUB_REPO_URL)}
              className="focus-ring inline-flex items-center gap-1 font-medium text-accent-text hover:underline"
            >
              <span>{t("settings.aboutLinkRepo")}</span>
              <ExternalLink className="h-3 w-3" />
            </button>
            <button
              type="button"
              onClick={() => setCreditsOpen(true)}
              className="focus-ring font-medium text-accent-text hover:underline"
            >
              {t("settings.aboutLinkCredits")}
            </button>
          </div>
        </div>
        <div className="text-right">
          <div>Copyright © 2026 HIVECAST</div>
          <div className="opacity-80">HIVECAST · Capture Every Live Moment</div>
        </div>
      </footer>

      {/* Open Source Credits Modal */}
      {creditsOpen ? (
        <div
          className="fixed inset-0 z-[80] flex items-center justify-center bg-black/45 p-4 backdrop-blur-[2px]"
          onClick={() => setCreditsOpen(false)}
        >
          <div
            className="w-full max-w-[460px] rounded-xl border border-line bg-surface p-5 shadow-[var(--shadow-pop)]"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="text-[14px] font-semibold text-ink">
              {t("settings.aboutCreditsTitle")}
            </div>
            <p className="mt-1 text-[12px] text-muted">{t("settings.aboutCreditsDesc")}</p>
            <div className="mt-3 max-h-[280px] divide-y divide-line overflow-y-auto rounded-lg border border-line bg-canvas">
              {CREDITS_LIST.map((lib) => (
                <div key={lib.name} className="flex items-center justify-between px-3 py-2 text-[12px]">
                  <button
                    type="button"
                    onClick={() => void openExternal(lib.url)}
                    className="font-medium text-ink hover:text-accent-text hover:underline"
                  >
                    {lib.name}
                  </button>
                  <span className="font-mono text-[11px] text-faint">{lib.license}</span>
                </div>
              ))}
            </div>
            <div className="mt-4 flex justify-end">
              <button
                type="button"
                onClick={() => setCreditsOpen(false)}
                className="focus-ring h-8 rounded-[7px] border border-line-strong bg-surface px-3.5 text-[12px] font-medium text-ink hover:bg-raised"
              >
                {t("settings.aboutCreditsClose")}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
