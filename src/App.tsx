import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion, useReducedMotion } from "motion/react";import { Button, IconButton, cx } from "./components/ui/Controls";
import WindowControls from "./components/WindowControls";
import { ToastStack } from "./components/arc/toast-stack/toast-stack";
import ResizeHandles from "./components/ResizeHandles";
import MediaPage from "./features/media/MediaPage";
import MonitorPage from "./features/monitor/MonitorPage";
import SettingsView, { type PageKey } from "./features/settings/SettingsView";
import TasksPage from "./features/tasks/TasksPage";
import WatchPage, { type WatchSeed } from "./features/watch/WatchPage";
import { setLanguage, type Language } from "./i18n";
import appMark from "./assets/app-mark.png";
import CloseDialog from "./features/close/CloseDialog";
import {
  activeTasks,
  closeArmQuit,
  closeCancelQuit,
  closeQuitNow,
  ffmpegStatus,
  isTauri,
  onAppNotice,
  onCloseBlocked,
  onCloseReady,
  settingsGet,
  settingsUpdate,
  type ActiveTask,
  type Settings,
} from "./lib/ipc";
import { suspendViews } from "./lib/viewLifecycle";
import { useNotify } from "./lib/notify";
import { applyTheme, watchSystemTheme, type ThemeMode } from "./lib/theme";
import { ThemeSwitch, type Theme, type ThemeSwitchVariant } from "@/components/arc/theme-switch/theme-switch";

type TabKey = "monitor" | "watch" | "media" | "tasks";
const PAGE_EASE: [number, number, number, number] = [0.22, 1, 0.36, 1];

