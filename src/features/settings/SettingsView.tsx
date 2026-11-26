import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { AnimatePresence, LayoutGroup, motion, useReducedMotion } from "motion/react";
import { Check, Undo2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { setLanguagePreference, type Language } from "../../i18n";
import {
  appInfo,
  ffmpegStatus,
  filenamePreview,
  isTauri,
  logsOpen,
  logsStatus,
  settingsGet,
  trashInfo,
  trashOpen,
  settingsUpdate,
  type AppInfo,
  type FfmpegStatus,
  type FilenamePreview,
  type LogStatus,
  type Settings,
  type TrashInfo,
} from "../../lib/ipc";
import { Button, Segmented, Switch, cx } from "../../components/ui/Controls";
import { ButtonGroup } from "../../components/arc/button-group/button-group";
import { useNotify } from "../../lib/notify";
import { SearchField } from "../../components/arc/search-field/search-field";
import { SelectMenu } from "../../components/ui/SelectMenu";
import InfoTip from "../../components/ui/InfoTip";
import AccountSection from "./AccountSection";
import { AboutSection } from "./AboutSection";

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return (unit === 0 ? value.toFixed(0) : value.toFixed(1)) + " " + units[unit];
}

const POST_MODES: Array<{ value: "source" | "convert" | "custom"; labelKey: string; descKey: string }> = [
  { value: "source", labelKey: "settings.postModeSource", descKey: "settings.postModeSourceDesc" },
  { value: "convert", labelKey: "settings.postModeConvert", descKey: "settings.postModeConvertDesc" },
  { value: "custom", labelKey: "settings.postModeCustom", descKey: "settings.postModeCustomDesc" },
];

// 全局只保留「自动」与「最高可用」：B 站专有的 25000/15000/30000
// 不能发给斗鱼/虎牙，具体档位由房间级动态菜单决定。
const QN_OPTIONS = [
  { value: 0, key: "qnAuto" },
  { value: 40000, key: "qnHighest" },
];

const RECORD_QN_OPTIONS = [{ value: 40000, key: "qnHighest" }];

export type PageKey = "account" | "record" | "danmaku" | "storage" | "notify" | "appearance" | "system" | "about";

type NavGroup = "account" | "recording" | "app";

type PageMeta = {
  key: PageKey;
  labelKey: string;
  titleKey: string;
  descKey: string;
  icon: ReactNode;
  group: NavGroup;
};

const ICON = {
  account: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <circle cx="8" cy="5.4" r="2.6" />
      <path d="M3 13.4c.7-2.3 2.7-3.6 5-3.6s4.3 1.3 5 3.6" strokeLinecap="round" />
    </svg>
  ),
  record: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <rect x="1.8" y="3.6" width="8.4" height="8.8" rx="1.6" />
      <path d="M10.2 7.2 14 5.2v5.6l-3.8-2Z" strokeLinejoin="round" />
    </svg>
  ),
  danmaku: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <path d="M2.4 3.4h11.2v7.2H8.1l-3.2 2.3v-2.3H2.4Z" strokeLinejoin="round" />
      <path d="M5.1 6h5.8M5.1 8.2h4.1" strokeLinecap="round" />
    </svg>
  ),
  storage: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <ellipse cx="8" cy="4" rx="5.2" ry="2.2" />
      <path d="M2.8 4v8c0 1.2 2.3 2.2 5.2 2.2s5.2-1 5.2-2.2V4" />
      <path d="M2.8 8c0 1.2 2.3 2.2 5.2 2.2s5.2-1 5.2-2.2" />
    </svg>
  ),
  notify: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <path d="M4 6.8a4 4 0 1 1 8 0c0 2.6.8 3.6 1.2 4.1H2.8c.4-.5 1.2-1.5 1.2-4.1Z" strokeLinejoin="round" />
      <path d="M6.4 12.9a1.7 1.7 0 0 0 3.2 0" strokeLinecap="round" />
    </svg>
  ),
  appearance: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <circle cx="8" cy="8" r="3.1" />
      <path d="M8 1.4v1.4M8 13.2v1.4M1.4 8h1.4M13.2 8h1.4M3.3 3.3l1 1M11.7 11.7l1 1M12.7 3.3l-1 1M4.3 11.7l-1 1" strokeLinecap="round" />
    </svg>
  ),
  system: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <rect x="1.8" y="2.8" width="12.4" height="8" rx="1.6" />
      <path d="M8 10.8v2.4M5.4 13.4h5.2" strokeLinecap="round" />
    </svg>
  ),
  about: (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
      <circle cx="8" cy="8" r="6.2" />
      <path d="M8 7.2v4.2M8 4.9v.1" strokeLinecap="round" />
    </svg>
  ),
};

const PAGES: PageMeta[] = [
  { key: "account", labelKey: "settings.navAccount", titleKey: "settings.pageAccount", descKey: "settings.pageAccountDesc", icon: ICON.account, group: "account" },
  { key: "record", labelKey: "settings.navRecord", titleKey: "settings.pageRecord", descKey: "settings.pageRecordDesc", icon: ICON.record, group: "recording" },
  { key: "danmaku", labelKey: "settings.navDanmaku", titleKey: "settings.pageDanmaku", descKey: "settings.pageDanmakuDesc", icon: ICON.danmaku, group: "recording" },
  { key: "storage", labelKey: "settings.navStorage", titleKey: "settings.pageStorage", descKey: "settings.pageStorageDesc", icon: ICON.storage, group: "recording" },
  { key: "notify", labelKey: "settings.navNotify", titleKey: "settings.pageNotify", descKey: "settings.pageNotifyDesc", icon: ICON.notify, group: "app" },
  { key: "appearance", labelKey: "settings.navAppearance", titleKey: "settings.pageAppearance", descKey: "settings.pageAppearanceDesc", icon: ICON.appearance, group: "app" },
  { key: "system", labelKey: "settings.navSystem", titleKey: "settings.pageSystem", descKey: "settings.pageSystemDesc", icon: ICON.system, group: "app" },
  { key: "about", labelKey: "settings.navAbout", titleKey: "settings.pageAbout", descKey: "settings.pageAboutDesc", icon: ICON.about, group: "app" },
];

const NAV_GROUPS: { key: NavGroup; labelKey: string }[] = [
  { key: "account", labelKey: "settings.groupAccountNav" },
  { key: "recording", labelKey: "settings.groupRecordingNav" },
  { key: "app", labelKey: "settings.groupAppNav" },
];

function PageIcon({ page, size = 15, className }: { page: PageKey; size?: number; className?: string }) {
  return (
    <span
      aria-hidden
      className={cx("flex shrink-0 items-center justify-center text-current", className ?? "h-6 w-6")}
    >
      <span style={{ width: size, height: size }} className="[&_svg]:h-full [&_svg]:w-full">
        {ICON[page]}
      </span>
    </span>
  );
}

type SearchEntry = { page: PageKey; id: string; titleKey: string; descKey?: string; extra: string };

