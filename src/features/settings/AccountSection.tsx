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
import { Button } from "../../components/ui/Controls";

const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"] as const;
type Platform = (typeof PLATFORMS)[number];

const PLATFORM_DOT: Record<Platform, string> = {
  bilibili: "#22a7f0",
  douyu: "#ff7a1a",
  huya: "#f5a623",
  douyin: "#ff2d6f",
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
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editing, setEditing] = useState<Platform | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [qr, setQr] = useState<QrState | null>(null);
  const [confirming, setConfirming] = useState<Platform | null>(null);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      setItems(await credentialsList());
      setError(null);
    } catch (err) {
      setError(describeError(err));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const find = (platform: string) => items?.find((item) => item.platform === platform);

  const saveCookie = async (platform: Platform) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await credentialSave(platform, draft);
      setEditing(null);
      setDraft("");
      setNotice(t("account.savedAs", { name: saved.accountName }));
      await load();
    } catch (err) {
      setError(describeError(err));
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
    setError(null);
    setNotice(null);
    try {
      await credentialDelete(platform);
      setNotice(t("account.loggedOut"));
      await load();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  };

  const openQr = async () => {
    setQr(null);
    setError(null);
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
      setError(describeError(err));
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
          setNotice(poll.message);
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

  return (
    <div className="space-y-2">
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-[10.5px] text-faint">{t("account.singleAccountHint")}</span>
      </div>

      <div className="divide-y divide-line overflow-hidden rounded-ctl border border-line bg-surface">
        {PLATFORMS.map((platform) => {
          const item = find(platform);
          const expired = item?.status === "expired";
          return (
            <div key={platform} className="px-3 py-2">
              <div className="flex items-center gap-2">
                <span className="flex w-[68px] shrink-0 items-center gap-1.5 text-[11px] text-ink-soft">
                  <span
                    className="h-1.5 w-1.5 rounded-full"
                    style={{ background: PLATFORM_DOT[platform] }}
                    aria-hidden
                  />
                  {t("platformName." + platform, { defaultValue: platform })}
                </span>
                <span
                  className={
                    "min-w-0 flex-1 truncate text-[11.5px] " + (expired ? "text-warn" : "text-muted")
                  }
                >
                  {item
                    ? expired
                      ? t("account.expired")
                      : item.accountName || t("account.connected")
                    : t("account.notConnected")}
                </span>
                {item && (
                  <span className="shrink-0 font-mono text-[10px] tabular-nums text-faint">
                    {t("account.cookieCount", { n: item.cookieNames.length })}
                  </span>
                )}
                <div className="flex shrink-0 items-center gap-1">
                  {platform === "bilibili" && (
                    <Button size="sm" variant="ghost" disabled={busy} onClick={() => void openQr()}>
                      {t("account.qrLogin")}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => {
                      setEditing(platform);
                      setDraft("");
                      setNotice(null);
                      setError(null);
                    }}
                  >
                    {item ? t("account.updateCookie") : t("account.pasteCookie")}
                  </Button>
                  {item && (
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
                  )}
                </div>
              </div>

              {platform === "bilibili" && !item && (
                <p className="mt-1 text-[10px] leading-4 text-faint">{t("account.bilibiliHint")}</p>
              )}
              {expired && <p className="mt-1 text-[10px] leading-4 text-warn">{t("account.expiredHint")}</p>}
            </div>
          );
        })}
      </div>

      {editing && (
        <div className="animate-fade-rise space-y-2 rounded-ctl border border-line bg-surface p-2.5">
          <div className="flex items-center justify-between">
            <span className="text-[11px] text-muted">
              {t("account.cookieTitle", {
                name: t("platformName." + editing, { defaultValue: editing }),
              })}
            </span>
            <button
              type="button"
              onClick={() => {
                setEditing(null);
                setDraft("");
              }}
              className="text-[10.5px] text-faint transition-colors hover:text-ink"
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
            className="focus-ring w-full resize-none rounded-ctl border border-line bg-raised px-2.5 py-2 font-mono text-[10.5px] leading-relaxed text-ink transition-colors placeholder:text-faint focus:border-accent-line"
          />
          <div className="flex items-center justify-between gap-2">
            <span className="text-[10px] leading-4 text-faint">{t("account.cookieHint")}</span>
            <Button
              size="sm"
              variant="primary"
              disabled={busy || draft.trim().length === 0}
              onClick={() => void saveCookie(editing)}
            >
              {busy ? t("account.saving") : t("account.save")}
            </Button>
          </div>
        </div>
      )}

      {qr && (
        <div className="animate-fade-rise space-y-2 rounded-ctl border border-line bg-surface p-3">
          <div className="flex items-center justify-between">
            <span className="text-[11px] text-muted">{t("account.qrTitle")}</span>
            <button
              type="button"
              onClick={() => setQr(null)}
              className="text-[10.5px] text-faint transition-colors hover:text-ink"
            >
              {t("account.cancel")}
            </button>
          </div>
          <div
            className="mx-auto w-[188px] overflow-hidden rounded-ctl bg-white p-2 [&_svg]:h-full [&_svg]:w-full"
            // SVG 由本地 qrcode 库生成，不含外部输入。
            dangerouslySetInnerHTML={{ __html: qr.ticket.svg }}
          />
          <p
            className={
              qrState === "scanned"
                ? "text-center text-[11px] text-accent-text"
                : qrState === "expired" || qr?.error
                  ? "text-center text-[11px] text-rec"
                  : "text-center text-[11px] text-muted"
            }
          >
            {qrMessage}
          </p>
          {counting && secondsLeft !== null && (
            <p
              className={
                "text-center font-mono text-[10px] tabular-nums " +
                (secondsLeft <= 30 ? "text-warn" : "text-faint")
              }
            >
              {t("account.qrExpiresIn", { time: formatCountdown(secondsLeft) })}
            </p>
          )}
          <div className="flex items-center justify-center gap-2">
            {(qrState === "expired" || qr?.error) && (
              <Button size="sm" variant="secondary" onClick={() => void openQr()}>
                {t("account.qrRefresh")}
              </Button>
            )}
            <span className="text-[10px] text-faint">{t("account.qrHint")}</span>
          </div>
        </div>
      )}

      {notice && <p className="animate-fade-rise text-[11px] text-live">{notice}</p>}
      {error && <p className="animate-fade-rise text-[11px] leading-4 text-rec">{error}</p>}
    </div>
  );
}
