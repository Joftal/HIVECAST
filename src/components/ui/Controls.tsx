import type { ReactNode } from "react";

export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

type ButtonProps = {
  children: ReactNode;
  onClick?: () => void;
  onBlur?: () => void;
  variant?: "primary" | "secondary" | "ghost" | "danger" | "recording";
  size?: "sm" | "md";
  disabled?: boolean;
  title?: string;
  type?: "button" | "submit";
  className?: string;
};

const BUTTON_VARIANT: Record<NonNullable<ButtonProps["variant"]>, string> = {
  primary:
    "bg-accent text-accent-ink shadow-[0_1px_2px_rgba(16,24,40,0.12)] hover:brightness-[0.97] active:brightness-95",
  secondary: "border border-line bg-surface text-ink hover:bg-raised active:bg-raised",
  ghost: "text-muted hover:bg-raised hover:text-ink",
  danger: "border border-rec/25 bg-rec/10 text-rec hover:bg-rec/15",
  recording: "bg-rec text-white hover:brightness-[0.97] shadow-[0_1px_2px_rgba(229,72,77,0.25)]",
};

export function Button({
  children,
  onClick,
  onBlur,
  variant = "secondary",
  size = "md",
  disabled,
  title,
  type = "button",
  className,
}: ButtonProps) {
  return (
    <button
      type={type}
      title={title}
      onClick={onClick}
      onBlur={onBlur}
      disabled={disabled}
      className={cx(
        "focus-ring inline-flex shrink-0 items-center justify-center gap-1.5 rounded-ctl font-medium transition-[background-color,color,box-shadow,filter,transform] duration-150 active:scale-[0.985]",
        size === "sm" ? "h-7 px-2.5 text-[11.5px]" : "h-8 px-3 text-[12px]",
        BUTTON_VARIANT[variant],
        disabled ? "pointer-events-none opacity-45" : "",
        className,
      )}
    >
      {children}
    </button>
  );
}

export function IconButton({
  children,
  onClick,
  label,
  disabled,
  active,
}: {
  children: ReactNode;
  onClick?: () => void;
  label: string;
  disabled?: boolean;
  active?: boolean;
}) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      onClick={onClick}
      disabled={disabled}
      className={cx(
        "focus-ring flex h-7 w-7 shrink-0 items-center justify-center rounded-ctl transition-colors duration-150",
        active ? "bg-raised text-ink" : "text-faint hover:bg-raised hover:text-ink",
        disabled ? "pointer-events-none opacity-40" : "",
      )}
    >
      {children}
    </button>
  );
}

type Tone = "neutral" | "accent" | "live" | "rec" | "warn";

const CHIP_TONE: Record<Tone, string> = {
  neutral: "bg-raised text-muted",
  accent: "bg-accent-soft text-accent-text",
  live: "bg-live/12 text-live",
  rec: "bg-rec/12 text-rec",
  warn: "bg-warn/12 text-warn",
};

export function Chip({
  children,
  tone = "neutral",
  dot,
  className,
}: {
  children: ReactNode;
  tone?: Tone;
  dot?: boolean;
  className?: string;
}) {
  return (
    <span
      className={cx(
        "inline-flex shrink-0 items-center gap-1.5 rounded-full px-2 py-[3px] text-[10.5px] font-medium leading-none",
        CHIP_TONE[tone],
        className,
      )}
    >
      {dot ? <span className="h-[5px] w-[5px] rounded-full bg-current" /> : null}
      {children}
    </span>
  );
}

export function Switch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      title={label}
      onClick={() => onChange(!checked)}
      className={cx(
        "focus-ring relative h-[18px] w-[30px] shrink-0 rounded-full transition-colors duration-200",
        checked ? "bg-accent" : "bg-line-strong",
      )}
    >
      <span
        className={cx(
          "absolute top-[2px] h-[14px] w-[14px] rounded-full bg-white shadow-sm transition-transform duration-200 ease-[var(--ease-spring)]",
          checked ? "translate-x-[14px]" : "translate-x-[2px]",
        )}
      />
    </button>
  );
}

export function Segmented<T extends string>({
  items,
  value,
  onChange,
}: {
  items: Array<{ key: T; label: string }>;
  value: T;
  onChange: (next: T) => void;
}) {
  const index = Math.max(0, items.findIndex((item) => item.key === value));
  return (
    <div
      role="tablist"
      className="relative grid w-fit rounded-[10px] bg-raised p-[3px] text-[11.5px]"
      style={{ gridTemplateColumns: `repeat(${items.length}, minmax(0, 1fr))` }}
    >
      <span
        aria-hidden
        className="pointer-events-none absolute inset-y-[3px] left-[3px] rounded-[7px] bg-surface shadow-[0_1px_2px_rgba(16,24,40,0.1),0_0_0_1px_rgba(16,24,40,0.04)] transition-transform duration-[260ms] ease-[var(--ease-spring)]"
        style={{
          width: `calc((100% - 6px) / ${items.length})`,
          transform: `translateX(${index * 100}%)`,
        }}
      />
      {items.map((item) => (
        <button
          key={item.key}
          type="button"
          role="tab"
          aria-selected={value === item.key}
          onClick={() => onChange(item.key)}
          className={cx(
            "focus-ring relative z-10 rounded-[7px] px-3 py-[5px] font-medium transition-colors duration-200",
            value === item.key ? "text-ink" : "text-muted hover:text-ink-soft",
          )}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}

export function Select({
  value,
  onChange,
  options,
  title,
  disabled,
  className,
}: {
  value: string;
  onChange: (next: string) => void;
  options: Array<{ value: string; label: string }>;
  title?: string;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <div className={cx("relative inline-flex", className)}>
      <select
        value={value}
        title={title}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        className={cx(
          "focus-ring h-7 w-full cursor-pointer appearance-none rounded-ctl border border-line bg-surface pl-2 pr-6 text-[11.5px] text-ink-soft transition-colors hover:border-line-strong disabled:pointer-events-none disabled:opacity-45",
        )}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <svg
        width="10"
        height="10"
        viewBox="0 0 12 12"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
        className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-faint"
        aria-hidden
      >
        <path d="M3 4.5 6 7.5 9 4.5" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </div>
  );
}

export function TextField({
  value,
  onChange,
  placeholder,
  onSubmit,
  className,
}: {
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
  onSubmit?: () => void;
  className?: string;
}) {
  return (
    <input
      value={value}
      onChange={(event) => onChange(event.target.value)}
      placeholder={placeholder}
      spellCheck={false}
      onKeyDown={(event) => {
        if (event.key === "Enter" && onSubmit) onSubmit();
      }}
      className={cx(
        "focus-ring h-8 w-full rounded-ctl border border-line bg-surface px-2.5 text-[12px] text-ink transition-colors placeholder:text-faint hover:border-line-strong",
        className,
      )}
    />
  );
}