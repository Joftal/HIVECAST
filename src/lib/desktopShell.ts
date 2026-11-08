import { isTauri } from "./ipc";

/** 桌面壳：收掉 WebView2 的浏览器默认行为。
 *  右键菜单、F12 / Ctrl+Shift+I/J/C、Ctrl+R/P/U 都不该出现在桌面应用里。 */
export function installDesktopShellGuards(): void {
  if (!isTauri()) return;
  document.documentElement.dataset.desktop = "";

  window.addEventListener(
    "contextmenu",
    (event) => {
      event.preventDefault();
    },
    { capture: true },
  );

  window.addEventListener(
    "keydown",
    (event) => {
      const key = event.key.toLowerCase();
      const mod = event.ctrlKey || event.metaKey;
      const devtools =
        key === "f12" || (mod && event.shiftKey && (key === "i" || key === "j" || key === "c"));
      const browser =
        key === "f5" || (mod && (key === "u" || key === "p" || key === "r"));
      const zoom = mod && (key === "+" || key === "=" || key === "-" || key === "0");
      if (!devtools && !browser && !zoom) return;
      event.preventDefault();
      event.stopPropagation();
    },
    { capture: true },
  );

  // WebView 的 Ctrl+滚轮会被当成页面缩放，桌面端固定 UI 比例。
  window.addEventListener(
    "wheel",
    (event) => {
      if (!event.ctrlKey && !event.metaKey) return;
      event.preventDefault();
    },
    { capture: true, passive: false },
  );
}
