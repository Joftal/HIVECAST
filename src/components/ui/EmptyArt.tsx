export type EmptyArtKind = "monitor" | "watch" | "media" | "tasks" | "search" | "gone";

/** 各 Tab 的空态插画：默认 24px 线性图标，内部只做 transform/opacity 动画，不参与布局。 */
export default function EmptyArt({ kind, size = 24 }: { kind: EmptyArtKind; size?: number }) {
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.5,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    "aria-hidden": true,
  };

  if (kind === "monitor") {
    return (
      <svg {...common}>
        <rect x="2.5" y="4.5" width="19" height="12.5" rx="2" />
        <path d="M9 20.5h6" />
        <path className="empty-art-scan" d="M5 8h14" stroke="var(--c-accent)" opacity="0.22" />
        <circle className="empty-art-pulse" cx="5.8" cy="7" r="0.9" fill="var(--c-rec)" stroke="none" />
        <path d="M10.4 8.6l3.4 2.1-3.4 2.1z" fill="var(--c-accent)" stroke="none" />
      </svg>
    );
  }

  if (kind === "watch") {
    return (
      <svg {...common}>
        <rect x="2.5" y="2.5" width="8.5" height="8.5" rx="1.8" />
        <rect x="13" y="2.5" width="8.5" height="8.5" rx="1.8" />
        <rect x="2.5" y="13" width="8.5" height="8.5" rx="1.8" />
        <rect
          className="empty-art-pulse"
          x="13"
          y="13"
          width="8.5"
          height="8.5"
          rx="1.8"
          fill="var(--c-accent)"
          stroke="none"
        />
      </svg>
    );
  }

  if (kind === "media") {
    return (
      <svg {...common}>
        <rect x="2.5" y="4" width="19" height="16" rx="2" />
        <path d="M8 4v16M16 4v16" opacity="0.35" />
        <path d="M2.5 8h5.5M2.5 12h5.5M2.5 16h5.5M16 8h5.5M16 12h5.5M16 16h5.5" opacity="0.35" />
        <path
          className="empty-art-pulse"
          d="M10.5 9.4l4.1 2.6-4.1 2.6z"
          fill="var(--c-accent)"
          stroke="none"
        />
      </svg>
    );
  }

  if (kind === "tasks") {
    return (
      <svg {...common}>
        <g className="empty-art-task-row empty-art-task-row-1">
          <circle className="empty-art-task-dot" cx="4.2" cy="6.4" r="1.65" />
          <path className="empty-art-task-line" d="M9 6.5h11" />
        </g>
        <g className="empty-art-task-row empty-art-task-row-2">
          <circle className="empty-art-task-dot" cx="4.2" cy="12" r="1.65" />
          <path className="empty-art-task-line" d="M9 12h11" />
        </g>
        <g className="empty-art-task-row empty-art-task-row-3">
          <circle className="empty-art-task-dot" cx="4.2" cy="17.6" r="1.65" />
          <path className="empty-art-task-line" d="M9 17.5h7" />
        </g>
      </svg>
    );
  }

  if (kind === "gone") {
    return (
      <svg {...common}>
        <rect x="2.5" y="4" width="19" height="16" rx="2" opacity="0.55" />
        <path d="M8 4v16M16 4v16" opacity="0.25" />
        <path d="M4 20L20 4" stroke="var(--c-rec)" opacity="0.75" />
      </svg>
    );
  }

  return (
    <svg {...common}>
      <circle cx="10.5" cy="10.5" r="5.5" />
      <path d="M14.8 14.8l4.7 4.7" />
      <path className="empty-art-scan" d="M6.5 10.5h8" stroke="var(--c-accent)" opacity="0.25" />
    </svg>
  );
}
