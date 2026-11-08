import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useMaximized } from "../lib/useMaximized";

/** Windows 无边框窗口的自绘标题栏按钮：最小化 / 最大化 / 还原 / 关闭。
    macOS 继续用系统红绿灯（Overlay 标题栏），不渲染这一组。 */
export default function WindowControls() {
  const { t } = useTranslation();
  const maximized = useMaximized();

  const win = getCurrentWindow();
  const base =
    "focus-ring flex h-full w-[46px] items-center justify-center text-muted transition-colors duration-150";
  const normal = `${base} hover:bg-raised hover:text-ink`;
  const close = `${base} hover:bg-rec hover:text-white`;

  return (
    <div className="ml-2 flex h-full items-stretch self-stretch border-l border-line">
      <button
        type="button"
        aria-label={t("app.windowMinimize")}
        title={t("app.windowMinimize")}
        className={normal}
        onClick={() => void win.minimize().catch(() => undefined)}
      >
        <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden>
          <path d="M2.6 6h6.8" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
        </svg>
      </button>
      <button
        type="button"
        aria-label={maximized ? t("app.windowRestore") : t("app.windowMaximize")}
        title={maximized ? t("app.windowRestore") : t("app.windowMaximize")}
        className={normal}
        onClick={() => void win.toggleMaximize().catch(() => undefined)}
      >
        {maximized ? (
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden>
            <rect x="2" y="2" width="6.2" height="6.2" rx="1.1" stroke="currentColor" strokeWidth="1.1" />
            <path
              d="M4.2 9.7h4.4a1.2 1.2 0 0 0 1.2-1.2V4.1"
              stroke="currentColor"
              strokeWidth="1.1"
              strokeLinecap="round"
            />
          </svg>
        ) : (
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden>
            <rect x="2.2" y="2.2" width="7.6" height="7.6" rx="1.2" stroke="currentColor" strokeWidth="1.1" />
          </svg>
        )}
      </button>
      <button
        type="button"
        aria-label={t("app.windowClose")}
        title={t("app.windowClose")}
        className={close}
        onClick={() => void win.close().catch(() => undefined)}
      >
        <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden>
          <path d="M2.9 2.9l6.2 6.2M9.1 2.9L2.9 9.1" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
        </svg>
      </button>
    </div>
  );
}
