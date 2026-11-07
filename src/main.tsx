import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
import { logUiError } from "./lib/uiLog";
import "./i18n";
import "./styles/index.css";

// 渲染期异常由 ErrorBoundary 兜；这里补事件回调、定时器与 Promise 的漏网之鱼。
window.addEventListener("error", (event) => {
  logUiError(event.error ?? event.message, "window.error " + event.filename + ":" + event.lineno);
});
window.addEventListener("unhandledrejection", (event) => {
  logUiError(event.reason, "unhandledrejection");
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
