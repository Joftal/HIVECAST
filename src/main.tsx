import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
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

const lab = import.meta.env.DEV && new URLSearchParams(window.location.search).has("arc-lab");
const ArcLab = React.lazy(() => import("./features/design/ArcLab"));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      {lab ? (
        <React.Suspense fallback={null}>
          <ArcLab />
        </React.Suspense>
      ) : (
        <App />
      )}
    </ErrorBoundary>
  </React.StrictMode>,
);
