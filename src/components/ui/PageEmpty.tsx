import type { EmptyStateProps } from "../arc/empty-state/empty-state";
import EmptyState from "../arc/empty-state/empty-state";

/** 统一空态：占满剩余空间并在其中垂直/水平居中，避免不同页面的父容器结构导致高度不一致。 */
export default function PageEmpty({ className, ...props }: EmptyStateProps & { className?: string }) {
  return (
    <div className={["flex min-h-0 items-center justify-center", className ?? ""].join(" ")}>
      <EmptyState {...props} />
    </div>
  );
}
