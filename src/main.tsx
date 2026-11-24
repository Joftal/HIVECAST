import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
import { ToastStackProvider } from "./components/arc/toast-stack/toast-stack";
import { logUiError } from "./lib/uiLog";
import { installDesktopShellGuards } from "./lib/desktopShell";
import "./i18n";
import "@/components/arc/foundation.css";
import "./styles/index.css";
import "./styles/arc-bridge.css";

installDesktopShellGuards();

// 渲染期异常由 ErrorBoundary 兜；这里补事件回调、定时器与 Promise 的漏网之鱼。
window.addEventListener("error", (event) => {
  logUiError(event.error ?? event.message, "window.error " + event.filename + ":" + event.lineno);
});
window.addEventListener("unhandledrejection", (event) => {
  logUiError(event.reason, "unhandledrejection");
});

// 实验页只在开发构建里存在：用编译期常量包住动态 import，生产包不会
// 再拆出 ArcLab/PlayerLab chunk（文档 196-23）。
let Lab: React.LazyExoticComponent<React.ComponentType> | null = null;
if (import.meta.env.DEV) {
  const params = new URLSearchParams(window.location.search);
  if (params.has("player-lab")) {
    Lab = React.lazy(() => import("./features/design/PlayerLab"));
  } else if (params.has("arc-lab")) {
    Lab = React.lazy(() => import("./features/design/ArcLab"));
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ToastStackProvider>
      <ErrorBoundary>
        {Lab ? (
          <React.Suspense fallback={null}>
            <Lab />
          </React.Suspense>
        ) : (
          <App />
        )}
      </ErrorBoundary>
    </ToastStackProvider>
  </React.StrictMode>,
);
