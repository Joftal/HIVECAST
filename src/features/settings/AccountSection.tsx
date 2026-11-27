import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertCircle, Lock, QrCode, RefreshCw, Search, X } from "lucide-react";
import {
  bilibiliQrPoll,
  bilibiliQrStart,
  bilibiliRefreshNow,
  credentialDelete,
  credentialSave,
  credentialsList,
  describeError,
  douyinRefreshNow,
  douyuQrPoll,
  douyuQrStart,
  douyuRefreshNow,
  huyaQrCancel,
  huyaQrPoll,
  huyaQrStart,
  huyaRefreshNow,
  isTauri,
  onAccountRefresh,
  onWindowVisibility,
  type CredentialInfo,
  type QrPoll,
  type QrTicket,
  type RefreshStatus,
} from "../../lib/ipc";
import { formatCountdown, useSecondsLeft } from "../../lib/countdown";
import { Button, cx } from "../../components/ui/Controls";
import { Dialog, DialogContent } from "../../components/arc/dialog/dialog";
import {
  PlatformBadge,
  getPlatformVisual,
  platformBrandStyle,
} from "../../components/ui/PlatformBadge";
import { useNotify } from "../../lib/notify";

type Platform = "bilibili" | "douyu" | "huya" | "douyin";
type FilterKey = "all" | "connected" | "expired" | "disconnected";
type AuthTab = "qr" | "cookie";

type PlatformSpec = {
  id: Platform;
  supportsQr: boolean;
  hintKey: string;
  qrTitleKey?: string;
  qrHintKey?: string;
  autoRefreshOnKey: string;
  noRefreshKey: string;
  refreshNow: () => Promise<RefreshStatus>;
  qrStart?: () => Promise<QrTicket>;
  qrPoll?: (key: string) => Promise<QrPoll>;
  qrCancel?: (key: string) => Promise<void>;
};

const PLATFORM_SPECS: readonly PlatformSpec[] = [
  {
    id: "bilibili",
    supportsQr: true,
    hintKey: "account.bilibiliHint",
    qrTitleKey: "account.qrTitle",
    qrHintKey: "account.qrHint",
    autoRefreshOnKey: "account.autoRefreshOn",
    noRefreshKey: "account.autoRefreshOff",
    refreshNow: bilibiliRefreshNow,
    qrStart: bilibiliQrStart,
    qrPoll: bilibiliQrPoll,
  },
  {
    id: "douyu",
    supportsQr: true,
    hintKey: "account.douyuHint",
    qrTitleKey: "account.qrTitleDouyu",
    qrHintKey: "account.qrHintDouyu",
    autoRefreshOnKey: "account.autoRefreshOnDouyu",
    noRefreshKey: "account.douyuNoRefresh",
    refreshNow: douyuRefreshNow,
    qrStart: douyuQrStart,
    qrPoll: douyuQrPoll,
  },
  {
    id: "huya",
    supportsQr: true,
    hintKey: "account.huyaHint",
    qrTitleKey: "account.qrTitleHuya",
    qrHintKey: "account.qrHintHuya",
    autoRefreshOnKey: "account.autoRefreshOnHuya",
    noRefreshKey: "account.huyaNoRefresh",
    refreshNow: huyaRefreshNow,
    qrStart: huyaQrStart,
    qrPoll: huyaQrPoll,
    qrCancel: huyaQrCancel,
  },
  {
    id: "douyin",
    supportsQr: false,
    hintKey: "account.douyinHint",
    autoRefreshOnKey: "account.autoRefreshOnDouyin",
    noRefreshKey: "account.douyinNoRefresh",
    refreshNow: douyinRefreshNow,
  },
];

const SPEC_MAP: Record<Platform, PlatformSpec> = Object.fromEntries(
  PLATFORM_SPECS.map((spec) => [spec.id, spec]),
) as Record<Platform, PlatformSpec>;

const REFRESH_OUTCOME_KEY: Record<string, string> = {
  never: "account.refreshOutcome.never",
  fresh: "account.refreshOutcome.fresh",
  refreshed: "account.refreshOutcome.refreshed",
  confirm_failed: "account.refreshOutcome.confirmFailed",
  failed: "account.refreshOutcome.failed",
  not_refreshable: "account.refreshOutcome.notRefreshable",
  discarded: "account.refreshOutcome.discarded",
  key_stale: "account.refreshOutcome.keyStale",
};

type QrState = {
  platform: Platform;
  ticket: QrTicket;
  poll: QrPoll | null;
  error: string | null;
  /** 失效时刻（绝对毫秒）；每次轮询用服务端剩余秒数重新对齐。 */
  expiresAt: number;
};

function isRefreshWarning(status: RefreshStatus): boolean {
  return (
    status.keyStale ||
    status.outcome === "failed" ||
    status.outcome === "confirm_failed" ||
    status.outcome === "not_refreshable" ||
    status.outcome === "discarded"
  );
}

