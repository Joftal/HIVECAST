import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  bilibiliQrPoll,
  bilibiliQrStart,
  bilibiliRefreshNow,
  credentialDelete,
  douyuQrPoll,
  douyuQrStart,
  douyuRefreshNow,
  credentialSave,
  credentialsList,
  describeError,
  onAccountRefresh,
  isTauri,
  type CredentialInfo,
  type QrPoll,
  type QrTicket,
  type RefreshStatus,
} from "../../lib/ipc";
import { formatCountdown, useSecondsLeft } from "../../lib/countdown";
import { Button, cx } from "../../components/ui/Controls";
import { useNotify } from "../../lib/notify";

const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"] as const;
type Platform = (typeof PLATFORMS)[number];

const PLATFORM_ABBR: Record<Platform, string> = {
  bilibili: "BL",
  douyu: "DY",
  huya: "HY",
  douyin: "DO",
};

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

export default function AccountSection() {
  const { t, i18n } = useTranslation();
  const [items, setItems] = useState<CredentialInfo[] | null>(null);
  const notify = useNotify();
  const [editing, setEditing] = useState<Platform | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const [qr, setQr] = useState<QrState | null>(null);
  const [confirming, setConfirming] = useState<Platform | null>(null);
  /** 每次打开/关闭二维码递增，丢弃迟到的 generate/poll 响应。 */
  const qrSeq = useRef(0);
  /** 手动检查的同步闸门：state 更新前连点也不会发第二次请求。 */
  const checkingRef = useRef(false);

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

  // 续期检查可能耗时十几秒，且可能发生在设置页之外。后端写完状态后发事件，
  // 前端据此重拉，不再用 setTimeout 猜完成时间（也避免卸载后仍触发 load）。
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

  const find = (platform: string) => items?.find((item) => item.platform === platform);

  const refreshOutcomeLabel = (status: RefreshStatus) => {
    if (status.keyStale) return t("account.refreshOutcome.keyStale");
    return t(REFRESH_OUTCOME_KEY[status.outcome] ?? "account.refreshOutcome.failed");
  };

  const refreshTime = (value: number) =>
    new Date(value * 1000).toLocaleString(i18n.resolvedLanguage ?? undefined, {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });

  const checkRefresh = async (platform: Platform) => {
    if (checkingRef.current) return;
    checkingRef.current = true;
    setChecking(true);
    try {
      const status =
        platform === "douyu" ? await douyuRefreshNow() : await bilibiliRefreshNow();
      await load();
      notify.success(t("account.refreshDone", { result: refreshOutcomeLabel(status) }));
    } catch (err) {
      notify.error(err);
      // 失败也会落库：重新拉一次，让卡片显示最近一次检查结果。
      await load();
    } finally {
      checkingRef.current = false;
      setChecking(false);
    }
  };

  const saveCookie = async (platform: Platform) => {
    setBusy(true);
    try {
      const saved = await credentialSave(platform, draft);
      setEditing(null);
      setDraft("");
      notify.success(t("account.savedAs", { name: saved.accountName }));
      await load();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusy(false);
    }
  };

  const logout = async (platform: Platform) => {
    if (confirming !== platform) {
      setConfirming(platform);
      return;
    }
    setBusy(true);
    setConfirming(null);
    try {
      await credentialDelete(platform);
      notify.success(t("account.loggedOut"));
      await load();
    } catch (err) {
      notify.error(err);
    } finally {
      setBusy(false);
    }
  };

  const closeQr = () => {
    qrSeq.current += 1;
    setQr(null);
    setBusy(false);
  };

  const openQr = async (platform: Platform) => {
    const seq = ++qrSeq.current;
    setQr(null);
    setBusy(true);
    try {
      const ticket = platform === "douyu" ? await douyuQrStart() : await bilibiliQrStart();
      // 关闭面板或重新打开后，旧请求的响应必须丢弃。
      if (seq !== qrSeq.current) return;
      setQr({
        platform,
        ticket,
        poll: null,
        error: null,
        expiresAt: Date.now() + Math.max(1, ticket.expiresIn) * 1000,
      });
    } catch (err) {
      if (seq === qrSeq.current) notify.error(err);
    } finally {
      if (seq === qrSeq.current) setBusy(false);
    }
  };

  const qrState = qr?.poll?.state ?? "pending";
  const qrMessage =
    qr?.error ??
    (qr ? (qr.poll ? qr.poll.message : t("account.qrWaiting")) : t("account.qrWaiting"));
  const secondsLeft = useSecondsLeft(qr?.expiresAt);
  const counting = Boolean(qr) && (qrState === "pending" || qrState === "scanned");

  // 本地先到点就先判失效，不必等服务端下一次轮询（最多省 1.6 秒的错觉）。
  useEffect(() => {
    if (!counting || secondsLeft === null || secondsLeft > 0) return;
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
  }, [counting, secondsLeft, t]);

  // 扫码期间每 1.6 秒问一次；confirmed / expired / error 后停表。
  useEffect(() => {
    if (!qr) return;
    const state = qr.poll?.state ?? "pending";
    if (state !== "pending" && state !== "scanned") return;
    let cancelled = false;
    // 单飞：HTTP 超时 10s，若网络慢，setInterval 会叠出多个在途 poll。
    let inFlight = false;
    const timer = window.setInterval(async () => {
      if (cancelled || inFlight) return;
      inFlight = true;
      try {
        const poll =
          qr.platform === "douyu"
            ? await douyuQrPoll(qr.ticket.qrcodeKey)
            : await bilibiliQrPoll(qr.ticket.qrcodeKey);
        if (cancelled) return;
        setQr((current) =>
          current
            ? {
                ...current,
                poll,
                error: null,
                // 服务端是权威：每轮都按它还剩多少秒重新对齐本地倒计时。
                expiresAt:
                  poll.expiresIn > 0
                    ? Date.now() + poll.expiresIn * 1000
                    : current.expiresAt,
              }
            : current,
        );
        // confirmed 的后处理失败、二维码失效都会返回 error/expired，effect 依赖
        // qrState 变化后自动卸载，不会继续轮询已消费的 key。
        if (poll.state === "confirmed") {
          window.clearInterval(timer);
          notify.success(poll.message);
          setQr(null);
          await load();
        }
      } catch (err) {
        if (cancelled) return;
        setQr((current) => (current ? { ...current, error: describeError(err) } : current));
      } finally {
        inFlight = false;
      }
    }, 1600);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [qr?.platform, qr?.ticket.qrcodeKey, qrState, load]);

  if (!isTauri()) return null;

  const cookieEditor = (platform: Platform) => (
    <div className="animate-fade-rise mt-3 space-y-2 rounded-[9px] border border-line bg-raised/40 p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-[10.5px] font-medium text-muted">
          {t("account.cookieTitle", {
            name: t("platformName." + platform, { defaultValue: platform }),
          })}
        </span>
        <button
          type="button"
          onClick={() => {
            setEditing(null);
            setDraft("");
          }}
          className="shrink-0 text-[10px] text-faint transition-colors hover:text-ink"
        >
          {t("account.cancel")}
        </button>
      </div>
      <textarea
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        placeholder={t("account.cookiePlaceholder")}
        spellCheck={false}
        rows={3}
        className="focus-ring w-full resize-none rounded-ctl border border-line bg-surface px-2.5 py-2 font-mono text-[10.5px] leading-relaxed text-ink transition-colors placeholder:text-faint focus:border-accent-line"
      />
      <div className="flex items-end justify-between gap-3">
        <span className="min-w-0 text-[10px] leading-4 text-faint">{t("account.cookieHint")}</span>
        <Button
          size="sm"
          variant="primary"
          disabled={busy || draft.trim().length === 0}
          onClick={() => void saveCookie(platform)}
        >
          {busy ? t("account.saving") : t("account.save")}
        </Button>
      </div>
    </div>
  );

  const qrPanel = qr ? (
    <div className="animate-fade-rise mt-3 space-y-2.5 rounded-[9px] border border-line bg-raised/40 p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="min-w-0 truncate text-[10.5px] font-medium text-muted">
          {qr.platform === "douyu" ? t("account.qrTitleDouyu") : t("account.qrTitle")}
        </span>
        <button
          type="button"
          onClick={closeQr}
          className="shrink-0 text-[10px] text-faint transition-colors hover:text-ink"
        >
          {t("account.cancel")}
        </button>
      </div>
      <div className="flex items-start gap-3">
        <div
          className="w-[128px] shrink-0 overflow-hidden rounded-ctl border border-line bg-white p-1.5 [&_svg]:h-full [&_svg]:w-full"
          // SVG 由本地 qrcode 库生成，不含外部输入。
          dangerouslySetInnerHTML={{ __html: qr.ticket.svg }}
        />
        <div className="min-w-0 flex-1 space-y-1.5">
          <p
            className={cx(
              "text-[11px] leading-4",
              qrState === "scanned"
                ? "text-accent-text"
                : qrState === "expired" || qr?.error
                  ? "text-rec"
                  : "text-muted",
            )}
          >
            {qrMessage}
          </p>
          {counting && secondsLeft !== null ? (
            <p
              className={cx(
                "font-mono text-[10px] tabular-nums",
                secondsLeft <= 30 ? "text-warn" : "text-faint",
              )}
            >
              {t("account.qrExpiresIn", { time: formatCountdown(secondsLeft) })}
            </p>
          ) : null}
          <p className="text-[10px] leading-4 text-faint">
            {qr.platform === "douyu" ? t("account.qrHintDouyu") : t("account.qrHint")}
          </p>
          {qrState === "expired" || qr?.error ? (
            <Button size="sm" variant="secondary" onClick={() => void openQr(qr.platform)}>
              {t("account.qrRefresh")}
            </Button>
          ) : null}
        </div>
      </div>
    </div>
  ) : null;

  return (
    <div className="space-y-3.5">
      <p className="text-[11px] leading-4 text-muted">{t("account.singleAccountHint")}</p>

      <div className="grid grid-cols-1 items-start gap-3 md:grid-cols-2">
        {PLATFORMS.map((platform) => {
          const item = find(platform);
          const expired = item?.status === "expired";
          const connected = Boolean(item);
          const name = t("platformName." + platform, { defaultValue: platform });
          const status = connected
            ? expired
              ? t("account.expired")
              : t("account.connected")
            : t("account.notConnected");

          return (
            <article
              key={platform}
              className="flex flex-col rounded-[12px] border border-line bg-surface p-4 shadow-[0_1px_2px_rgba(16,24,40,0.04)]"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="flex min-w-0 items-center gap-2.5">
                  <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[8px] border border-line bg-raised font-mono text-[10px] tracking-[0.08em] text-muted">
                    {PLATFORM_ABBR[platform]}
                  </span>
                  <div className="min-w-0">
                    <h3 className="truncate text-[12.5px] font-semibold text-ink">{name}</h3>
                    {item ? (
                      <p className="mt-0.5 font-mono text-[9.5px] tabular-nums text-faint">
                        {t("account.cookieCount", { n: item.cookieNames.length })}
                      </p>
                    ) : null}
                  </div>
                </div>
                <span
                  className={cx(
                    "shrink-0 rounded-full border px-2 py-0.5 text-[9.5px] leading-4",
                    expired
                      ? "border-warn/35 text-warn"
                      : connected
                        ? "border-line text-muted"
                        : "border-line text-faint",
                  )}
                >
                  {status}
                </span>
              </div>

              <div className="mt-3 min-w-0">
                {item ? (
                  <p
                    className={cx(
                      "truncate text-[13px]",
                      expired ? "text-ink-soft" : "text-ink",
                    )}
                  >
                    {item.accountName || t("account.connected")}
                  </p>
                ) : null}
                <p
                  className={cx(
                    item ? "mt-1 text-[10px] leading-4" : "text-[10.5px] leading-4",
                    expired ? "text-warn/90" : "text-faint",
                  )}
                >
                  {item
                    ? expired
                      ? t("account.expiredHint")
                      : t("account.cookieHint")
                    : platform === "bilibili"
                      ? t("account.bilibiliHint")
                      : platform === "douyu"
                        ? t("account.douyuHint")
                        : t("account.cookieHint")}
                </p>
                {(platform === "bilibili" || platform === "douyu") && item ? (
                  <div className="mt-1.5">
                    <p
                      className={cx(
                        "flex items-center gap-1.5 text-[10px] leading-4",
                        item.refreshable ? "text-muted" : "text-warn/90",
                      )}
                    >
                      <span
                        className={cx(
                          "h-1 w-1 rounded-full",
                          item.refreshable ? "bg-live" : "bg-warn",
                        )}
                      />
                      {item.refreshable
                        ? platform === "douyu"
                          ? t("account.autoRefreshOnDouyu")
                          : t("account.autoRefreshOn")
                        : platform === "douyu"
                          ? t("account.douyuNoRefresh")
                          : t("account.autoRefreshOff")}
                    </p>
                    {item.refreshable ? (
                      <div className="mt-1 flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <p
                            className={cx(
                              "truncate text-[10px] leading-4",
                              item.refreshStatus.keyStale ? "text-warn" : "text-faint",
                            )}
                            title={item.refreshStatus.message ?? undefined}
                          >
                            {item.refreshStatus.checkedAt > 0
                              ? t("account.refreshChecked", {
                                  time: refreshTime(item.refreshStatus.checkedAt),
                                  result: refreshOutcomeLabel(item.refreshStatus),
                                })
                              : t("account.refreshNever")}
                          </p>
                          {(item.refreshStatus.outcome === "failed" ||
                            item.refreshStatus.outcome === "confirm_failed" ||
                            item.refreshStatus.outcome === "not_refreshable" ||
                            item.refreshStatus.keyStale) &&
                          item.refreshStatus.message ? (
                            <p
                              className="truncate text-[10px] leading-4 text-warn"
                              title={item.refreshStatus.message}
                            >
                              {item.refreshStatus.message}
                            </p>
                          ) : null}
                          {item.refreshStatus.lastRefreshedAt > 0 &&
                          item.refreshStatus.outcome !== "refreshed" ? (
                            <p className="truncate text-[9.5px] leading-4 text-faint">
                              {t("account.refreshLastRotated", {
                                time: refreshTime(item.refreshStatus.lastRefreshedAt),
                              })}
                            </p>
                          ) : null}
                        </div>
                        <Button
                          size="sm"
                          variant="ghost"
                          className="shrink-0"
                          disabled={busy || checking}
                          onClick={() => void checkRefresh(platform)}
                        >
                          {checking ? t("account.refreshChecking") : t("account.refreshNow")}
                        </Button>
                      </div>
                    ) : null}
                  </div>
                ) : null}
              </div>

              <div className="mt-3 flex flex-wrap items-center gap-1 border-t border-line/80 pt-2.5">
                {platform === "bilibili" || platform === "douyu" ? (
                  <Button
                    size="sm"
                    variant={connected ? "ghost" : "secondary"}
                    disabled={busy}
                    onClick={() => {
                      setEditing(null);
                      setDraft("");
                      void openQr(platform);
                    }}
                  >
                    {t("account.qrLogin")}
                  </Button>
                ) : null}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => {
                    setEditing(platform);
                    setDraft("");
                    closeQr();
                  }}
                >
                  {item ? t("account.updateCookie") : t("account.pasteCookie")}
                </Button>
                {item ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    className={confirming === platform ? "text-rec" : ""}
                    onClick={() => void logout(platform)}
                    onBlur={() => setConfirming((current) => (current === platform ? null : current))}
                  >
                    {confirming === platform ? t("account.confirmLogout") : t("account.logout")}
                  </Button>
                ) : null}
              </div>

              {editing === platform ? cookieEditor(platform) : null}
              {qr?.platform === platform ? qrPanel : null}
            </article>
          );
        })}
      </div>
    </div>
  );
}
