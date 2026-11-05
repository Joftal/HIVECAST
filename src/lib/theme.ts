export type ThemeMode = "light" | "dark" | "system";

const QUERY = "(prefers-color-scheme: dark)";

export function resolveTheme(mode: ThemeMode): "light" | "dark" {
  if (mode === "system") {
    if (typeof window === "undefined" || !window.matchMedia) return "light";
    return window.matchMedia(QUERY).matches ? "dark" : "light";
  }
  return mode;
}

/** 把解析后的主题写到 <html data-theme>，CSS 变量据此切换。 */
export function applyTheme(mode: ThemeMode): "light" | "dark" {
  const resolved = resolveTheme(mode);
  document.documentElement.dataset.theme = resolved;
  return resolved;
}

/** 跟随系统时监听变化；返回取消订阅函数。 */
export function watchSystemTheme(onChange: (resolved: "light" | "dark") => void): () => void {
  if (!window.matchMedia) return () => undefined;
  const media = window.matchMedia(QUERY);
  const handler = () => onChange(media.matches ? "dark" : "light");
  media.addEventListener("change", handler);
  return () => media.removeEventListener("change", handler);
}