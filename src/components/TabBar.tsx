type Item = { key: string; label: string };

type Props = {
  items: Item[];
  value: string;
  onChange: (value: string) => void;
};

// 分段切换器：指示块用 transform 滑动，避免切换时整屏重排。
// 段数由调用方决定（M6 起有监控/观看/媒体库三段）。
export default function TabBar({ items, value, onChange }: Props) {
  const index = Math.max(0, items.findIndex((item) => item.key === value));
  const count = items.length;
  return (
    <div
      className="relative grid rounded-full border hairline bg-ink-800/70 p-1 text-[13px]"
      style={{ gridTemplateColumns: `repeat(${count}, minmax(0, 1fr))` }}
    >
      <span
        aria-hidden
        className="pointer-events-none absolute inset-y-1 left-1 rounded-full bg-ink-700 shadow-[0_1px_0_rgba(255,255,255,0.06)_inset,0_6px_18px_rgba(0,0,0,0.35)] transition-transform duration-300 ease-[cubic-bezier(0.22,1,0.36,1)]"
        style={{
          width: `calc((100% - 8px) / ${count})`,
          transform: `translateX(${index * 100}%)`,
        }}
      />
      {items.map((item) => (
        <button
          key={item.key}
          type="button"
          onClick={() => onChange(item.key)}
          aria-pressed={value === item.key}
          className={[
            "focus-ring relative z-10 rounded-full px-4 py-1.5 font-medium transition-colors duration-200",
            value === item.key ? "text-white" : "text-zinc-400 hover:text-zinc-200",
          ].join(" ")}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
