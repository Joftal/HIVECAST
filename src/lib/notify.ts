import { useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useToastStack } from "../components/arc/toast-stack/toast-stack";
import { describeError } from "./ipc";

/** 统一提示入口：页面里的错误、成功、警告都走顶部中间的 ToastStack。 */
export function useNotify() {
  const { toast, update, dismiss } = useToastStack();
  const { t } = useTranslation();
  // 语言切换会让 t 换身份；用 ref 读最新翻译，避免 notify 对象跟着变，
  // 把依赖 notify 的页面 effect（例如设置页 load）误触发成重新拉取。
  const tRef = useRef(t);
  tRef.current = t;

  return useMemo(
    () => ({
      error: (message: unknown, title?: string) =>
        toast({
          type: "error",
          title: title ?? tRef.current("toast.errorTitle"),
          description: describeError(message),
        }),
      warning: (message: unknown, title?: string) =>
        toast({
          type: "warning",
          title: title ?? tRef.current("toast.warningTitle"),
          description: describeError(message),
        }),
      info: (title: string, description?: string) =>
        toast({ type: "info", title, description }),
      success: (title: string, description?: string) =>
        toast({ type: "success", title, description }),
      toast,
      update,
      dismiss,
    }),
    [dismiss, toast, update],
  );
}
