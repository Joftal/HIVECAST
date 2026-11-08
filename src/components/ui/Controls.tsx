import type { ReactNode } from "react";
import { Switch as ArcSwitch } from "../arc/switch/switch";
import { Button as ArcButton, type ButtonVariant as ArcButtonVariant } from "../arc/button/button";

export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

type ButtonProps = {
  children: ReactNode;
  onClick?: () => void;
  onBlur?: () => void;
  variant?: "primary" | "secondary" | "ghost" | "danger" | "recording";
  size?: "sm" | "md" | "lg";
  disabled?: boolean;
  title?: string;
  type?: "button" | "submit";
  className?: string;
};

const BUTTON_VARIANT: Record<NonNullable<ButtonProps["variant"]>, ArcButtonVariant> = {
  primary: "accent",
  secondary: "secondary",
  ghost: "ghost",
  danger: "danger",
  recording: "recording",
};

/** 统一使用 Arc (uiarc.dev) 的 Button：按下缩放、标签切换时弹簧收放。
 *  保留项目原有的 variant / size 语义，primary 映射到品牌强调色变体。 */
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
    <ArcButton
      type={type}
      title={title}
      onClick={onClick}
      onBlur={onBlur}
      disabled={disabled}
      variant={BUTTON_VARIANT[variant]}
      size={size}
      className={className}
    >
      {children}
    </ArcButton>
  );
}

export function IconButton({
  children,
  onClick,
  label,
  disabled,
  active,
  className,
}: {
  children: ReactNode;
  onClick?: () => void;
  label: string;
  disabled?: boolean;
  active?: boolean;
  className?: string;
}) {
  return (
    <ArcButton
      type="button"
      title={label}
      aria-label={label}
      onClick={onClick}
      disabled={disabled}
      variant={active ? "secondary" : "ghost"}
      size="icon"
      className={cx("shrink-0", className)}
    >
      {children}
    </ArcButton>
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

/** 统一使用 Arc (uiarc.dev) 的 Switch：按下时 thumb 拉伸、松开后弹簧归位。
 *  保留项目原有的 onChange / label 调用方式，label 只作为 aria-label，不再在轨道旁重复显示文字。 */
export function Switch({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <ArcSwitch
      checked={checked}
      onCheckedChange={onChange}
      aria-label={label}
      disabled={disabled}
    />
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