const SEARCH_INDEX: SearchEntry[] = [
  { page: "account", id: "set-account", titleKey: "settings.navAccount", descKey: "settings.pageAccountDesc", extra: "cookie 登录 扫码 qr login account" },
  { page: "record", id: "set-record-quality", titleKey: "settings.recordQuality", descKey: "settings.recordQualityHint", extra: "画质 清晰度 quality qn 最高 4K HDR" },
  { page: "record", id: "set-source-format", titleKey: "settings.sourceFormat", descKey: "settings.sourceFormatHint", extra: "源流 格式 flv hls m3u8 ts stream format" },
  { page: "record", id: "set-record-codec", titleKey: "settings.recordCodec", descKey: "settings.recordCodecHint", extra: "编码 codec h264 h265 hevc" },
  { page: "record", id: "set-split-title", titleKey: "settings.splitOnTitle", descKey: "settings.splitOnTitleHint", extra: "分割 标题 split title" },
  { page: "record", id: "set-split-duration", titleKey: "settings.splitMaxDuration", descKey: "settings.splitMaxDurationHint", extra: "分割 时长 duration split" },
  { page: "record", id: "set-split-size", titleKey: "settings.splitMaxSize", descKey: "settings.splitMaxSizeHint", extra: "分割 大小 体积 size split" },
  { page: "record", id: "set-view-quality", titleKey: "settings.viewQuality", descKey: "settings.viewQualityHint", extra: "观看 画质 quality qn view" },
  { page: "danmaku", id: "set-danmaku-master", titleKey: "settings.danmakuRecord", descKey: "settings.danmakuRecordHint", extra: "弹幕 录制 danmaku record chat" },
  { page: "danmaku", id: "set-danmaku-gift", titleKey: "settings.danmakuGift", descKey: "settings.danmakuGiftHint", extra: "礼物 gift 辣条 免费 连击" },
  { page: "danmaku", id: "set-danmaku-super-chat", titleKey: "settings.danmakuSuperChat", descKey: "settings.danmakuSuperChatHint", extra: "醒目留言 sc super chat 付费" },
  { page: "danmaku", id: "set-danmaku-guard", titleKey: "settings.danmakuGuard", descKey: "settings.danmakuGuardHint", extra: "舰长 提督 总督 guard 上舰" },
  { page: "record", id: "set-post-mode", titleKey: "settings.postMode", descKey: "settings.postModeHint", extra: "后处理 源封装 转换 自定义 source convert custom" },
    { page: "record", id: "set-post-convert-format", titleKey: "settings.postConvertFormat", descKey: "settings.postConvertFormatHint", extra: "转封装 mp4 mkv ts 容器" },
    { page: "record", id: "set-post-delete-source", titleKey: "settings.postDeleteSource", descKey: "settings.postDeleteSourceHint", extra: "删除源 回收站 source trash" },
    { page: "record", id: "set-post-custom-command", titleKey: "settings.postCustomCommand", descKey: "settings.postCustomCommandHint", extra: "自定义 ffmpeg 命令 custom command" },
    { page: "record", id: "set-post-custom-ext", titleKey: "settings.postCustomOutputExt", descKey: "settings.postCustomOutputExtHint", extra: "自定义 输出 扩展名 extension" },
    { page: "record", id: "set-post-custom-timeout", titleKey: "settings.postCustomTimeout", descKey: "settings.postCustomTimeoutHint", extra: "自定义 超时 timeout" },
    { page: "storage", id: "set-ffmpeg-path", titleKey: "settings.ffmpegPath", descKey: "settings.ffmpegPathHint", extra: "ffmpeg 工具 路径 tool path" },
    { page: "storage", id: "set-media-root", titleKey: "settings.mediaRoot", descKey: "settings.mediaRootHint", extra: "目录 路径 folder path" },
    { page: "storage", id: "set-log-dir", titleKey: "settings.logDir", descKey: "settings.logDirHint", extra: "日志 log 目录 folder path" },
    { page: "storage", id: "set-log-retention", titleKey: "settings.logRetention", descKey: "settings.logRetentionHint", extra: "日志 log 保留 清理 retention" },
  { page: "record", id: "set-monitor-interval", titleKey: "settings.monitorInterval", descKey: "settings.monitorIntervalHint", extra: "监控 间隔 轮询 monitor interval poll" },
  { page: "record", id: "set-monitor-backoff", titleKey: "settings.monitorBackoff", descKey: "settings.monitorBackoffHint", extra: "退避 限流 失败 backoff throttle" },
  { page: "storage", id: "set-filename-template", titleKey: "settings.filenameTemplate", descKey: "settings.filenameTemplateHint", extra: "文件名 模板 命名 filename template naming" },
  { page: "storage", id: "set-strip-emoji", titleKey: "settings.stripEmoji", descKey: "settings.stripEmojiHint", extra: "emoji 特殊符号 文件名 symbol" },
  { page: "storage", id: "set-group-dirs", titleKey: "settings.groupDirs", descKey: "settings.groupDirsHint", extra: "目录 分目录 平台 主播 folder group" },
  { page: "storage", id: "set-retention", titleKey: "settings.retention", descKey: "settings.retentionHint", extra: "清理 保留 容量 retention cleanup" },
  { page: "notify", id: "set-notify-record", titleKey: "settings.notifyRecord", descKey: "settings.notifyRecordHint", extra: "通知 录制开始 结束 notify" },
  { page: "notify", id: "set-notify-error", titleKey: "settings.notifyError", descKey: "settings.notifyErrorHint", extra: "通知 异常 失败 error" },
  { page: "notify", id: "set-notify-auth", titleKey: "settings.notifyAuth", descKey: "settings.notifyAuthHint", extra: "通知 登录 失效 cookie expired" },
  { page: "appearance", id: "set-theme", titleKey: "settings.theme", descKey: "settings.themeHint", extra: "主题 深色 浅色 theme dark light" },
  { page: "appearance", id: "set-language", titleKey: "settings.language", descKey: "settings.languageHint", extra: "语言 language english 中文" },
  { page: "system", id: "set-close-action", titleKey: "settings.closeAction", descKey: "settings.closeActionHint", extra: "关闭 托盘 退出 close tray quit" },
  { page: "system", id: "set-prevent-sleep", titleKey: "settings.preventSleep", descKey: "settings.preventSleepHint", extra: "休眠 睡眠 唤醒 sleep power wake" },
  { page: "about", id: "set-about", titleKey: "settings.pageAbout", descKey: "settings.aboutBuild", extra: "版本 version 关于 about 构建" },
];

const FIELD =
  "focus-ring h-9 w-full rounded-[7px] border border-line bg-surface px-3 text-[12.5px] text-ink transition-colors placeholder:text-faint hover:border-line-strong";
const GROUP = "text-[10px] font-semibold uppercase tracking-[0.12em] text-faint";
const HINT = "mt-1 block max-w-[520px] text-[11.5px] leading-[18px] text-faint";

function SelectField({
  value,
  onChange,
  options,
  className,
}: {
  value: number;
  onChange: (next: number) => void;
  options: Array<{ value: number; label: string }>;
  className?: string;
}) {
  return (
    <SelectMenu
      size="md"
      value={String(value)}
      onChange={(next) => onChange(Number(next))}
      options={options.map((option) => ({ value: String(option.value), label: option.label }))}
      className={cx("w-[208px] justify-between", className)}
    />
  );
}

function StringSelectField({
  value,
  onChange,
  options,
  className,
}: {
  value: string;
  onChange: (next: string) => void;
  options: Array<{ value: string; label: string }>;
  className?: string;
}) {
  return (
    <SelectMenu
      size="md"
      value={value}
      onChange={onChange}
      options={options}
      className={cx("w-[208px] justify-between", className)}
    />
  );
}

