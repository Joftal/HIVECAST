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

const PLATFORMS = ["bilibili", "douyu", "huya", "douyin"] as const;
type Platform = (typeof PLATFORMS)[number];

const PLATFORM_ACCENT: Record<Platform, string> = {
  bilibili: "text-sky-300 border-sky-400/30 bg-sky-400/10",
  douyu: "text-orange-300 border-orange-400/30 bg-orange-400/10",
  huya: "text-amber-300 border-amber-400/30 bg-amber-400/10",
  douyin: "text-fuchsia-300 border-fuchsia-400/30 bg-fuchsia-400/10",
};

type QrState = {
  ticket: QrTicket;
  poll: QrPoll | null;
  error: string | null;
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
      setQr({ ticket, poll: null, error: null });
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
        setQr((current) => (current ? { ...current, poll, error: null } : current));
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

  const fieldClass =
    "w-full rounded-lg border hairline bg-ink-800/80 px-2.5 py-1.5 text-[12px] text-zinc-100 outline-none transition-colors focus:border-amber-400/50";
  const labelClass = "text-[11px] font-medium tracking-wide text-zinc-400";
  const smallButton =
    "focus-ring shrink-0 rounded-full border hairline px-2.5 py-1 text-[11px] text-zinc-300 transition-colors hover:border-white/25 hover:text-white disabled:cursor-not-allowed disabled:opacity-40";

  return (
    <div className="space-y-2 rounded-xl border hairline bg-ink-850/50 px-3 py-3">
      <div className="flex items-baseline justify-between gap-2">
        <span className={labelClass}>{t("account.title")}</span>
        <span className="text-[10px] text-zinc-600">{t("account.singleAccountHint")}</span>
      </div>

      <div className="space-y-1.5">
        {PLATFORMS.map((platform) => {
          const item = find(platform);
          const expired = item?.status === "expired";
          return (
            <div
              key={platform}
              className="rounded-lg border hairline bg-ink-900/60 px-2.5 py-2"
            >
              <div className="flex items-center gap-2">
                <span
                  className={
                    "rounded-full border px-1.5 py-0.5 text-[10px] leading-none " +
                    PLATFORM_ACCENT[platform]
                  }
                >
                  {t("platformName." + platform, { defaultValue: platform })}
                </span>
                <span className="min-w-0 flex-1 truncate text-[12px] text-zinc-300">
                  {item
                    ? expired
                      ? t("account.expired")
                      : item.accountName || t("account.connected")
                    : t("account.notConnected")}
                </span>
                {item && (
                  <span className="shrink-0 text-[10px] text-zinc-600">
                    {t("account.cookieCount", { n: item.cookieNames.length })}
                  </span>
                )}
              </div>

              <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                {platform === "bilibili" && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void openQr()}
                    className="focus-ring rounded-full border border-sky-400/40 bg-sky-400/10 px-2.5 py-1 text-[11px] text-sky-200 transition-colors hover:bg-sky-400/20 disabled:opacity-40"
                  >
                    {t("account.qrLogin")}
                  </button>
                )}
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setEditing(platform);
                    setDraft("");
                    setNotice(null);
                    setError(null);
                  }}
                  className={smallButton}
                >
                  {item ? t("account.updateCookie") : t("account.pasteCookie")}
                </button>
                {item && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void logout(platform)}
                    onBlur={() => setConfirming((current) => (current === platform ? null : current))}
                    className={
                      confirming === platform
                        ? "focus-ring shrink-0 rounded-full border border-red-400/40 bg-red-400/10 px-2.5 py-1 text-[11px] text-red-300"
                        : smallButton
                    }
                  >
                    {confirming === platform ? t("account.confirmLogout") : t("account.logout")}
                  </button>
                )}
              </div>

              {platform === "bilibili" && !item && (
                <p className="mt-1.5 text-[10px] leading-relaxed text-zinc-600">
                  {t("account.bilibiliHint")}
                </p>
              )}
              {expired && (
                <p className="mt-1.5 text-[10px] leading-relaxed text-amber-300/80">
                  {t("account.expiredHint")}
                </p>
              )}
            </div>
          );
        })}
      </div>

      {editing && (
        <div className="space-y-1.5 rounded-lg border hairline bg-ink-900/80 px-2.5 py-2">
          <div className="flex items-center justify-between">
            <span className="text-[11px] text-zinc-400">
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
              className="text-[11px] text-zinc-500 hover:text-white"
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
            className={fieldClass + " resize-none font-mono text-[11px] leading-relaxed"}
          />
          <div className="flex items-center justify-between gap-2">
            <span className="text-[10px] leading-relaxed text-zinc-600">
              {t("account.cookieHint")}
            </span>
            <button
              type="button"
              disabled={busy || draft.trim().length === 0}
              onClick={() => void saveCookie(editing)}
              className="focus-ring shrink-0 rounded-full border border-amber-400/40 bg-amber-400/15 px-3 py-1 text-[11px] font-medium text-amber-200 hover:bg-amber-400/25 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {busy ? t("account.saving") : t("account.save")}
            </button>
          </div>
        </div>
      )}

      {qr && (
        <div className="space-y-2 rounded-lg border hairline bg-ink-900/90 px-3 py-3">
          <div className="flex items-center justify-between">
            <span className="text-[11px] text-zinc-400">{t("account.qrTitle")}</span>
            <button
              type="button"
              onClick={() => setQr(null)}
              className="text-[11px] text-zinc-500 hover:text-white"
            >
              {t("account.cancel")}
            </button>
          </div>
          <div
            className="mx-auto w-[196px] overflow-hidden rounded-lg bg-white p-2 [&_svg]:h-full [&_svg]:w-full"
            // SVG 由本地 qrcode 库生成，不含外部输入。
            dangerouslySetInnerHTML={{ __html: qr.ticket.svg }}
          />
          <p
            className={
              qrState === "scanned"
                ? "text-center text-[11px] text-amber-300"
                : qrState === "expired" || qr?.error
                  ? "text-center text-[11px] text-red-300"
                  : "text-center text-[11px] text-zinc-400"
            }
          >
            {qrMessage}
          </p>
          <div className="flex items-center justify-center gap-2">
            {(qrState === "expired" || qr?.error) && (
              <button type="button" onClick={() => void openQr()} className={smallButton}>
                {t("account.qrRefresh")}
              </button>
            )}
            <span className="text-[10px] text-zinc-600">{t("account.qrHint")}</span>
          </div>
        </div>
      )}

      {notice && <p className="text-[11px] text-emerald-400">{notice}</p>}
      {error && <p className="text-[11px] leading-relaxed text-red-300">{error}</p>}
    </div>
  );
}
