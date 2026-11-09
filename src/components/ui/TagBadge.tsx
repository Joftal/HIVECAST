import type { CSSProperties } from "react";
import { cx } from "./Controls";

/** 同一个标签在列表、抽屉和筛选里保持相同色相，方便快速区分。 */
function tagHue(tag: string): number {
  let hash = 0;
  for (let index = 0; index < tag.length; index += 1) {
    hash = (hash * 31 + tag.charCodeAt(index)) % 360;
  }
  return hash;
}

export default function TagBadge({
  tag,
  className,
  title,
}: {
  tag: string;
  className?: string;
  title?: string;
}) {
  return (
    <span
      className={cx("tag-badge", className)}
      style={{ "--tag-h": tagHue(tag) } as CSSProperties}
      title={title ?? tag}
    >
      {tag}
    </span>
  );
}