function Row({
  id,
  title,
  desc,
  tip,
  children,
  focus,
}: {
  id: string;
  title: string;
  desc?: string;
  tip?: string;
  children: ReactNode;
  focus: boolean;
}) {
  return (
    <div
      id={id}
      className={cx(
        "flex min-h-[58px] items-center gap-6 px-5 py-3.5 transition-colors",
        focus ? "bg-accent-soft" : "hover:bg-raised/45",
      )}
    >
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <div className="text-[12.5px] font-medium text-ink">{title}</div>
          {tip ? <InfoTip content={tip} /> : null}
        </div>
        {desc ? <p className={HINT}>{desc}</p> : null}
      </div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

function Section({ title, desc, children }: { title: string; desc?: string; children: ReactNode }) {
  return (
    <section className="space-y-2.5">
      <div className="px-0.5">
        <h3 className="text-[12.5px] font-semibold tracking-[-0.005em] text-ink">{title}</h3>
        {desc ? <p className="mt-1 max-w-[620px] text-[11.5px] leading-[17px] text-muted">{desc}</p> : null}
      </div>
      <div className="overflow-hidden rounded-[10px] border border-line bg-surface shadow-[0_1px_2px_rgba(16,24,40,0.04)]">
        <div className="divide-y divide-line">{children}</div>
      </div>
    </section>
  );
}

type Props = {
  initialPage?: PageKey;
  onSaved: (settings: Settings) => void;
  onDirtyChange?: (dirty: boolean, changedCount: number) => void;
  /** 把保存动作交给 App：离开设置时先默认保存，并记住保存前的值用于撤销。 */
  onRegisterSave?: (save: ((options?: { quiet?: boolean }) => Promise<Settings | null>) | null) => void;
  /** 顶栏改了主题/语言后，把新值同步进来，避免保存时用旧值覆盖。 */
  onRegisterSync?: (sync: ((patch: Partial<Settings>) => void) | null) => void;
};

export default function SettingsView({
  initialPage,
  onSaved,
  onDirtyChange,
  onRegisterSave,
  onRegisterSync,
}: Props) {
  const { t } = useTranslation();
  const [page, setPage] = useState<PageKey>(initialPage ?? "account");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [initial, setInitial] = useState("");
  const [saving, setSaving] = useState(false);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const notify = useNotify();
  const [query, setQuery] = useState("");
  const [focusId, setFocusId] = useState<string | null>(null);
  const [preview, setPreview] = useState<FilenamePreview | null>(null);
  const [logStatus, setLogStatus] = useState<LogStatus | null>(null);
  const [ffmpeg, setFfmpeg] = useState<FfmpegStatus | null>(null);
  const [trash, setTrash] = useState<TrashInfo | null>(null);
  // 分割体积在 UI 上允许 MB/GB 切换，落库统一存 MB。
  const [sizeUnit, setSizeUnit] = useState<"MB" | "GB">("MB");

  const changedCount = useMemo(() => {
    if (!settings || !initial) return 0;
    try {
      const base = JSON.parse(initial) as Record<string, unknown>;
      const current = settings as unknown as Record<string, unknown>;
      return Object.keys(current).filter(
        (key) => JSON.stringify(current[key]) !== JSON.stringify(base[key]),
      ).length;
    } catch {
      return 0;
    }
  }, [settings, initial]);
  const dirty = changedCount > 0;
  const reduceMotion = useReducedMotion();
  const isWindows = /Win/.test(navigator.platform || navigator.userAgent);

  // App 负责拿这个状态决定切页前是否要确认保存/放弃。
  useEffect(() => {
    onDirtyChange?.(dirty, changedCount);
  }, [changedCount, dirty, onDirtyChange]);

  useEffect(() => {
    if (!isTauri()) return;
    void appInfo()
      .then((loadedInfo) => setInfo(loadedInfo))
      .catch(() => setInfo(null));
  }, []);

  const ffmpegSourceLabel = useMemo(() => {
    if (!ffmpeg) return "";
    if (ffmpeg.source === "manual") return t("settings.ffmpegSourceManual");
    if (ffmpeg.source === "path") return t("settings.ffmpegSourcePath");
    if (ffmpeg.source === "candidate") return t("settings.ffmpegSourceCandidate");
    return t("settings.ffmpegSourceMissing");
  }, [ffmpeg, t]);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const loaded = await settingsGet();
      setSettings(loaded);
      setInitial(JSON.stringify(loaded));
    } catch (err) {
      notify.error(err);
    }
  }, [notify]);

  const loadFfmpeg = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setFfmpeg(await ffmpegStatus());
    } catch {
      setFfmpeg(null);
    }
  }, []);

  const loadLogs = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setLogStatus(await logsStatus());
    } catch {
      setLogStatus(null);
    }
  }, []);

  // 从「找不到 ffmpeg」横幅进入时直接落到存储页，省掉一次导航。
  useEffect(() => {
    if (initialPage) setPage(initialPage);
  }, [initialPage]);

  useEffect(() => {
    setQuery("");
    if (isTauri()) {
      void trashInfo()
        .then(setTrash)
        .catch(() => setTrash(null));
    }
    setFocusId(null);
    void load();
    void loadLogs();
    void loadFfmpeg();
    // 运行中日志目录可能失效并回落到默认目录；设置页开着时也要让状态行跟上。
    const timer = window.setInterval(() => void loadLogs(), 5000);
    return () => window.clearInterval(timer);
  }, [load, loadLogs, loadFfmpeg]);

  const patch = (next: Partial<Settings>) => {
    setSettings((current) => (current ? { ...current, ...next } : current));
  };

  // 顶栏是设置页之外的另一个写入口：它改主题/语言时，把新值同步到本地
  // settings 和 initial，否则保存会把顶栏刚写入的旧值覆盖回去。
  const syncExternal = useCallback((next: Partial<Settings>) => {
    setSettings((current) => (current ? { ...current, ...next } : current));
    setInitial((current) => {
      if (!current) return current;
      try {
        return JSON.stringify({ ...(JSON.parse(current) as Settings), ...next });
      } catch {
        return current;
      }
    });
  }, []);

  const savingRef = useRef<Promise<Settings | null> | null>(null);

  // 返回保存前的快照，App 用它做「撤销本次保存」；失败返回 null。
  const save = useCallback((options?: { quiet?: boolean }): Promise<Settings | null> => {
    const running = savingRef.current;
    if (running) return running;
    if (!settings || !initial) return Promise.resolve(null);
    // 撤销要回到「保存前的数据库值」，也就是脏状态基线 initial；settings 里已经是本次修改后的值。
    let previous: Settings;
    try {
      previous = JSON.parse(initial) as Settings;
    } catch {
      return Promise.resolve(null);
    }
    const task = (async () => {
      setSaving(true);
      try {
        const result = await settingsUpdate(settings);
        setSettings(result);
        setInitial(JSON.stringify(result));
        // 存活的是「偏好」而不是解析后的语言：选「跟随系统」后再次保存也要把 system 写进 localStorage，
        // 否则下次启动仍会读到上一次的具体语言。
        setLanguagePreference(result.language === "system" ? "system" : (result.language as Language));
        onSaved(result);
        void loadLogs();
        void loadFfmpeg();
        if (!options?.quiet) notify.success(t("settings.savedAll"));
        return previous;
      } catch (err) {
        notify.error(err);
        return null;
      } finally {
        setSaving(false);
      }
    })();
    savingRef.current = task;
    void task.finally(() => {
      if (savingRef.current === task) savingRef.current = null;
    });
    return task;
  }, [settings, initial, onSaved, loadLogs, loadFfmpeg, notify, t]);

  // 浮层里的「撤销」只回滚本地草稿，不写库；用户随后可以继续编辑或保存。
  const undoChanges = useCallback(() => {
    if (!initial) return;
    try {
      setSettings(JSON.parse(initial) as Settings);
    } catch {
      // initial 只由 JSON.stringify 写入，解析失败时保持现状即可。
    }
  }, [initial]);

  const saveItems = useMemo(
    () => [
      {
        id: "undo",
        label: t("settings.undo"),
        icon: <Undo2 size={14} strokeWidth={1.9} aria-hidden="true" />,
        onSelect: undoChanges,
      },
      {
        id: "save",
        label: t("settings.saveChanges"),
        icon: <Check size={14} strokeWidth={1.9} aria-hidden="true" />,
        onSelect: () => void save(),
      },
    ],
    [t, undoChanges, save],
  );

  useEffect(() => {
    onRegisterSave?.(save);
    return () => onRegisterSave?.(null);
  }, [onRegisterSave, save]);

  useEffect(() => {
    onRegisterSync?.(syncExternal);
    return () => onRegisterSync?.(null);
  }, [onRegisterSync, syncExternal]);

  useEffect(() => {
    if (!focusId) return;
    const node = document.getElementById(focusId);
    node?.scrollIntoView({ block: "center", behavior: "smooth" });
    const timer = window.setTimeout(() => setFocusId(null), 1600);
    return () => window.clearTimeout(timer);
  }, [focusId, page]);

  // 文件名预览跟着输入实时算；防抖 200ms，避免每敲一个字都过一次 IPC。
  useEffect(() => {
    if (!settings || !isTauri()) return;
    // 预览扩展名跟着后处理模式走，避免「保留源封装」时仍显示 .mp4。
    const ext =
      settings.postMode === "convert"
        ? settings.postConvertFormat
        : settings.postMode === "custom"
          ? settings.postCustomOutputExt
          : "flv";
    const timer = window.setTimeout(() => {
      void filenamePreview(
        settings.mediaFilenameTemplate,
        settings.mediaStripEmoji,
        settings.mediaGroupDirs,
        ext,
      )
        .then(setPreview)
        .catch(() => setPreview(null));
    }, 200);
    return () => window.clearTimeout(timer);
  }, [
    settings?.mediaFilenameTemplate,
    settings?.mediaStripEmoji,
    settings?.mediaGroupDirs,
    settings?.postMode,
    settings?.postConvertFormat,
    settings?.postCustomOutputExt,
  ]);

  const chooseFolder = async (target: "mediaRoot" | "logDir" = "mediaRoot") => {
    if (!settings) return;
    try {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const dir = await openDialog({
        directory: true,
        multiple: false,
        defaultPath:
          target === "mediaRoot"
            ? settings.mediaRoot
            : settings.logDir || logStatus?.defaultDir || settings.mediaRoot,
      });
      if (typeof dir === "string" && dir.length > 0) patch({ [target]: dir });
    } catch (err) {
      notify.error(err);
    }
  };

  // ffmpeg 是可执行文件，不是目录：Windows 只显示 .exe，macOS/Linux 不设过滤。
  const chooseFfmpeg = async () => {
    if (!settings) return;
    try {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const file = await openDialog({
        directory: false,
        multiple: false,
        title: t("settings.ffmpegChooseTitle"),
        defaultPath: settings.ffmpegPath || undefined,
        filters: isWindows ? [{ name: "ffmpeg", extensions: ["exe"] }] : undefined,
      });
      if (typeof file === "string" && file.length > 0) patch({ ffmpegPath: file });
    } catch (err) {
      notify.error(err);
    }
  };

  const openLogs = async () => {
    try {
      await logsOpen();
    } catch (err) {
      notify.error(err);
    }
  };

  const results = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return [];
    return SEARCH_INDEX.filter((entry) => {
      const hay = [t(entry.titleKey), entry.descKey ? t(entry.descKey) : "", entry.extra]
        .join(" ")
        .toLowerCase();
      return hay.includes(needle);
    });
  }, [query, t]);

  const renderBody = () => {
    if (!settings) return <div className="py-10 text-center text-[12px] text-muted">{t("common.loading")}</div>;

    if (query.trim()) {
      if (results.length === 0) {
        return (
          <div className="flex flex-col items-center gap-2 py-16 text-center">
            <span className="flex h-10 w-10 items-center justify-center rounded-full bg-raised text-faint">
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
                <circle cx="7" cy="7" r="4.2" />
                <path d="M10.2 10.2 13.6 13.6" strokeLinecap="round" />
              </svg>
            </span>
            <p className="text-[12.5px] font-medium text-ink">{t("settings.searchEmpty")}</p>
            <p className="text-[11px] text-faint">{t("settings.searchEmptyHint")}</p>
          </div>
        );
      }
      return (
        <div className="space-y-7">
          {PAGES.map((one) => {
            const hits = results.filter((entry) => entry.page === one.key);
            if (hits.length === 0) return null;
            return (
              <section key={one.key} className="space-y-2.5">
                <div className="flex items-center gap-2 px-0.5">
                  <PageIcon page={one.key} size={11} className="h-5 w-5 rounded-[5px]" />
                  <h2 className="text-[12px] font-semibold text-ink">{t(one.titleKey)}</h2>
                  <span className="rounded-full bg-raised px-1.5 py-0.5 font-mono text-[9.5px] tabular-nums text-faint">
                    {hits.length}
                  </span>
                </div>
                <div className="divide-y divide-line overflow-hidden rounded-[10px] border border-line bg-surface shadow-[0_1px_2px_rgba(16,24,40,0.04)]">
                  {hits.map((entry) => (
                    <button
                      key={entry.id}
                      type="button"
                      onClick={() => {
                        setPage(entry.page);
                        setQuery("");
                        setFocusId(entry.id);
                      }}
                      className="focus-ring group flex w-full items-center gap-3 px-5 py-3.5 text-left transition-colors hover:bg-raised/45"
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block text-[12.5px] font-medium text-ink">{t(entry.titleKey)}</span>
                        {entry.descKey ? (
                          <span className="mt-0.5 block truncate text-[11.5px] text-faint">{t(entry.descKey)}</span>
                        ) : null}
                      </span>
                      <span className="shrink-0 rounded-full border border-line px-2 py-0.5 text-[10px] text-faint">
                        {t(one.labelKey)}
                      </span>
                      <svg width="10" height="10" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden className="shrink-0 text-faint transition-transform group-hover:translate-x-0.5">
                        <path d="M4 2.5 7.5 6 4 9.5" strokeLinecap="round" strokeLinejoin="round" />
                      </svg>
                    </button>
                  ))}
                </div>
              </section>
            );
          })}
        </div>
      );
    }

    if (page === "account") {
      return <AccountSection />;
    }

    if (page === "danmaku") {
      const eventsDisabled = !settings.recordDanmaku;
      return (
        <div className="space-y-7">
          <Section title={t("settings.sectionDanmaku")} desc={t("settings.sectionDanmakuDesc")}>
            <Row
              id="set-danmaku-master"
              focus={focusId === "set-danmaku-master"}
              title={t("settings.danmakuRecord")}
              desc={t("settings.danmakuRecordHint")}
            >
              <Switch
                checked={settings.recordDanmaku}
                onChange={(next) => patch({ recordDanmaku: next })}
                label={t("settings.danmakuRecord")}
              />
            </Row>
          </Section>

          <Section
            title={t("settings.sectionDanmakuEvents")}
            desc={t("settings.sectionDanmakuEventsDesc")}
          >
            <Row
              id="set-danmaku-gift"
              focus={focusId === "set-danmaku-gift"}
              title={t("settings.danmakuGift")}
              desc={t("settings.danmakuGiftHint")}
            >
              <Switch
                checked={settings.recordDanmakuGift}
                onChange={(next) => patch({ recordDanmakuGift: next })}
                disabled={eventsDisabled}
                label={t("settings.danmakuGift")}
              />
            </Row>
            <Row
              id="set-danmaku-super-chat"
              focus={focusId === "set-danmaku-super-chat"}
              title={t("settings.danmakuSuperChat")}
              desc={t("settings.danmakuSuperChatHint")}
            >
              <Switch
                checked={settings.recordDanmakuSuperChat}
                onChange={(next) => patch({ recordDanmakuSuperChat: next })}
                disabled={eventsDisabled}
                label={t("settings.danmakuSuperChat")}
              />
            </Row>
            <Row
              id="set-danmaku-guard"
              focus={focusId === "set-danmaku-guard"}
              title={t("settings.danmakuGuard")}
              desc={t("settings.danmakuGuardHint")}
            >
              <Switch
                checked={settings.recordDanmakuGuard}
                onChange={(next) => patch({ recordDanmakuGuard: next })}
                disabled={eventsDisabled}
                label={t("settings.danmakuGuard")}
              />
            </Row>
            <div className="px-5 py-3 text-[10.5px] leading-4 text-faint">
              {t("settings.danmakuChatAlways")}
            </div>
          </Section>
        </div>
      );
    }

    if (page === "record") {
      return (
        <div className="space-y-7">
          <Section title={t("settings.sectionStream")} desc={t("settings.sectionStreamDesc")}>
            <Row
              id="set-record-quality"
              focus={focusId === "set-record-quality"}
              title={t("settings.recordQuality")}
              desc={t("settings.recordQualityHint")}
              tip={t("settings.recordQualityHint")}
            >
              <SelectField
                value={settings.defaultRecordQn}
                onChange={(next) => patch({ defaultRecordQn: next })}
                options={RECORD_QN_OPTIONS.map((option) => ({ value: option.value, label: t("settings." + option.key) }))}
              />
            </Row>
            <Row
              id="set-source-format"
              focus={focusId === "set-source-format"}
              title={t("settings.sourceFormat")}
              desc={t("settings.sourceFormatHint")}
              tip={t("settings.sourceFormatHint")}
            >
              <StringSelectField
                value={settings.recordSourceFormat}
                onChange={(next) => patch({ recordSourceFormat: next })}
                options={[
                  { value: "auto", label: t("settings.formatAuto") },
                  { value: "flv", label: t("settings.formatFlv") },
                  { value: "hls", label: t("settings.formatHls") },
                  { value: "ts", label: t("settings.formatTs") },
                ]}
              />
            </Row>
            <Row
              id="set-record-codec"
              focus={focusId === "set-record-codec"}
              title={t("settings.recordCodec")}
              desc={t("settings.recordCodecHint")}
              tip={t("settings.recordCodecHint")}
            >
              <StringSelectField
                value={settings.recordCodec}
                onChange={(next) => patch({ recordCodec: next })}
                options={[
                  { value: "auto", label: t("settings.codecAuto") },
                  { value: "h264", label: t("settings.codecH264") },
                  { value: "h265", label: t("settings.codecH265") },
                ]}
              />
            </Row>
          </Section>

          <Section title={t("settings.sectionViewQuality")}>
            <Row
              id="set-view-quality"
              focus={focusId === "set-view-quality"}
              title={t("settings.viewQuality")}
              desc={t("settings.viewQualityHint")}
              tip={t("settings.viewQualityHint")}
            >
              <SelectField
                value={settings.defaultViewQn}
                onChange={(next) => patch({ defaultViewQn: next })}
                options={QN_OPTIONS.map((option) => ({ value: option.value, label: t("settings." + option.key) }))}
              />
            </Row>
          </Section>

          <Section title={t("settings.sectionSplit")} desc={t("settings.sectionSplitDesc")}>
            <Row
              id="set-split-title"
              focus={focusId === "set-split-title"}
              title={t("settings.splitOnTitle")}
              desc={t("settings.splitOnTitleHint")}
              tip={t("settings.splitOnTitleHint")}
            >
              <Switch
                checked={settings.splitOnTitle}
                onChange={(next) => patch({ splitOnTitle: next })}
                label={t("settings.splitOnTitle")}
              />
            </Row>
            <Row
              id="set-split-duration"
              focus={focusId === "set-split-duration"}
              title={t("settings.splitMaxDuration")}
              desc={t("settings.splitMaxDurationHint")}
              tip={t("settings.splitMaxDurationHint")}
            >
              <div className="flex items-center gap-1.5">
                <div className="w-[100px]">
                  <input
                    type="number"
                    min={0}
                    max={86400}
                    value={settings.splitMaxDurationSec}
                    onChange={(event) =>
                      patch({ splitMaxDurationSec: Number(event.target.value) })
                    }
                    className={FIELD + " font-mono tabular-nums"}
                  />
                </div>
                <span className="whitespace-nowrap text-[11px] text-muted">
                  {t("settings.unitSeconds")}
                </span>
              </div>
            </Row>
            <Row
              id="set-split-size"
              focus={focusId === "set-split-size"}
              title={t("settings.splitMaxSize")}
              desc={t("settings.splitMaxSizeHint")}
              tip={t("settings.splitMaxSizeHint")}
            >
              <div className="flex items-center gap-1.5">
                <div className="w-[100px]">
                  <input
                    type="number"
                    min={0}
                    max={sizeUnit === "GB" ? 1024 : 1048576}
                    value={
                      sizeUnit === "GB"
                        ? Math.round((settings.splitMaxSizeMb / 1024) * 100) / 100
                        : settings.splitMaxSizeMb
                    }
                    onChange={(event) => {
                      const value = Number(event.target.value);
                      patch({
                        splitMaxSizeMb: Math.max(
                          0,
                          Math.round(value * (sizeUnit === "GB" ? 1024 : 1)),
                        ),
                      });
                    }}
                    className={FIELD + " font-mono tabular-nums"}
                  />
                </div>
                <div className="w-[92px]">
                  <SelectMenu
                    size="md"
                    value={sizeUnit}
                    onChange={(next) => setSizeUnit(next as "MB" | "GB")}
                    options={[
                      { value: "MB", label: t("settings.unitMB") },
                      { value: "GB", label: t("settings.unitGB") },
                    ]}
                    className="w-full justify-between"
                  />
                </div>
              </div>
            </Row>
          </Section>

          <Section title={t("settings.sectionMonitor")}>
            <Row
              id="set-monitor-interval"
              focus={focusId === "set-monitor-interval"}
              title={t("settings.monitorInterval")}
              desc={t("settings.monitorIntervalHint")}
              tip={t("settings.monitorIntervalHint")}
            >
              <div className="flex items-center gap-1.5">
                <div className="w-[88px]">
                  <input
                    type="number"
                    min={0}
                    max={86400}
                    value={settings.monitorIntervalSec}
                    onChange={(event) => patch({ monitorIntervalSec: Number(event.target.value) })}
                    className={FIELD + " font-mono tabular-nums"}
                  />
                </div>
                <span className="whitespace-nowrap text-[11px] text-muted">
                  {t("settings.unitSeconds")}
                </span>
              </div>
            </Row>
            <Row
              id="set-monitor-backoff"
              focus={focusId === "set-monitor-backoff"}
              title={t("settings.monitorBackoff")}
              desc={t("settings.monitorBackoffHint")}
              tip={t("settings.monitorBackoffHint")}
            >
              <Switch
                checked={settings.monitorBackoffEnabled}
                onChange={(next) => patch({ monitorBackoffEnabled: next })}
                label={t("settings.monitorBackoff")}
              />
            </Row>
            <Row
              id="set-monitor-backoff-cap"
              focus={focusId === "set-monitor-backoff-cap"}
              title={t("settings.monitorBackoffCap")}
              desc={t("settings.monitorBackoffCapHint")}
            >
              <div className="flex items-center gap-1.5">
                <div className="w-[88px]">
                  <input
                    type="number"
                    min={60}
                    max={3600}
                    disabled={!settings.monitorBackoffEnabled}
                    value={settings.monitorBackoffCapSec}
                    onChange={(event) => patch({ monitorBackoffCapSec: Number(event.target.value) })}
                    className={FIELD + " font-mono tabular-nums disabled:cursor-not-allowed disabled:opacity-50"}
                  />
                </div>
                <span className="whitespace-nowrap text-[11px] text-muted">
                  {t("settings.unitSeconds")}
                </span>
              </div>
            </Row>
          </Section>

          <Section title={t("settings.sectionPost")}>
            <div
              id="set-post-mode"
              className={cx(
                "px-5 py-4 transition-colors",
                focusId === "set-post-mode" ? "bg-accent-soft" : "",
              )}
            >
              <div className="flex items-center gap-1.5">
                <div className="text-[12.5px] font-medium text-ink">{t("settings.postMode")}</div>
                <InfoTip content={t("settings.postModeHint")} />
              </div>
              <p className={HINT}>{t("settings.postModeHint")}</p>
              <div role="radiogroup" aria-label={t("settings.postMode")} className="mt-3 grid gap-2 sm:grid-cols-3">
                {POST_MODES.map((mode) => {
                  const active = settings.postMode === mode.value;
                  return (
                    <button
                      key={mode.value}
                      type="button"
                      role="radio"
                      aria-checked={active}
                      onClick={() =>
                        patch({
                          postMode: mode.value,
                          ...(mode.value === "convert" ? {} : { postDeleteSource: false }),
                        })
                      }
                      className={cx(
                        "focus-ring rounded-[9px] border px-3 py-2.5 text-left transition-colors",
                        active
                          ? "border-accent bg-accent-soft text-ink shadow-[0_0_0_1px_var(--accent)]"
                          : "border-line bg-surface text-ink-soft hover:border-line-strong hover:bg-raised/50",
                      )}
                    >
                      <span className="flex items-center gap-2 text-[12px] font-medium">
                        <span
                          aria-hidden
                          className={cx(
                            "flex h-[14px] w-[14px] shrink-0 items-center justify-center rounded-[4px] border",
                            active ? "border-accent bg-accent text-accent-ink" : "border-line-strong bg-surface",
                          )}
                        >
                          {active ? (
                            <svg width="9" height="9" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
                              <path d="m2 5.2 1.9 1.9L8 3" strokeLinecap="round" strokeLinejoin="round" />
                            </svg>
                          ) : null}
                        </span>
                        {t(mode.labelKey)}
                      </span>
                      <span className="mt-1.5 block pl-[21px] text-[10.5px] leading-[15px] text-faint">
                        {t(mode.descKey)}
                      </span>
                    </button>
                  );
                })}
              </div>
            </div>
            <Row
              id="set-post-convert-format"
              focus={focusId === "set-post-convert-format"}
              title={t("settings.postConvertFormat")}
              desc={t("settings.postConvertFormatHint")}
            >
              <StringSelectField
                value={settings.postConvertFormat}
                onChange={(next) => patch({ postConvertFormat: next })}
                options={[
                  { value: "mp4", label: "MP4" },
                  { value: "mkv", label: "MKV" },
                  { value: "ts", label: "TS" },
                ]}
              />
            </Row>
            <Row
              id="set-post-delete-source"
              focus={focusId === "set-post-delete-source"}
              title={t("settings.postDeleteSource")}
              desc={t("settings.postDeleteSourceHint")}
            >
              <Switch
                checked={settings.postDeleteSource}
                onChange={(next) => patch({ postDeleteSource: next })}
                disabled={settings.postMode !== "convert"}
                label={t("settings.postDeleteSource")}
              />
            </Row>
            <Row
              id="set-post-custom-command"
              focus={focusId === "set-post-custom-command"}
              title={t("settings.postCustomCommand")}
              desc={t("settings.postCustomCommandHint")}
              tip={t("settings.postCustomCommandHint")}
            >
              <input
                value={settings.postCustomCommand}
                onChange={(event) => patch({ postCustomCommand: event.target.value })}
                disabled={settings.postMode !== "custom"}
                placeholder="-i {input} -c copy {output}"
                spellCheck={false}
                className={FIELD + " w-[420px] font-mono text-[11px] disabled:cursor-not-allowed disabled:opacity-50"}
              />
            </Row>
            <Row
              id="set-post-custom-ext"
              focus={focusId === "set-post-custom-ext"}
              title={t("settings.postCustomOutputExt")}
              desc={t("settings.postCustomOutputExtHint")}
            >
              <input
                value={settings.postCustomOutputExt}
                onChange={(event) => patch({ postCustomOutputExt: event.target.value })}
                disabled={settings.postMode !== "custom"}
                placeholder="mp4"
                spellCheck={false}
                className={FIELD + " w-[88px] font-mono text-[11px] disabled:cursor-not-allowed disabled:opacity-50"}
              />
            </Row>
            <Row
              id="set-post-custom-timeout"
              focus={focusId === "set-post-custom-timeout"}
              title={t("settings.postCustomTimeout")}
              desc={t("settings.postCustomTimeoutHint")}
            >
              <div className="flex items-center gap-1.5">
                <div className="w-[88px]">
                  <input
                    type="number"
                    min={10}
                    max={86400}
                    disabled={settings.postMode !== "custom"}
                    value={settings.postCustomTimeoutSec}
                    onChange={(event) =>
                      patch({ postCustomTimeoutSec: Number(event.target.value) })
                    }
                    className={FIELD + " font-mono tabular-nums disabled:cursor-not-allowed disabled:opacity-50"}
                  />
                </div>
                <span className="whitespace-nowrap text-[11px] text-muted">
                  {t("settings.unitSeconds")}
                </span>
              </div>
            </Row>
          </Section>
        </div>
      );
    }

    if (page === "storage") {
      return (
        <div className="space-y-7">
          <Section title={t("settings.sectionTools")}>
            <Row
              id="set-ffmpeg-path"
              focus={focusId === "set-ffmpeg-path"}
              title={t("settings.ffmpegPath")}
              desc={t("settings.ffmpegPathHint")}
            >
              <div className="flex w-[480px] items-center gap-1.5">
                <input
                  value={settings.ffmpegPath}
                  onChange={(event) => patch({ ffmpegPath: event.target.value })}
                  placeholder={t("settings.ffmpegAuto")}
                  className={FIELD + " font-mono text-[11px]"}
                  spellCheck={false}
                />
                <Button size="sm" variant="secondary" onClick={() => void chooseFfmpeg()}>
                  {t("settings.ffmpegChoose")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => patch({ ffmpegPath: "" })}>
                  {t("settings.ffmpegReset")}
                </Button>
              </div>
            </Row>
            <div className="space-y-0.5 px-5 pb-4 text-[10.5px] leading-4 text-faint">
              {ffmpeg ? (
                <>
                  {ffmpeg.path ? (
                    <p className="break-all font-mono">
                      {t("settings.ffmpegActual", { path: ffmpeg.path })}
                    </p>
                  ) : (
                    <p className="text-rec">{t("settings.ffmpegMissing")}</p>
                  )}
                  <p>
                    {ffmpegSourceLabel}
                    {ffmpeg.version ? " · " + t("settings.ffmpegVersion", { version: ffmpeg.version }) : ""}
                  </p>
                  {ffmpeg.configured && ffmpeg.source !== "manual" ? (
                    <p className="break-all text-rec">
                      {t("settings.ffmpegFallback", { path: ffmpeg.configured })}
                    </p>
                  ) : null}
                </>
              ) : (
                <p>{t("settings.ffmpegUnavailable")}</p>
              )}
            </div>
          </Section>

          <Section title={t("settings.sectionDirectory")}>
            <Row
              id="set-media-root"
              focus={focusId === "set-media-root"}
              title={t("settings.mediaRoot")}
              desc={t("settings.mediaRootHint")}
            >
              <div className="flex w-[420px] items-center gap-1.5">
                <input
                  value={settings.mediaRoot}
                  onChange={(event) => patch({ mediaRoot: event.target.value })}
                  className={FIELD + " font-mono text-[11px]"}
                  spellCheck={false}
                />
                <Button size="sm" variant="secondary" onClick={() => void chooseFolder("mediaRoot")}>
                  {t("settings.choose")}
                </Button>
              </div>
            </Row>
          </Section>

          <Section title={t("settings.sectionLogs")}>
            <Row
              id="set-log-dir"
              focus={focusId === "set-log-dir"}
              title={t("settings.logDir")}
              desc={t("settings.logDirHint")}
            >
              <div className="flex w-[420px] flex-col gap-1.5">
                <div className="flex items-center gap-1.5">
                  <input
                    value={settings.logDir}
                    onChange={(event) => patch({ logDir: event.target.value })}
                    placeholder={logStatus?.defaultDir ?? ""}
                    className={FIELD + " font-mono text-[11px]"}
                    spellCheck={false}
                  />
                  <Button size="sm" variant="secondary" onClick={() => void chooseFolder("logDir")}>
                    {t("settings.choose")}
                  </Button>
                </div>
                <div className="flex items-center gap-1.5">
                  <Button size="sm" variant="ghost" onClick={() => void openLogs()}>
                    {t("settings.logOpen")}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => patch({ logDir: "" })}>
                    {t("settings.logReset")}
                  </Button>
                </div>
              </div>
            </Row>
            <div className="space-y-0.5 px-5 pb-4 text-[10.5px] leading-4 text-faint">
              {logStatus ? (
                <>
                  <p className="break-all font-mono">
                    {t("settings.logActual", { dir: logStatus.dir })}
                  </p>
                  <p>
                    {t("settings.logFiles", {
                      n: logStatus.fileCount,
                      size: formatBytes(Number(logStatus.totalBytes)),
                    })}
                    {logStatus.currentFile
                      ? " · " + t("settings.logCurrent", { file: logStatus.currentFile })
                      : ""}
                  </p>
                  {logStatus.dropped > 0 ? (
                    <p className="text-rec">
                      {t("settings.logDropped", { n: logStatus.dropped })}
                    </p>
                  ) : null}
                  {logStatus.fallback ? (
                    <p className="break-all text-rec">
                      {t("settings.logFallback", { detail: logStatus.fallback })}
                    </p>
                  ) : null}
                  {logStatus.lastError ? (
                    <p className="break-all text-rec">
                      {t("settings.logError", { detail: logStatus.lastError })}
                    </p>
                  ) : null}
                </>
              ) : (
                <p>{t("settings.logUnavailable")}</p>
              )}
            </div>
            <Row
              id="set-log-retention"
              focus={focusId === "set-log-retention"}
              title={t("settings.logRetention")}
              desc={t("settings.logRetentionHint")}
            >
              <label className="flex items-center gap-2 whitespace-nowrap text-[11px] text-muted">
                {t("settings.retentionDays")}
                <div className="w-[72px]">
                  <input
                    type="number"
                    min={0}
                    max={3650}
                    value={settings.logRetentionDays}
                    onChange={(event) => patch({ logRetentionDays: Number(event.target.value) })}
                    className={FIELD + " font-mono tabular-nums"}
                  />
                </div>
                <span>{t("settings.unitDays")}</span>
              </label>
            </Row>
          </Section>

          <Section title={t("settings.sectionNaming")}>
            <Row
              id="set-filename-template"
              focus={focusId === "set-filename-template"}
              title={t("settings.filenameTemplate")}
              desc={t("settings.filenameTemplateHint")}
            >
              <div className="w-[420px]">
                <input
                  value={settings.mediaFilenameTemplate}
                  onChange={(event) => patch({ mediaFilenameTemplate: event.target.value })}
                  className={FIELD + " font-mono text-[11px]"}
                  spellCheck={false}
                />
              </div>
            </Row>
            <div className="px-5 pb-4">
              <p className="text-[10.5px] leading-4 text-faint">{t("settings.filenameVars")}</p>
              {preview && settings.mediaFilenameTemplate.trim() ? (
                preview.valid ? (
                  <p className="mt-1 break-all font-mono text-[10.5px] text-muted">{preview.path}</p>
                ) : (
                  <p className="mt-1 text-[10.5px] text-rec">{preview.error}</p>
                )
              ) : null}
            </div>
            <Row
              id="set-strip-emoji"
              focus={focusId === "set-strip-emoji"}
              title={t("settings.stripEmoji")}
              desc={t("settings.stripEmojiHint")}
            >
              <Switch
                checked={settings.mediaStripEmoji}
                onChange={(next) => patch({ mediaStripEmoji: next })}
                label={t("settings.stripEmoji")}
              />
            </Row>
            <Row
              id="set-group-dirs"
              focus={focusId === "set-group-dirs"}
              title={t("settings.groupDirs")}
              desc={t("settings.groupDirsHint")}
            >
              <Switch
                checked={settings.mediaGroupDirs}
                onChange={(next) => patch({ mediaGroupDirs: next })}
                label={t("settings.groupDirs")}
              />
            </Row>
          </Section>

          <Section title={t("settings.sectionTrash")}>
            <Row
              id="set-trash"
              focus={focusId === "set-trash"}
              title={t("settings.trashTitle")}
              desc={t("settings.trashHint")}
            >
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  void trashOpen().catch((err) => notify.error(err));
                }}
              >
                {t("settings.trashOpen")}
              </Button>
            </Row>
            <div className="space-y-0.5 px-5 pb-4 text-[10.5px] leading-4 text-faint">
              {trash ? (
                <>
                  <p className="break-all font-mono">
                    {trash.platform === "windows" ? t("settings.trashLocationWindows") : trash.location}
                  </p>
                  <p className={trash.canRestore ? "" : "text-warn"}>
                    {trash.canRestore ? t("settings.trashRestoreYes") : t("settings.trashRestoreNo")}
                  </p>
                  <p>{t("settings.trashNote")}</p>
                </>
              ) : (
                <p>{t("settings.trashUnavailable")}</p>
              )}
            </div>
          </Section>

          <Section title={t("settings.sectionRetention")}>
            <Row
              id="set-retention"
              focus={focusId === "set-retention"}
              title={t("settings.retention")}
              desc={t("settings.retentionHint")}
              tip={t("settings.retentionHint")}
            >
              <div className="flex items-center gap-3">
                <label className="flex items-center gap-2 whitespace-nowrap text-[11px] text-muted">
                  {t("settings.retentionDays")}
                  <div className="w-[72px]">
                    <input
                      type="number"
                      min={0}
                      max={3650}
                      value={settings.retentionDays}
                      onChange={(event) => patch({ retentionDays: Number(event.target.value) })}
                      className={FIELD + " font-mono tabular-nums"}
                    />
                  </div>
                  <span>{t("settings.unitDays")}</span>
                </label>
                <label className="flex items-center gap-2 whitespace-nowrap text-[11px] text-muted">
                  {t("settings.retentionGb")}
                  <div className="w-[72px]">
                    <input
                      type="number"
                      min={0}
                      max={100000}
                      value={settings.maxTotalGb}
                      onChange={(event) => patch({ maxTotalGb: Number(event.target.value) })}
                      className={FIELD + " font-mono tabular-nums"}
                    />
                  </div>
                  <span>{t("settings.unitGb")}</span>
                </label>
              </div>
            </Row>
          </Section>
        </div>
      );
    }

    if (page === "notify") {
      return (
        <Section title={t("settings.sectionEvents")}>
          <Row
            id="set-notify-record"
            focus={focusId === "set-notify-record"}
            title={t("settings.notifyRecord")}
            desc={t("settings.notifyRecordHint")}
          >
            <Switch
              checked={settings.notifyEnabled}
              onChange={(next) => patch({ notifyEnabled: next })}
              label={t("settings.notifyRecord")}
            />
          </Row>
          <Row
            id="set-notify-error"
            focus={focusId === "set-notify-error"}
            title={t("settings.notifyError")}
            desc={t("settings.notifyErrorHint")}
          >
            <Switch
              checked={settings.notifyError}
              onChange={(next) => patch({ notifyError: next })}
              label={t("settings.notifyError")}
            />
          </Row>
          <Row
            id="set-notify-auth"
            focus={focusId === "set-notify-auth"}
            title={t("settings.notifyAuth")}
            desc={t("settings.notifyAuthHint")}
          >
            <Switch
              checked={settings.notifyAuth}
              onChange={(next) => patch({ notifyAuth: next })}
              label={t("settings.notifyAuth")}
            />
          </Row>
        </Section>
      );
    }

    if (page === "appearance") {
      return (
        <Section title={t("settings.sectionInterface")}>
          <Row
            id="set-theme"
            focus={focusId === "set-theme"}
            title={t("settings.theme")}
            desc={t("settings.themeHint")}
          >
            <Segmented
              items={[
                { key: "light", label: t("settings.themeLight") },
                { key: "dark", label: t("settings.themeDark") },
                { key: "system", label: t("settings.themeSystem") },
              ]}
              value={(settings.theme as "light" | "dark" | "system") ?? "light"}
              onChange={(next) => patch({ theme: next })}
            />
          </Row>
          <Row
            id="set-language"
            focus={focusId === "set-language"}
            title={t("settings.language")}
            desc={t("settings.languageHint")}
          >
            <Segmented
              items={[
                { key: "system", label: t("settings.languageSystem") },
                { key: "zh-CN", label: "中文" },
                { key: "en-US", label: "English" },
              ]}
              value={settings.language}
              onChange={(next) => patch({ language: next })}
            />
          </Row>
        </Section>
      );
    }

    if (page === "system") {
      return (
        <div className="space-y-7">
          <Section title={t("settings.sectionClose")}>
            <Row
              id="set-close-action"
              focus={focusId === "set-close-action"}
              title={t("settings.closeAction")}
              desc={t("settings.closeActionHint")}
            >
              <Segmented
                items={[
                  { key: "tray", label: t("settings.closeTray") },
                  { key: "quit", label: t("settings.closeQuit") },
                ]}
                value={settings.closeAction === "quit" ? "quit" : "tray"}
                onChange={(next) => patch({ closeAction: next })}
              />
            </Row>
            <div className="px-4 py-3">
              <p className="text-[11px] leading-4 text-faint">{t("settings.trayHint")}</p>
            </div>
          </Section>

          <Section title={t("settings.sectionPower")}>
            <Row
              id="set-prevent-sleep"
              focus={focusId === "set-prevent-sleep"}
              title={t("settings.preventSleep")}
              desc={t("settings.preventSleepHint")}
            >
              <Switch
                checked={settings.preventSleep}
                onChange={(next) => patch({ preventSleep: next })}
                label={t("settings.preventSleep")}
              />
            </Row>
          </Section>
        </div>
      );
    }

    return (
      <AboutSection
        info={info}
        ffmpeg={ffmpeg}
        highlighted={focusId === "set-about"}
      />
    );
  };

  return (
    <div className="relative flex min-h-0 flex-1 bg-canvas text-ink">
      <aside className="flex w-[220px] shrink-0 flex-col border-r border-line/80 bg-canvas">
        <div className="shrink-0 px-3 pb-2 pt-4">
          <SearchField
            label={t("settings.searchSettings")}
            hideLabel
            value={query}
            onValueChange={setQuery}
            onKeyDown={(event) => {
              if (event.key === "Escape" && query) {
                event.stopPropagation();
                setQuery("");
              }
            }}
            placeholder={t("settings.searchSettings")}
            spellCheck={false}
            clearLabel={t("settings.searchClear")}
          />
        </div>

        <LayoutGroup id="settings-nav">
          <nav aria-label={t("settings.title")} className="min-h-0 flex-1 overflow-auto px-3 pb-4">
            {NAV_GROUPS.map((group) => {
              const pages = PAGES.filter((one) => one.group === group.key);
              if (pages.length === 0) return null;
              return (
                <div key={group.key} className="pt-4 first:pt-2.5">
                  <p className={GROUP + " px-2.5 pb-1.5"}>{t(group.labelKey)}</p>
                  <div className="space-y-0.5">
                    {pages.map((one) => {
                      const active = one.key === page && !query.trim();
                      return (
                        <button
                          key={one.key}
                          type="button"
                          onClick={() => {
                            setPage(one.key);
                            setQuery("");
                          }}
                          aria-current={active ? "page" : undefined}
                          className={cx(
                            "focus-ring relative flex h-8 w-full items-center gap-2.5 rounded-[7px] px-2.5 text-left text-[12.5px] transition-colors",
                            active ? "font-medium text-ink" : "text-muted hover:text-ink",
                          )}
                        >
                          {active ? (
                            <motion.span
                              layoutId="settings-nav-active"
                              className="absolute inset-0 rounded-[7px] border border-line bg-surface shadow-[0_1px_2px_rgba(16,24,40,0.06)]"
                              transition={
                                reduceMotion
                                  ? { duration: 0 }
                                  : { type: "spring", stiffness: 520, damping: 42 }
                              }
                            />
                          ) : null}
                          <PageIcon page={one.key} size={14} className="relative z-10 h-4 w-4" />
                          <span className="relative z-10 truncate">{t(one.labelKey)}</span>
                        </button>
                      );
                    })}
                  </div>
                </div>
              );
            })}
          </nav>
        </LayoutGroup>

      </aside>

      <main className="flex min-w-0 flex-1 flex-col bg-surface">
        <div className="min-h-0 flex-1 overflow-auto px-8 py-6">
          <div className={cx("mx-auto w-full max-w-[780px]", dirty ? "pb-24" : "pb-6")}>
            <div className="space-y-7">{renderBody()}</div>
          </div>
        </div>
      </main>

      {/* 有未保存修改时，保存/撤销浮在内容之上，不占布局；离开设置时同一套保存函数由 App 调用。 */}
      <AnimatePresence>
        {dirty ? (
          <motion.div
            key="settings-save-dock"
            initial={reduceMotion ? false : { opacity: 0, y: 8 }}
            animate={{ opacity: 1, y: 0 }}
            exit={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 8 }}
            transition={reduceMotion ? { duration: 0 } : { type: "spring", stiffness: 480, damping: 38 }}
            className="pointer-events-none absolute inset-x-0 bottom-5 z-[60] flex justify-center px-5"
          >
            <div className="pointer-events-auto max-w-[calc(100vw-40px)]">
              <ButtonGroup
                items={saveItems}
                label={t("settings.pendingChanges")}
                size="sm"
                disabled={saving}
                className="shadow-[var(--shadow-floating)]"
              />
            </div>
          </motion.div>
        ) : null}
      </AnimatePresence>
    </div>
  );
}
