import { Component, type ErrorInfo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { logUiError } from "../lib/uiLog";

function Fallback({ detail }: { detail: string }) {
  const { t } = useTranslation();
  return (
    <div className="flex h-full items-center justify-center bg-canvas p-8 text-ink">
      <div className="max-w-[520px] space-y-3">
        <h1 className="text-[15px] font-semibold">{t("app.errorTitle")}</h1>
        <p className="text-[12px] leading-5 text-muted">{t("app.errorBody")}</p>
        <pre className="max-h-40 overflow-auto rounded-ctl border border-line bg-raised p-3 font-mono text-[10.5px] leading-4 text-faint">
          {detail}
        </pre>
        <button
          type="button"
          onClick={() => window.location.reload()}
          className="focus-ring rounded-ctl bg-accent px-3 py-1.5 text-[12px] font-medium text-white"
        >
          {t("app.reload")}
        </button>
      </div>
    </div>
  );
}

type State = { error: Error | null };

// 渲染期异常不会走 window.onerror，必须用边界兜住，否则整页白屏且没有任何日志。
export default class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    logUiError(error, "react:" + (info.componentStack ?? "").slice(0, 2000));
  }

  render() {
    if (this.state.error) return <Fallback detail={this.state.error.stack ?? this.state.error.message} />;
    return this.props.children;
  }
}