export default function AccountSection() {
  const { t, i18n } = useTranslation();
  const notify = useNotify();

  const [items, setItems] = useState<CredentialInfo[] | null>(null);
  const [filter, setFilter] = useState<FilterKey>("all");
  const [query, setQuery] = useState("");

  // 登录/更新弹窗状态：modalOpen 与 modalPlatform 拆分，确保弹窗退场动画完整播放。
  const [modalOpen, setModalOpen] = useState(false);
  const [modalPlatform, setModalPlatform] = useState<Platform | null>(null);
  const [authTab, setAuthTab] = useState<AuthTab>("qr");
  const [draft, setDraft] = useState("");
  const [savingCookie, setSavingCookie] = useState(false);
  const [qrLoading, setQrLoading] = useState(false);
  const [qrStartError, setQrStartError] = useState<string | null>(null);
  const [qr, setQr] = useState<QrState | null>(null);

  // 单平台独立的检查与退出状态
  const [checkingPlatforms, setCheckingPlatforms] = useState<Record<string, boolean>>({});
  const [checkingAll, setCheckingAll] = useState(false);
  const [loggingOut, setLoggingOut] = useState<Platform | null>(null);
  const [confirming, setConfirming] = useState<Platform | null>(null);

  /** 每次打开/关闭/刷新二维码或组件卸载递增，丢弃迟到的 generate/poll 响应。 */
  const qrSeq = useRef(0);
  /** 记录已通知后端取消过的 qrcodeKey，确保每个虎牙 session 只取消一次且无重复 IPC。 */
  const cancelledKeysRef = useRef<Set<string>>(new Set());
  const checkingSetRef = useRef<Set<Platform>>(new Set());
  const checkingAllRef = useRef(false);

  // 组件卸载时递增 qrSeq，使仍在途的 qrStart() 返回后自动释放后端 session 并丢弃状态更新。
  useEffect(() => {
    return () => {
      qrSeq.current += 1;
    };
  }, []);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setItems(await credentialsList());
    } catch (err) {
      notify.error(err);
    }
  }, [notify]);

  useEffect(() => {
    void load();
  }, [load]);

  // 续期检查可能耗时十几秒，且可能发生在设置页之外。后端写完状态后发事件，前端据此重拉。
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void onAccountRefresh(() => {
      if (!disposed) void load();
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [load]);

  const credMap = useMemo(() => {
    const map = new Map<string, CredentialInfo>();
    for (const item of items ?? []) {
      map.set(item.platform, item);
    }
    return map;
  }, [items]);

  const refreshOutcomeLabel = useCallback(
    (status: RefreshStatus) => {
      if (status.keyStale) return t("account.refreshOutcome.keyStale");
      return t(REFRESH_OUTCOME_KEY[status.outcome] ?? "account.refreshOutcome.failed");
    },
    [t],
  );

  const refreshTime = useCallback(
    (value: number) =>
      new Date(value * 1000).toLocaleString(i18n.resolvedLanguage ?? undefined, {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      }),
    [i18n.resolvedLanguage],
  );

  const cancelPlatformQr = useCallback((platform: Platform | undefined, key: string | undefined) => {
    if (!platform || !key) return;
    const cancel = SPEC_MAP[platform]?.qrCancel;
    if (!cancel) return;
    if (cancelledKeysRef.current.has(key)) return;
    cancelledKeysRef.current.add(key);
    void cancel(key).catch(() => {});
  }, []);

  const stopQr = useCallback(() => {
    qrSeq.current += 1;
    // 清理后端 session 由 qr 的 useEffect cleanup 统一负责，避免在 setState updater 内触发副作用或重复取消。
    setQr(null);
    setQrStartError(null);
    setQrLoading(false);
  }, []);

  const startQr = useCallback(
    async (platform: Platform) => {
      const spec = SPEC_MAP[platform];
      if (!spec?.qrStart) return;
      const seq = ++qrSeq.current;
      setQr(null);
      setQrStartError(null);
      setQrLoading(true);
      try {
        const ticket = await spec.qrStart();
        if (seq !== qrSeq.current) {
          cancelPlatformQr(platform, ticket.qrcodeKey);
          return;
        }
        cancelledKeysRef.current.delete(ticket.qrcodeKey);
        setQr({
          platform,
          ticket,
          poll: null,
          error: null,
          expiresAt: Date.now() + Math.max(1, ticket.expiresIn) * 1000,
        });
      } catch (err) {
        if (seq === qrSeq.current) {
          const message = describeError(err);
          setQrStartError(message);
          notify.error(err);
        }
      } finally {
        if (seq === qrSeq.current) setQrLoading(false);
      }
    },
    [cancelPlatformQr, notify],
  );

  const openAuthModal = useCallback(
    (platform: Platform, preferredTab?: AuthTab) => {
      const spec = SPEC_MAP[platform];
      const nextTab: AuthTab =
        preferredTab && (preferredTab === "cookie" || spec.supportsQr)
          ? preferredTab
          : spec.supportsQr
            ? "qr"
            : "cookie";
      setConfirming(null);
      setModalPlatform(platform);
      setModalOpen(true);
      setAuthTab(nextTab);
      setDraft("");
      if (nextTab === "qr" && spec.supportsQr) {
        void startQr(platform);
      } else {
        stopQr();
      }
    },
    [startQr, stopQr],
  );

  const closeAuthModal = useCallback(() => {
    stopQr();
    setModalOpen(false);
    setDraft("");
    setSavingCookie(false);
  }, [stopQr]);

  const switchAuthTab = useCallback(
    (nextTab: AuthTab) => {
      if (!modalPlatform || nextTab === authTab) return;
      const spec = SPEC_MAP[modalPlatform];
      if (nextTab === "qr" && !spec.supportsQr) return;
      setAuthTab(nextTab);
      if (nextTab === "qr") {
        void startQr(modalPlatform);
      } else {
        stopQr();
      }
    },
    [authTab, modalPlatform, startQr, stopQr],
  );

  const checkRefresh = useCallback(
    async (platform: Platform, silent = false) => {
      if (checkingSetRef.current.has(platform)) return;
      checkingSetRef.current.add(platform);
      setCheckingPlatforms((prev) => ({ ...prev, [platform]: true }));
      try {
        const status = await SPEC_MAP[platform].refreshNow();
        await load();
        if (!silent) {
          const msg = t("account.refreshDone", { result: refreshOutcomeLabel(status) });
          if (isRefreshWarning(status)) notify.warning(msg);
          else notify.success(msg);
        }
      } catch (err) {
        if (!silent) notify.error(err);
        await load();
      } finally {
        checkingSetRef.current.delete(platform);
        setCheckingPlatforms((prev) => {
          const next = { ...prev };
          delete next[platform];
          return next;
        });
      }
    },
    [load, notify, refreshOutcomeLabel, t],
  );

  const checkAllRefreshable = useCallback(async () => {
    if (checkingAllRef.current) return;
    const targets = PLATFORM_SPECS.filter((spec) => {
      const cred = credMap.get(spec.id);
      return Boolean(cred && cred.status !== "expired" && cred.refreshable);
    });
    if (targets.length === 0) return;

    checkingAllRef.current = true;
    setCheckingAll(true);
    let failed = 0;
    try {
      for (const spec of targets) {
        if (checkingSetRef.current.has(spec.id)) continue;
        checkingSetRef.current.add(spec.id);
        setCheckingPlatforms((prev) => ({ ...prev, [spec.id]: true }));
        try {
          const status = await spec.refreshNow();
          if (isRefreshWarning(status)) failed += 1;
        } catch {
          failed += 1;
        } finally {
          checkingSetRef.current.delete(spec.id);
          setCheckingPlatforms((prev) => {
            const next = { ...prev };
            delete next[spec.id];
            return next;
          });
        }
      }
      await load();
      if (failed > 0) {
        notify.warning(t("account.checkAllPartial", { ok: targets.length - failed, failed }));
      } else {
        notify.success(t("account.checkAllDone", { count: targets.length }));
      }
    } finally {
      checkingAllRef.current = false;
      setCheckingAll(false);
    }
  }, [credMap, load, notify, t]);

  const saveCookie = useCallback(
    async (platform: Platform) => {
      setSavingCookie(true);
      try {
        const saved = await credentialSave(platform, draft);
        closeAuthModal();
        notify.success(t("account.savedAs", { name: saved.accountName }));
        await load();
      } catch (err) {
        notify.error(err);
      } finally {
        setSavingCookie(false);
      }
    },
    [closeAuthModal, draft, load, notify, t],
  );

  const logout = useCallback(
    async (platform: Platform) => {
      if (confirming !== platform) {
        setConfirming(platform);
        return;
      }
      setLoggingOut(platform);
      setConfirming(null);
      try {
        await credentialDelete(platform);
        notify.success(t("account.loggedOut"));
        await load();
      } catch (err) {
        notify.error(err);
      } finally {
        setLoggingOut(null);
      }
    },
    [confirming, load, notify, t],
  );

  // 关闭到托盘后 WebView 不会卸载；隐藏期间暂停扫码轮询，避免空跑网络请求。
  const [windowVisible, setWindowVisible] = useState(true);
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void onWindowVisibility((visible) => {
      if (alive) setWindowVisible(visible);
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch(() => {});
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  // 组件卸载、关闭弹窗或切换二维码时，通知后端释放需要显式取消的扫码 session（如虎牙）。
  useEffect(() => {
    if (!qr) return;
    const platform = qr.platform;
    const key = qr.ticket.qrcodeKey;
    return () => {
      cancelPlatformQr(platform, key);
    };
  }, [cancelPlatformQr, qr?.platform, qr?.ticket.qrcodeKey]);

  const qrState = qr?.poll?.state ?? "pending";
  const qrMessage =
    qrStartError ??
    qr?.error ??
    (qr ? (qr.poll ? qr.poll.message : t("account.qrWaiting")) : t("account.qrWaiting"));
  const counting = Boolean(qr) && (qrState === "pending" || qrState === "scanned");
  const secondsLeft = useSecondsLeft(counting ? qr?.expiresAt : null);

  // 本地先到点就先判失效，并释放后端扫码 session。
  useEffect(() => {
    if (!counting || secondsLeft === null || secondsLeft > 0) return;
    if (qr) cancelPlatformQr(qr.platform, qr.ticket.qrcodeKey);
    setQr((current) =>
      current && !current.poll?.state?.startsWith("expired")
        ? {
            ...current,
            poll: {
              state: "expired",
              message: t("account.qrExpired"),
              expiresIn: 0,
              credential: null,
            },
          }
        : current,
    );
  }, [cancelPlatformQr, counting, qr, secondsLeft, t]);

  // 扫码期间每 1.6 秒轮询一次；pending <-> scanned 切换时不重建定时器，confirmed/expired/error 后停表。
  const qrPollingActive = counting && windowVisible;
  useEffect(() => {
    if (!qrPollingActive || !qr) return;
    const pollFn = SPEC_MAP[qr.platform]?.qrPoll;
    if (!pollFn) return;
    const activePlatform = qr.platform;
    const activeKey = qr.ticket.qrcodeKey;

    let cancelled = false;
    let inFlight = false;
    const timer = window.setInterval(async () => {
      if (cancelled || inFlight) return;
      inFlight = true;
      try {
        const poll = await pollFn(activeKey);
        if (cancelled) return;
        setQr((current) =>
          current && current.platform === activePlatform && current.ticket.qrcodeKey === activeKey
            ? {
                ...current,
                poll,
                error: null,
                expiresAt:
                  poll.expiresIn > 0
                    ? Date.now() + poll.expiresIn * 1000
                    : current.expiresAt,
              }
            : current,
        );
        if (poll.state === "confirmed") {
          window.clearInterval(timer);
          // 已在后端完成消费，标记避免卸载时发多余的 cancel IPC。
          cancelledKeysRef.current.add(activeKey);
          notify.success(poll.message);
          closeAuthModal();
          await load();
        }
      } catch (err) {
        if (cancelled) return;
        setQr((current) =>
          current && current.platform === activePlatform && current.ticket.qrcodeKey === activeKey
            ? { ...current, error: describeError(err) }
            : current,
        );
      } finally {
        inFlight = false;
      }
    }, 1600);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [closeAuthModal, load, notify, qr?.platform, qr?.ticket.qrcodeKey, qrPollingActive]);

  // 统计数量与筛选列表
  const stats = useMemo(() => {
    let connected = 0;
    let expired = 0;
    let disconnected = 0;
    let refreshable = 0;
    for (const spec of PLATFORM_SPECS) {
      const cred = credMap.get(spec.id);
      if (!cred) {
        disconnected += 1;
      } else if (cred.status === "expired") {
        expired += 1;
      } else {
        connected += 1;
        if (cred.refreshable) refreshable += 1;
      }
    }
    return { total: PLATFORM_SPECS.length, connected, expired, disconnected, refreshable };
  }, [credMap]);

  // 如果当前停在「需处理」筛选而所有失效项已被修复，自动回到「全部」。
  useEffect(() => {
    if (filter === "expired" && stats.expired === 0) {
      setFilter("all");
    }
  }, [filter, stats.expired]);

  const { activeSpecs, catalogSpecs } = useMemo(() => {
    const q = query.trim().toLowerCase();
    const matched = PLATFORM_SPECS.filter((spec) => {
      const cred = credMap.get(spec.id);
      const isExpired = cred?.status === "expired";
      const isConnected = Boolean(cred) && !isExpired;

      if (filter === "connected" && !isConnected) return false;
      if (filter === "expired" && !isExpired) return false;
      if (filter === "disconnected" && cred) return false;

      if (!q) return true;
      const localized = t("platformName." + spec.id, { defaultValue: spec.id });
      const keywords = getPlatformVisual(spec.id).keywords;
      const haystack = [localized, spec.id, keywords, cred?.accountName ?? ""]
        .join(" ")
        .toLowerCase();
      return haystack.includes(q);
    });

    const active = matched
      .filter((spec) => credMap.has(spec.id))
      .sort((a, b) => {
        const ae = credMap.get(a.id)?.status === "expired" ? 0 : 1;
        const be = credMap.get(b.id)?.status === "expired" ? 0 : 1;
        return ae - be;
      });
    const catalog = matched.filter((spec) => !credMap.has(spec.id));
    return { activeSpecs: active, catalogSpecs: catalog };
  }, [credMap, filter, query, t]);

  if (!isTauri()) return null;

  const modalSpec = modalPlatform ? SPEC_MAP[modalPlatform] : null;
  const modalCred = modalPlatform ? credMap.get(modalPlatform) : undefined;
  const modalPlatformName = modalPlatform
    ? t("platformName." + modalPlatform, { defaultValue: modalPlatform })
    : "";

  const filterOptions: Array<{ key: FilterKey; label: string; count: number; warn?: boolean }> = [
    { key: "all", label: t("account.filterAll"), count: stats.total },
    { key: "connected", label: t("account.connected"), count: stats.connected },
    ...(stats.expired > 0
      ? [{ key: "expired" as const, label: t("account.filterExpired"), count: stats.expired, warn: true }]
      : []),
    { key: "disconnected", label: t("account.notConnected"), count: stats.disconnected },
  ];

  return (
    <div id="set-account" className="space-y-4">
      {/* 1. 顶部状态总览 + 一键检查 */}
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-[12px] border border-line bg-surface px-4 py-3 shadow-[var(--shadow-card)]">
        <div className="flex flex-wrap items-center gap-3 text-[12px] text-ink-soft">
          <span className="inline-flex items-center gap-1.5">
            <span className="h-[7px] w-[7px] rounded-full bg-ok" />
            <span>
              {t("account.connected")}{" "}
              <strong className="font-semibold tabular-nums text-ink">{stats.connected}</strong>
              <span className="text-faint"> / {stats.total}</span>
            </span>
          </span>

          {stats.expired > 0 ? (
            <>
              <span className="h-3.5 w-px bg-line" aria-hidden />
              <button
                type="button"
                onClick={() => setFilter("expired")}
                className="focus-ring inline-flex items-center gap-1.5 rounded text-warn hover:underline"
              >
                <span className="h-[7px] w-[7px] rounded-full bg-warn" />
                <span className="font-medium">
                  {t("account.summaryExpired", { count: stats.expired })}
                </span>
              </button>
            </>
          ) : null}

          <span className="h-3.5 w-px bg-line" aria-hidden />
          <span className="inline-flex items-center gap-1.5 text-muted">
            <span className="h-[7px] w-[7px] rounded-full bg-faint/60" />
            <span>
              {t("account.notConnected")}{" "}
              <strong className="font-semibold tabular-nums text-ink-soft">
                {stats.disconnected}
              </strong>
            </span>
          </span>
        </div>

        <Button
          size="sm"
          variant="secondary"
          disabled={checkingAll || stats.refreshable === 0 || items === null}
          onClick={() => void checkAllRefreshable()}
        >
          <RefreshCw className={cx("h-3.5 w-3.5", checkingAll && "animate-spin")} />
          <span>{checkingAll ? t("account.checkingAll") : t("account.checkAll")}</span>
        </Button>
      </div>

      {/* 2. 筛选分段 + 实时搜索 */}
      <div className="flex flex-wrap items-center justify-between gap-2.5">
        <div
          role="tablist"
          aria-label={t("account.filterLabel")}
          className="inline-flex items-center gap-0.5 rounded-[9px] border border-line bg-raised p-[3px]"
        >
          {filterOptions.map((opt) => {
            const active = filter === opt.key;
            return (
              <button
                key={opt.key}
                type="button"
                role="tab"
                aria-selected={active}
                onClick={() => setFilter(opt.key)}
                className={cx(
                  "focus-ring inline-flex items-center gap-1.5 rounded-[6px] px-2.5 py-1 text-[11.5px] font-medium transition-all",
                  active
                    ? "bg-surface text-ink shadow-[0_1px_2px_rgba(0,0,0,0.08)]"
                    : "text-muted hover:text-ink",
                )}
              >
                <span>{opt.label}</span>
                <span
                  className={cx(
                    "font-mono text-[10px] tabular-nums",
                    opt.warn
                      ? "rounded-full bg-warn/15 px-1.5 font-semibold text-warn"
                      : "opacity-70",
                  )}
                >
                  {opt.count}
                </span>
              </button>
            );
          })}
        </div>

        <div className="relative w-full sm:w-[224px]">
          <Search
            aria-hidden
            className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-faint"
          />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("account.searchPlaceholder")}
            className="focus-ring h-8 w-full rounded-[8px] border border-line bg-surface pl-8 pr-7 text-[12px] text-ink transition-colors placeholder:text-faint hover:border-line-strong"
          />
          {query ? (
            <button
              type="button"
              aria-label={t("account.clearSearch")}
              onClick={() => setQuery("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-0.5 text-faint hover:text-ink"
            >
              <X className="h-3 w-3" />
            </button>
          ) : null}
        </div>
      </div>

      {/* 3. 主内容：加载态 / 空筛选态 / 已关联 + 可添加分层 */}
      {items === null ? (
        <div className="space-y-2.5">
          {[0, 1].map((idx) => (
            <div
              key={idx}
              className="h-[72px] animate-pulse rounded-[12px] border border-line bg-surface/70"
            />
          ))}
        </div>
      ) : activeSpecs.length === 0 && catalogSpecs.length === 0 ? (
        <div className="rounded-[12px] border border-dashed border-line-strong bg-surface px-5 py-7 text-center text-[12px] text-faint">
          {t("account.emptySearch")}
        </div>
      ) : (
        <div className="space-y-5">
          {/* 区块 A：已关联账号（失效项自动置顶） */}
          {activeSpecs.length > 0 ? (
            <section className="space-y-2.5">
              <div className="flex flex-wrap items-center justify-between gap-2 px-0.5">
                <h3 className="text-[10.5px] font-semibold uppercase tracking-[0.08em] text-faint">
                  {t("account.sectionConnected", { count: activeSpecs.length })}
                </h3>
                <span className="text-[11px] text-faint">{t("account.singleAccountHint")}</span>
              </div>

              <div className="space-y-2">
                {activeSpecs.map((spec) => {
                  const item = credMap.get(spec.id)!;
                  const expired = item.status === "expired";
                  const name = t("platformName." + spec.id, { defaultValue: spec.id });
                  const isChecking = Boolean(checkingPlatforms[spec.id]);
                  const isLoggingOut = loggingOut === spec.id;
                  const hasRefreshWarn =
                    (item.refreshStatus.outcome === "failed" ||
                      item.refreshStatus.outcome === "confirm_failed" ||
                      item.refreshStatus.outcome === "not_refreshable" ||
                      item.refreshStatus.keyStale) &&
                    Boolean(item.refreshStatus.message);

                  const autoRefreshDesc = expired
                    ? t("account.expiredHint")
                    : item.refreshable
                      ? t(spec.autoRefreshOnKey)
                      : t(spec.noRefreshKey);

                  const checkSummary =
                    item.refreshable && !expired
                      ? item.refreshStatus.checkedAt > 0
                        ? t("account.refreshChecked", {
                            time: refreshTime(item.refreshStatus.checkedAt),
                            result: refreshOutcomeLabel(item.refreshStatus),
                          })
                        : t("account.refreshNever")
                      : null;

                  return (
                    <article
                      key={spec.id}
                      style={platformBrandStyle(spec.id)}
                      className={cx(
                        "relative flex flex-col gap-3 overflow-hidden rounded-[12px] border px-4 py-3.5 pl-[18px] shadow-[var(--shadow-card)] transition-colors sm:flex-row sm:items-center",
                        expired
                          ? "border-warn/40 bg-warn/[0.035]"
                          : "border-line bg-surface hover:border-line-strong",
                      )}
                    >
                      <span
                        aria-hidden
                        className="account-brand-bar absolute bottom-2.5 left-0 top-2.5 w-[3px] rounded-r-full opacity-85"
                      />

                      <div className="flex min-w-0 flex-1 items-start gap-3 sm:items-center">
                        <PlatformBadge platform={spec.id} size="md" />

                        <div className="min-w-0 flex-1">
                          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                            <span className="text-[13px] font-semibold text-ink">{name}</span>
                            <span className="text-line-strong" aria-hidden>
                              ·
                            </span>
                            <span className="truncate text-[12.5px] font-medium text-ink-soft">
                              {item.accountName || t("account.connected")}
                            </span>
                            <span
                              className={cx(
                                "inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[10px] font-semibold leading-4",
                                expired
                                  ? "bg-warn/15 text-warn"
                                  : "bg-ok/12 text-ok",
                              )}
                            >
                              {expired ? t("account.expired") : t("account.connected")}
                            </span>
                          </div>

                          <div
                            className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] leading-[17px] text-faint"
                            title={autoRefreshDesc}
                          >
                            <span
                              className={cx(
                                "inline-flex items-center gap-1.5",
                                expired || !item.refreshable || item.refreshStatus.keyStale
                                  ? "font-medium text-warn"
                                  : "text-muted",
                              )}
                            >
                              <span
                                className={cx(
                                  "h-[5px] w-[5px] shrink-0 rounded-full",
                                  expired || !item.refreshable || item.refreshStatus.keyStale
                                    ? "bg-warn"
                                    : "bg-live",
                                )}
                              />
                              <span className="truncate">
                                {checkSummary ?? autoRefreshDesc}
                              </span>
                            </span>
                            <span className="text-line-strong" aria-hidden>
                              ·
                            </span>
                            <span className="shrink-0 font-mono text-[10.5px] tabular-nums text-faint">
                              {t("account.cookieCount", { n: item.cookieNames.length })}
                            </span>
                          </div>

                          {hasRefreshWarn ? (
                            <p
                              className="mt-1 truncate text-[10.5px] leading-4 text-warn"
                              title={item.refreshStatus.message ?? undefined}
                            >
                              {item.refreshStatus.message}
                            </p>
                          ) : item.refreshable &&
                            !expired &&
                            item.refreshStatus.lastRefreshedAt > 0 &&
                            item.refreshStatus.outcome !== "refreshed" ? (
                            <p className="mt-0.5 truncate text-[10px] leading-4 text-faint">
                              {t("account.refreshLastRotated", {
                                time: refreshTime(item.refreshStatus.lastRefreshedAt),
                              })}
                            </p>
                          ) : null}
                        </div>
                      </div>

                      <div className="flex shrink-0 items-center justify-end gap-1.5 border-t border-line/70 pt-2.5 sm:border-t-0 sm:pt-0">
                        {expired ? (
                          <Button
                            size="sm"
                            variant="secondary"
                            className="border-warn/40 bg-warn/12 text-warn hover:bg-warn/20"
                            disabled={isLoggingOut}
                            onClick={() => openAuthModal(spec.id)}
                          >
                            {t("account.relogin")}
                          </Button>
                        ) : (
                          <>
                            {item.refreshable ? (
                              <Button
                                size="sm"
                                variant="secondary"
                                disabled={isChecking || checkingAll || isLoggingOut}
                                onClick={() => void checkRefresh(spec.id)}
                              >
                                {isChecking ? t("account.refreshChecking") : t("account.checkShort")}
                              </Button>
                            ) : null}
                            <Button
                              size="sm"
                              variant="ghost"
                              disabled={isLoggingOut}
                              onClick={() => openAuthModal(spec.id)}
                            >
                              {t("account.updateShort")}
                            </Button>
                          </>
                        )}
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={isLoggingOut}
                          className={
                            confirming === spec.id
                              ? "text-rec hover:bg-rec/10 hover:text-rec"
                              : "hover:text-rec"
                          }
                          onClick={() => void logout(spec.id)}
                          onBlur={() =>
                            setConfirming((current) => (current === spec.id ? null : current))
                          }
                        >
                          {confirming === spec.id
                            ? t("account.confirmLogout")
                            : t("account.logoutShort")}
                        </Button>
                      </div>
                    </article>
                  );
                })}
              </div>
            </section>
          ) : null}

          {/* 区块 B：可添加平台（紧凑磁贴目录，多平台自动 3 列） */}
          {catalogSpecs.length > 0 ? (
            <section className="space-y-2.5">
              <div className="flex flex-wrap items-center justify-between gap-2 px-0.5">
                <h3 className="text-[10.5px] font-semibold uppercase tracking-[0.08em] text-faint">
                  {t("account.sectionAvailable", { count: catalogSpecs.length })}
                </h3>
                <span className="text-[11px] text-faint">{t("account.catalogHint")}</span>
              </div>

              <div
                className={cx(
                  "grid grid-cols-1 gap-2.5 sm:grid-cols-2",
                  catalogSpecs.length >= 5 && "lg:grid-cols-3",
                )}
              >
                {catalogSpecs.map((spec) => {
                  const name = t("platformName." + spec.id, { defaultValue: spec.id });
                  const hint = t(spec.hintKey);
                  return (
                    <div
                      key={spec.id}
                      style={platformBrandStyle(spec.id)}
                      className="platform-tile flex items-center gap-2.5 rounded-[11px] border border-line bg-surface px-3 py-2.5 transition-all hover:-translate-y-px"
                    >
                      <PlatformBadge platform={spec.id} size="sm" />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-1.5">
                          <span className="truncate text-[12.5px] font-semibold text-ink">
                            {name}
                          </span>
                          <span className="shrink-0 rounded-[4px] border border-line bg-raised px-1.5 py-px text-[9.5px] font-semibold text-muted">
                            {spec.supportsQr ? t("account.methodQr") : t("account.methodCookie")}
                          </span>
                        </div>
                        <p className="mt-0.5 truncate text-[11px] text-faint" title={hint}>
                          {hint}
                        </p>
                      </div>
                      <Button
                        size="sm"
                        variant="secondary"
                        className="shrink-0"
                        onClick={() => openAuthModal(spec.id)}
                      >
                        {t("account.signInAction")}
                      </Button>
                    </div>
                  );
                })}
              </div>
            </section>
          ) : null}
        </div>
      )}

      {/* 4. 独立登录 / 更新凭据弹窗 */}
      <Dialog
        open={modalOpen && Boolean(modalPlatform)}
        onOpenChange={(open) => {
          if (!open) closeAuthModal();
        }}
      >
        {modalSpec && modalPlatform ? (
          <DialogContent
            size="sm"
            title={
              modalCred
                ? t("account.modalUpdateTitle", { name: modalPlatformName })
                : t("account.modalSignInTitle", { name: modalPlatformName })
            }
            description={t(modalSpec.hintKey)}
          >
            <div className="space-y-4">
              {/* 平台标识条 */}
              <div className="flex items-center gap-2.5 rounded-[10px] border border-line bg-canvas px-3 py-2">
                <PlatformBadge platform={modalPlatform} size="sm" />
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] font-semibold text-ink">{modalPlatformName}</div>
                  <div className="truncate text-[11px] text-faint">
                    {modalCred?.accountName
                      ? t("account.modalCurrentUser", { name: modalCred.accountName })
                      : t("account.cookieHint")}
                  </div>
                </div>
              </div>

              {/* 认证方式切换（支持扫码的平台显示双 Tab） */}
              {modalSpec.supportsQr ? (
                <div
                  role="tablist"
                  className="grid grid-cols-2 gap-1.5 rounded-[9px] border border-line bg-raised p-1"
                >
                  <button
                    type="button"
                    role="tab"
                    aria-selected={authTab === "qr"}
                    onClick={() => switchAuthTab("qr")}
                    className={cx(
                      "focus-ring inline-flex h-8 items-center justify-center gap-1.5 rounded-[7px] text-[12px] font-medium transition-all",
                      authTab === "qr"
                        ? "border border-accent-line bg-accent-soft text-accent-text"
                        : "text-muted hover:text-ink",
                    )}
                  >
                    <QrCode className="h-3.5 w-3.5" />
                    <span>{t("account.tabQr")}</span>
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={authTab === "cookie"}
                    onClick={() => switchAuthTab("cookie")}
                    className={cx(
                      "focus-ring inline-flex h-8 items-center justify-center gap-1.5 rounded-[7px] text-[12px] font-medium transition-all",
                      authTab === "cookie"
                        ? "border border-accent-line bg-accent-soft text-accent-text"
                        : "text-muted hover:text-ink",
                    )}
                  >
                    <Lock className="h-3.5 w-3.5" />
                    <span>{t("account.tabCookie")}</span>
                  </button>
                </div>
              ) : null}

              {/* 扫码面板 */}
              {authTab === "qr" && modalSpec.supportsQr ? (
                <div className="flex flex-col items-center gap-3 py-1 text-center">
                  <div className="flex h-[168px] w-[168px] items-center justify-center overflow-hidden rounded-[14px] border border-line bg-white p-2.5 shadow-[0_4px_18px_-10px_rgba(0,0,0,0.25)]">
                    {qr && qr.platform === modalPlatform ? (
                      <div
                        className={cx(
                          "h-full w-full transition-opacity [&_svg]:h-full [&_svg]:w-full",
                          (qrState === "expired" || qrState === "error") && "opacity-35",
                        )}
                        // SVG 由本地 qrcode 库生成，不含外部输入。
                        dangerouslySetInnerHTML={{ __html: qr.ticket.svg }}
                      />
                    ) : qrStartError ? (
                      <AlertCircle className="h-7 w-7 text-rec" />
                    ) : (
                      <RefreshCw
                        className={cx("h-6 w-6 text-faint", qrLoading && "animate-spin")}
                      />
                    )}
                  </div>

                  <div className="space-y-1.5">
                    <p
                      className={cx(
                        "text-[12.5px] font-medium",
                        qrState === "scanned"
                          ? "text-accent-text"
                          : qrStartError || qrState === "expired" || qrState === "error" || qr?.error
                            ? "text-rec"
                            : "text-ink",
                      )}
                    >
                      {qrLoading
                        ? t("account.qrWaiting")
                        : qrStartError || qr?.poll || qr?.error
                          ? qrMessage
                          : t(modalSpec.qrTitleKey ?? "account.qrTitle")}
                    </p>

                    {counting && secondsLeft !== null ? (
                      <span
                        className={cx(
                          "inline-block rounded-full bg-raised px-2.5 py-0.5 font-mono text-[10.5px] tabular-nums",
                          secondsLeft <= 30 ? "text-warn" : "text-muted",
                        )}
                      >
                        {t("account.qrExpiresIn", { time: formatCountdown(secondsLeft) })}
                      </span>
                    ) : null}

                    <p className="mx-auto max-w-[310px] text-[11px] leading-relaxed text-faint">
                      {t(modalSpec.qrHintKey ?? "account.qrHint")}
                    </p>
                  </div>

                  {qrStartError ||
                  qrState === "expired" ||
                  qrState === "error" ||
                  qr?.error ||
                  (!qr && !qrLoading) ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={qrLoading}
                      onClick={() => void startQr(modalPlatform)}
                    >
                      <RefreshCw className="h-3.5 w-3.5" />
                      <span>{t("account.qrRefresh")}</span>
                    </Button>
                  ) : null}
                </div>
              ) : (
                /* 手动粘贴 Cookie 面板 */
                <div className="space-y-3">
                  <textarea
                    value={draft}
                    onChange={(event) => setDraft(event.target.value)}
                    placeholder={t("account.cookiePlaceholder")}
                    spellCheck={false}
                    rows={4}
                    className="focus-ring w-full resize-none rounded-[9px] border border-line bg-canvas px-3 py-2.5 font-mono text-[11px] leading-relaxed text-ink transition-colors placeholder:text-faint"
                  />
                  <div className="flex items-center justify-between gap-3">
                    <span className="inline-flex min-w-0 items-center gap-1.5 text-[11px] text-faint">
                      <Lock className="h-3 w-3 shrink-0" />
                      <span className="truncate">{t("account.cookieHint")}</span>
                    </span>
                    <div className="flex shrink-0 items-center gap-1.5">
                      <Button size="sm" variant="secondary" onClick={closeAuthModal}>
                        {t("account.cancel")}
                      </Button>
                      <Button
                        size="sm"
                        variant="primary"
                        disabled={savingCookie || draft.trim().length === 0}
                        onClick={() => void saveCookie(modalPlatform)}
                      >
                        {savingCookie ? t("account.saving") : t("account.saveVerify")}
                      </Button>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </DialogContent>
        ) : null}
      </Dialog>
    </div>
  );
}
