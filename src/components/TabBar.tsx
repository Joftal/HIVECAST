type TabKey = "monitor" | "watch";

type Props = {
  value: TabKey;
  onChange: (value: TabKey) => void;
  labels: Record<TabKey, string>;
};

// 两段式切换器：指示块用 transform 滑动，避免切换时整屏重排。
export default function TabBar({ value, onChange, labels }: Props) {
  const index = value === "monitor" ? 0 : 1;
  return (
    <div className="relative grid grid-cols-2 rounded-full border hairline bg-ink-800/70 p-1 text-[13px]">
      <span
        aria-hidden
        className="pointer-events-none absolute inset-y-1 left-1 w-[calc(50%-4px)] rounded-full bg-ink-700 shadow-[0_1px_0_rgba(255,255,255,0.06)_inset,0_6px_18px_rgba(0,0,0,0.35)] transition-transform duration-300 ease-[cubic-bezier(0.22,1,0.36,1)]"
        style={{ transform: index === 0 ? "translateX(0)" : "translateX(100%)" }}
      />
      {(["monitor", "watch"] as const).map((key) => (
        <button
          key={key}
          type="button"
          onClick={() => onChange(key)}
          aria-pressed={value === key}
          className={[
            "focus-ring relative z-10 rounded-full px-5 py-1.5 font-medium transition-colors duration-200",
            value === key ? "text-white" : "text-zinc-400 hover:text-zinc-200",
          ].join(" ")}
        >
          {labels[key]}
        </button>
      ))}
    </div>
  );
}
