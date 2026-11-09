import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Tooltip } from "../arc/tooltip/tooltip";

/** 控件标题右侧的小帮助按钮：图标保持低调，悬停/聚焦时由 Arc Tooltip 解释功能。 */
export default function InfoTip({ content, label }: { content: ReactNode; label?: string }) {
  const { t } = useTranslation();
  return (
    <Tooltip content={content} side="top">
      <button
        type="button"
        aria-label={label ?? t("common.help")}
        className="focus-ring inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-faint transition-colors hover:bg-raised hover:text-ink"
      >
        <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden>
          <circle cx="8" cy="8" r="5.4" />
          <path d="M8 7.1v3.2" strokeLinecap="round" />
          <circle cx="8" cy="5.1" r="0.7" fill="currentColor" stroke="none" />
        </svg>
      </button>
    </Tooltip>
  );
}
