import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  bilibiliQrPoll,
  bilibiliQrStart,
  credentialDelete,
  credentialSave,
  credentialsList,
  describeError,
  isTauri,
  type CredentialInfo,
  type QrPoll,
  type QrTicket,
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

type QrState = {
  ticket: QrTicket;
  poll: QrPoll | null;
  error: string | null;
  /** 失效时刻（绝对毫秒）；每次轮询用服务端剩余秒数重新对齐。 */
  expiresAt: number;
};

export default function AccountSection() {
  const { t } = useTranslation();
  const [items, setItems] = useState<CredentialInfo[] | null>(null);
  const notify = useNotify();
  const [editing, setEditing] = useState<Platform | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [qr, setQr] = useState<QrState | null>(null);
  const [confirming, setConfirming] = useState<Platform | null>(null);

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

  const find = (platform: string) => items?.find((item) => item.platform === platform);

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

  const openQr = async () => {
    setQr(null);
    setBusy(true);
    try {
      const ticket = await bilibiliQrStart();
      setQr({
        ticket,
        poll: null,
        error: null,
        expiresAt: Date.now() + Math.max(1, ticket.expiresIn) * 1000,
      });
    } catch (err) {
      notify.error(err);
    } finally {
      setBusy(false);
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
    const timer = window.setInterval(async () => {
      try {
        const poll = await bilibiliQrPoll(qr.ticket.qrcodeKey);
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
        if (poll.state === "confirmed") {
          window.clearInterval(timer);
          notify.success(poll.message);
          setQr(null);
          await load();
        }
      } catch (err) {
        if (cancelled) return;
        setQr((current) => (current ? { ...current, error: describeError(err) } : current));
      }
    }, 1600);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [qr?.ticket.qrcodeKey, qrState, load]);

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
          {t("account.qrTitle")}
        </span>
        <button
          type="button"
          onClick={() => setQr(null)}
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
          <p className="text-[10px] leading-4 text-faint">{t("account.qrHint")}</p>
          {qrState === "expired" || qr?.error ? (
            <Button size="sm" variant="secondary" onClick={() => void openQr()}>
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
                      : t("account.cookieHint")}
                </p>
                {platform === "bilibili" && item ? (
                  <p
                    className={cx(
                      "mt-1 flex items-center gap-1.5 text-[10px] leading-4",
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
                      ? t("account.autoRefreshOn")
                      : t("account.autoRefreshOff")}
                  </p>
                ) : null}
              </div>

              <div className="mt-3 flex flex-wrap items-center gap-1 border-t border-line/80 pt-2.5">
                {platform === "bilibili" ? (
                  <Button
                    size="sm"
                    variant={connected ? "ghost" : "secondary"}
                    disabled={busy}
                    onClick={() => {
                      setEditing(null);
                      setDraft("");
                      void openQr();
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
                    setQr(null);
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
              {platform === "bilibili" ? qrPanel : null}
            </article>
          );
        })}
      </div>
    </div>
  );
}
