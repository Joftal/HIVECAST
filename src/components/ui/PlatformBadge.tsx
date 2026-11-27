import type { CSSProperties } from "react";
import bilibiliIcon from "../../assets/platforms/bilibili.png";
import douyuIcon from "../../assets/platforms/douyu.png";
import huyaIcon from "../../assets/platforms/huya.png";
import douyinIcon from "../../assets/platforms/douyin.png";
import { cx } from "./Controls";

export type PlatformVisual = {
  brand: string;
  brandDark: string;
  keywords: string;
  iconSrc?: string;
};

const FALLBACK_VISUAL: PlatformVisual = {
  brand: "#2f6bff",
  brandDark: "#60a5fa",
  keywords: "",
};

export const PLATFORM_VISUALS: Record<string, PlatformVisual> = {
  bilibili: {
    brand: "#FB7299",
    brandDark: "#FF85A9",
    keywords: "b站 bilibili bl 哔哩哔哩",
    iconSrc: bilibiliIcon,
  },
  douyu: {
    brand: "#FF6D00",
    brandDark: "#FF8F33",
    keywords: "douyu dy 斗鱼",
    iconSrc: douyuIcon,
  },
  huya: {
    brand: "#E59400",
    brandDark: "#F5B841",
    keywords: "huya hy 虎牙",
    iconSrc: huyaIcon,
  },
  douyin: {
    brand: "#1F2329",
    brandDark: "#E6E9EF",
    keywords: "douyin dy tiktok 抖音",
    iconSrc: douyinIcon,
  },
};

export function getPlatformVisual(platform: string): PlatformVisual {
  return PLATFORM_VISUALS[platform] ?? FALLBACK_VISUAL;
}

export function platformBrandStyle(platform: string): CSSProperties {
  const v = getPlatformVisual(platform);
  return {
    "--brand": v.brand,
    "--brand-dark": v.brandDark,
  } as CSSProperties;
}

export function PlatformBadge({
  platform,
  size = "md",
  className,
}: {
  platform: string;
  size?: "sm" | "md";
  className?: string;
}) {
  const visual = getPlatformVisual(platform);
  const sizeClass = size === "sm" ? "h-9 w-9 rounded-[9px]" : "h-10 w-10 rounded-[10px]";

  if (visual.iconSrc) {
    return (
      <span
        aria-hidden
        style={platformBrandStyle(platform)}
        className={cx(
          "platform-icon-frame relative flex shrink-0 items-center justify-center overflow-hidden bg-surface",
          sizeClass,
          className,
        )}
      >
        <img
          src={visual.iconSrc}
          alt=""
          draggable={false}
          className="h-full w-full object-cover"
        />
      </span>
    );
  }

  return (
    <span
      aria-hidden
      style={platformBrandStyle(platform)}
      className={cx(
        "platform-badge flex shrink-0 items-center justify-center",
        sizeClass,
        className,
      )}
    >
      <svg
        width="20"
        height="20"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <circle cx="12" cy="12" r="7.5" />
        <polygon points="10.5 9 15.5 12 10.5 15" fill="currentColor" stroke="none" />
      </svg>
    </span>
  );
}
