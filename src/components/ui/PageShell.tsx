import type { ReactNode } from "react";
import { cx } from "./Controls";
import { SearchField } from "../arc/search-field/search-field";

/** 四个主 Tab 共用的页面骨架：固定的标题区 + 可选的工具栏 + 可滚动内容区。 */

export function PageShell({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return <div className={cx("relative flex min-h-0 flex-1 flex-col", className)}>{children}</div>;
}

export function PageHeader({
  title,
  subtitle,
  leading,
  prefix,
  actions,
  className,
}: {
  /** 主 Tab 可以不传标题，只保留右上角操作。 */
  title?: ReactNode;
  subtitle?: ReactNode;
  /** 标题上方的返回/面包屑等。 */
  leading?: ReactNode;
  /** 标题前的主播头像等。 */
  prefix?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  const hasHeading = Boolean(leading || prefix || title || subtitle);
  if (!hasHeading && !actions) return null;

  return (
    <header className={cx("flex shrink-0 items-center gap-3 px-5 pb-3 pt-4", className)}>
      {hasHeading ? (
        <div className="min-w-0">
          {leading ? <div className="mb-1">{leading}</div> : null}
          <div className="flex min-w-0 items-center gap-2.5">
            {prefix ? <div className="shrink-0">{prefix}</div> : null}
            {title ? (
              <h1 className="truncate text-[21px] font-semibold leading-7 tracking-[-0.01em] text-ink">
                {title}
              </h1>
            ) : null}
          </div>
          {subtitle ? (
            <div className="mt-0.5 text-[11.5px] leading-5 text-muted">{subtitle}</div>
          ) : null}
        </div>
      ) : null}
      {actions ? <div className="ml-auto flex shrink-0 items-center gap-2">{actions}</div> : null}
    </header>
  );
}

export function PageToolbar({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cx("flex min-h-9 shrink-0 flex-wrap items-center gap-2 px-5 pb-3", className)}>
      {children}
    </div>
  );
}

export function PageBody({
  children,
  className,
  scroll = true,
}: {
  children: ReactNode;
  className?: string;
  scroll?: boolean;
}) {
  return (
    <div
      className={cx(
        "flex min-h-0 flex-1 flex-col",
        scroll ? "overflow-auto px-5 pb-6" : "overflow-hidden",
        className,
      )}
    >
      {children}
    </div>
  );
}

/** 工具栏里的筛选胶囊，和监控页「全部 / 仅录制中」同一套状态样式。 */
export function PageChip({
  active,
  onClick,
  children,
  title,
}: {
  active?: boolean;
  onClick?: () => void;
  children: ReactNode;
  title?: string;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-pressed={active}
      onClick={onClick}
      className={cx(
        "focus-ring rounded-ctl px-2.5 py-1.5 text-[12.5px] transition-colors",
        active
          ? "bg-accent-soft font-medium text-accent-text"
          : "text-muted hover:bg-raised hover:text-ink-soft",
      )}
    >
      {children}
    </button>
  );
}

/** 页面级搜索框：监控与媒体库共用，宽度和高度保持一致。 */
export function PageSearch({
  value,
  onChange,
  placeholder,
  clearLabel = "Clear",
  className,
}: {
  value: string;
  onChange: (next: string) => void;
  placeholder: string;
  /** 清除按钮的无障碍名称。 */
  clearLabel?: string;
  className?: string;
}) {
  return (
    <SearchField
      label={placeholder}
      hideLabel
      value={value}
      onValueChange={onChange}
      placeholder={placeholder}
      clearLabel={clearLabel}
      spellCheck={false}
      size="lg"
      rootClassName={cx("w-[260px]", className)}
    />
  );
}