export default function App() {
  const { t, i18n } = useTranslation();
  const [tab, setTab] = useState<TabKey>("monitor");
  const [settingsOpen, setSettingsOpen] = useState(false);
  // 从缺失提示进入设置时落到存储页；null = 普通打开。
  const [settingsPage, setSettingsPage] = useState<PageKey | null>(null);
  const [ffmpegMissing, setFfmpegMissing] = useState(false);
  const ffmpegToastShown = useRef(false);
  const notify = useNotify();
  // 观看页全屏时收起顶栏，把整块窗口让给画面。
  const [immersive, setImmersive] = useState(false);
  const [theme, setTheme] = useState<ThemeMode>("light");
  // 实际生效的明暗（theme=system 时取系统值），顶栏的 ThemeSwitch 只看这个。
  const [appearance, setAppearance] = useState<Theme>("light");
  const themeTimer = useRef<number | null>(null);
  // 监控页多选后带进观看页；nonce 让同一批房间再点一次也能重新应用。
  const [watchSeed, setWatchSeed] = useState<WatchSeed | null>(null);
  // 监控详情跳到媒体库/任务页时带的房间筛选；nonce 让重复跳转也生效。
  const [mediaSeed, setMediaSeed] = useState<{ roomId: string; nonce: number } | null>(null);
  const [taskSeed, setTaskSeed] = useState<{ roomId: string; nonce: number } | null>(null);
  // 通知开关按事件分三类：录制、异常、登录失效。
  const notifyRef = useRef<Settings | null>(null);
  // 退出被任务拦下：null = 没有提示；[] = 任务已清空，可以安全退出。
  const [closeTasks, setCloseTasks] = useState<ActiveTask[] | null>(null);
  const [closeArmed, setCloseArmed] = useState(false);
  const [closeBusy, setCloseBusy] = useState(false);
  const prevTab = useRef<TabKey>(tab);
  const reduceMotion = useReducedMotion() ?? false;
  const lang = i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";

  // 启动和保存设置后都重新探测一次：用户可能刚补上 ffmpeg 或换掉路径。
  const refreshFfmpeg = useCallback(() => {
    if (!isTauri()) return;
    void ffmpegStatus()
      .then((status) => setFfmpegMissing(status.path === null))
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    refreshFfmpeg();
  }, [refreshFfmpeg]);

  // ffmpeg 缺失是持久环境问题：用一条不自动消失的顶层 warning toast 代替页面内的横条。
  useEffect(() => {
    if (!ffmpegMissing) {
      ffmpegToastShown.current = false;
      return;
    }
    if (ffmpegToastShown.current) return;
    ffmpegToastShown.current = true;
    notify.toast({
      type: "warning",
      title: t("app.ffmpegMissing"),
      action: {
        label: t("app.ffmpegFix"),
        onClick: () => {
          setSettingsPage("storage");
          setSettingsOpen(true);
        },
      },
      duration: Infinity,
    });
  }, [ffmpegMissing, notify, t]);

  // 启动时读一次性设置：语言、主题与通知开关。读失败就保持默认，不打扰用户。
  useEffect(() => {
    if (!isTauri()) {
      applyTheme("light");
      return;
    }
    void settingsGet()
      .then((settings) => {
        notifyRef.current = settings;
        if (settings.language !== "system") setLanguage(settings.language as Language);
        const mode = (settings.theme as ThemeMode) ?? "light";
        setTheme(mode);
        setAppearance(applyTheme(mode));
      })
      .catch(() => applyTheme("light"));
  }, []);

  // 后端拦下退出后弹提示；任务清空时同一个面板切成「可以安全退出」。
  useEffect(() => {
    if (!isTauri()) return;
    const unlisteners: Array<() => void> = [];
    let alive = true;
    void onCloseBlocked((tasks) => {
      if (!alive) return;
      setCloseTasks(tasks);
      setCloseArmed(false);
    }).then((off) => (alive ? unlisteners.push(off) : off()));
    void onCloseReady(() => {
      if (!alive) return;
      setCloseTasks((current) => (current ? [] : current));
    }).then((off) => (alive ? unlisteners.push(off) : off()));
    return () => {
      alive = false;
      unlisteners.forEach((off) => off());
    };
  }, []);

  // 后端重启 / 事件漏收时兜底拉一次任务表。
  useEffect(() => {
    if (!isTauri()) return;
    const timer = window.setInterval(() => {
      if (closeTasks && closeTasks.length > 0) {
        void activeTasks().then((tasks) => setCloseTasks(tasks)).catch(() => undefined);
      }
    }, 2500);
    return () => window.clearInterval(timer);
  }, [closeTasks]);

  const cancelClose = () => {
    setCloseTasks(null);
    setCloseArmed(false);
    void closeCancelQuit().catch(() => undefined);
  };

  const armClose = () => {
    setCloseArmed(true);
    void closeArmQuit().catch(() => setCloseArmed(false));
  };

  const disarmClose = () => {
    setCloseArmed(false);
    void closeCancelQuit().catch(() => undefined);
  };

  const quitClose = () => {
    setCloseBusy(true);
    void closeQuitNow().finally(() => setCloseBusy(false));
  };

  // Cmd/Ctrl + , 打开设置，和 macOS 惯例一致。
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === ",") {
        event.preventDefault();
        setSettingsOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // 离开观看页就停掉所有观看管道：不拉流、不转封装，但房间与布局留在本地。
  useEffect(() => {
    if (prevTab.current === "watch" && tab !== "watch") void suspendViews();
    if (tab !== "watch") setImmersive(false);
    prevTab.current = tab;
  }, [tab]);

  // 跟随系统时，系统换主题要跟着换。
  useEffect(() => {
    if (theme !== "system") return;
    return watchSystemTheme(() => setAppearance(applyTheme("system")));
  }, [theme]);

  // 卸载时清掉还在等待的明暗扩散，避免定时器回调用在已卸载的窗口上。
  useEffect(
    () => () => {
      if (themeTimer.current !== null) window.clearTimeout(themeTimer.current);
    },
    [],
  );

  const persist = (patch: Record<string, unknown>) => {
    if (!isTauri()) return;
    void settingsGet()
      .then((settings) => settingsUpdate({ ...settings, ...patch }))
      .catch(() => undefined);
  };

  // 顶栏语言切换也写回设置，免得重启后被设置里的旧值覆盖。
  const pickLanguage = (code: Language) => {
    setLanguage(code);
    persist({ language: code });
  };

  // 顶栏只做明暗互换；「跟随系统」在设置里选。
  // View Transition 会把整页冻结成快照：如果图标 spring 还在跑，过渡结束的一瞬间才落到终态，看起来会抖一下。
  // 所以先让图标（260ms spring）跑完，再开始整页扩散，快照里的图标已经是终态。
  const changeTheme = (next: Theme, _variant: ThemeSwitchVariant, trigger: HTMLElement) => {
    if (themeTimer.current !== null) window.clearTimeout(themeTimer.current);
    const { left, top, width, height } = trigger.getBoundingClientRect();
    const x = `${left + width / 2}px`;
    const y = `${top + height / 2}px`;
    setTheme(next);
    setAppearance(next);
    themeTimer.current = window.setTimeout(() => {
      themeTimer.current = null;
      const apply = () => {
        applyTheme(next);
        persist({ theme: next });
      };
      if (!document.startViewTransition || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
        apply();
        return;
      }
      document.documentElement.style.setProperty("--x", x);
      document.documentElement.style.setProperty("--y", y);
      document.startViewTransition(apply);
    }, 320);
  };

  // 录制播报走系统通知；文案在前端 i18n，切语言即时生效。
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    let disposed = false;

    void (async () => {
      try {
        const { isPermissionGranted, requestPermission, sendNotification } = await import(
          "@tauri-apps/plugin-notification"
        );
        let granted = await isPermissionGranted();
        if (!granted) granted = (await requestPermission()) === "granted";

        const off = await onAppNotice((notice) => {
          if (notice.suppress) return;
          const current = notifyRef.current;
          if (!current) return;
          const allowed =
            notice.kind === "credential_expired"
              ? current.notifyAuth
              : notice.kind === "started" || notice.kind === "ended"
                ? current.notifyEnabled
                : current.notifyError;
          if (!allowed) return;
          const key =
            notice.kind === "disk_full"
              ? "diskFull"
              : notice.kind === "disk_lost"
                ? "diskLost"
                : notice.kind === "view_failed"
                  ? "viewFailed"
                  : notice.kind === "credential_expired"
                    ? "credentialExpired"
                    : notice.kind === "quality_fallback"
                      ? "qualityFallback"
                      : notice.kind;
          const title = t(`notice.${key}.title`);
          const body = t(`notice.${key}.body`, { host: notice.hostName, detail: notice.detail ?? "" });
          if (granted) sendNotification({ title, body });
          notify.toast({
            type:
              notice.kind === "started" || notice.kind === "ended"
                ? "success"
                : notice.kind === "quality_fallback"
                  ? "info"
                  : notice.kind === "credential_expired"
                    ? "warning"
                    : "error",
            title,
            description: body,
          });
        });
        if (disposed) off();
        else unlisten = off;
      } catch (err) {
        console.warn("notification unavailable", err);
      }
    })();

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [notify, t]);

  const dark = appearance === "dark";
  // macOS 用 Overlay 标题栏，红绿灯浮在自绘顶栏左侧，需要给它让位。
  const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  // Windows 由后端关掉原生边框，顶栏右侧自绘最小化/最大化/关闭。
  const isWin = isTauri() && /Windows/.test(navigator.userAgent);

  return (
    <div className="flex h-full min-h-0 flex-col bg-canvas text-ink">
      {isWin ? <ResizeHandles /> : null}
      {tab === "watch" && immersive ? null : (
      <header
        data-tauri-drag-region
        style={{ paddingLeft: isMac ? 78 : 14, paddingRight: isWin ? 0 : 12 }}
        className="flex h-12 shrink-0 items-stretch border-b border-line"
      >
        <div data-tauri-drag-region className="flex items-center gap-2 pr-2">
          <img
            src={appMark}
            alt=""
            width={24}
            height={24}
            draggable={false}
            className="h-6 w-6 shrink-0 select-none rounded-[6px]"
          />
          <span data-tauri-drag-region className="text-[11.5px] font-semibold tracking-[0.16em] text-ink">HIVECAST</span>
        </div>

        <nav className="ml-2 flex items-stretch">
          {(
            [
              { key: "monitor", label: t("tab.monitor") },
              { key: "watch", label: t("tab.watch") },
              { key: "media", label: t("tab.media") },
              { key: "tasks", label: t("tab.tasks") },
            ] as Array<{ key: TabKey; label: string }>
          ).map((item) => (
            <button
              key={item.key}
              type="button"
              onClick={() => setTab(item.key)}
              className={cx(
                "focus-ring relative px-3 text-[12px] font-medium transition-colors",
                tab === item.key ? "text-ink" : "text-muted hover:text-ink-soft",
              )}
            >
              {item.label}
              {tab === item.key ? (
                <motion.span
                  layoutId="tab-underline"
                  className="absolute inset-x-2 bottom-0 h-[2px] rounded-t-[2px] bg-accent"
                  transition={reduceMotion ? { duration: 0 } : { type: "spring", stiffness: 420, damping: 36 }}
                />
              ) : null}
            </button>
          ))}
        </nav>

        <div className="ml-auto flex items-center gap-1">
          {!isTauri() && (
            <span className="rounded-full bg-warn/12 px-2 py-1 text-[10.5px] text-warn">preview</span>
          )}
          <Button
            size="sm"
            variant="ghost"
            title={t("settings.language")}
            onClick={() => pickLanguage(lang === "zh-CN" ? "en-US" : "zh-CN")}
          >
            {lang === "zh-CN" ? "中" : "EN"}
          </Button>
          <ThemeSwitch
            theme={appearance}
            iconOnly
            bare
            label={dark ? t("app.themeSwitchLight") : t("app.themeSwitchDark")}
            onThemeChange={changeTheme}
          />
          <IconButton
            label={t("settings.title")}
            onClick={() => {
              setSettingsPage(null);
              setSettingsOpen(true);
            }}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 1.55V21a2 2 0 1 1-4 0v-.09A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.55-1H3a2 2 0 1 1 0-4h.09A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.6h.09A1.7 1.7 0 0 0 10 3.05V3a2 2 0 1 1 4 0v.09A1.7 1.7 0 0 0 15 4.6a1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 9v.09a1.7 1.7 0 0 0 1.55 1H21a2 2 0 1 1 0 4h-.09a1.7 1.7 0 0 0-1.51 1Z" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </IconButton>
          {isWin ? <WindowControls /> : null}
        </div>
      </header>
      )}



      <motion.main
        key={tab}
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: reduceMotion ? 0 : 0.16, ease: PAGE_EASE }}
        className="flex min-h-0 flex-1 flex-col"
      >
        {tab === "monitor" ? (
          <MonitorPage
            onOpenWatch={(ids, kind) => {
              if (ids && ids.length > 0) setWatchSeed({ ids, nonce: Date.now(), kind });
              setTab("watch");
            }}
            onOpenMedia={(roomId) => {
              setMediaSeed({ roomId, nonce: Date.now() });
              setTab("media");
            }}
            onOpenTasks={(roomId) => {
              setTaskSeed({ roomId, nonce: Date.now() });
              setTab("tasks");
            }}
          />
        ) : tab === "watch" ? (
          <WatchPage
            seed={watchSeed}
            onSeedApplied={() => setWatchSeed(null)}
            onImmersiveChange={setImmersive}
          />
        ) : tab === "media" ? (
          <MediaPage seed={mediaSeed} onSeedApplied={() => setMediaSeed(null)} />
        ) : (
          <TasksPage seed={taskSeed} onSeedApplied={() => setTaskSeed(null)} />
        )}
      </motion.main>

      {closeTasks ? (
        <CloseDialog
          tasks={closeTasks}
          armed={closeArmed}
          busy={closeBusy}
          onCancel={cancelClose}
          onArm={armClose}
          onDisarm={disarmClose}
          onQuitNow={quitClose}
        />
      ) : null}

      <ToastStack position="top-center" label={t("toast.region")} />

      <SettingsView
        open={settingsOpen}
        initialPage={settingsPage ?? undefined}
        onClose={() => {
          setSettingsOpen(false);
          setSettingsPage(null);
        }}
        onSaved={(settings) => {
          notifyRef.current = settings;
          const mode = (settings.theme as ThemeMode) ?? "light";
          setTheme(mode);
          setAppearance(applyTheme(mode));
          refreshFfmpeg();
        }}
      />
    </div>
  );
}