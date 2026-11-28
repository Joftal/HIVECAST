import { useTranslation } from "react-i18next";
import { PageShell } from "../../components/ui/PageShell";

/**
 * 观看页的懒加载/启动占位：先摆出播放器网格的壳，避免整块主内容区变成 spinner。
 * 只有一个格子有色差，让骨架看起来像“正在铺开画面”，而不是静态灰块。
 */
export default function WatchSkeleton() {
  const { t } = useTranslation();
  return (
    <div
      className="relative flex h-full min-h-0 min-w-0 overflow-hidden bg-canvas text-ink"
      role="status"
      aria-live="polite"
    >
      <span className="sr-only">{t("common.loading")}</span>
      <PageShell className="relative min-w-0">
        <div className="relative min-h-0 flex-1 p-3">
          <div className="grid h-full min-h-[280px] grid-cols-2 grid-rows-2 gap-2">
            {[0, 1, 2, 3].map((cell) => (
              <div
                key={cell}
                aria-hidden
                className={
                  cell === 0
                    ? "min-h-0 animate-pulse rounded-[8px] border border-line bg-raised/80 motion-reduce:animate-none"
                    : "min-h-0 animate-pulse rounded-[8px] border border-line bg-raised/55 motion-reduce:animate-none"
                }
              />
            ))}
          </div>
        </div>
      </PageShell>
    </div>
  );
}
