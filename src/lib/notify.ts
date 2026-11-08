import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useToastStack } from "../components/arc/toast-stack/toast-stack";
import { describeError } from "./ipc";

/** 统一提示入口：页面里的错误、成功、警告都走顶部中间的 ToastStack。 */
export function useNotify() {
  const { toast, update, dismiss } = useToastStack();
  const { t } = useTranslation();

  return useMemo(
    () => ({
      error: (message: unknown, title?: string) =>
        toast({
          type: "error",
          title: title ?? t("toast.errorTitle"),
          description: describeError(message),
        }),
      warning: (message: unknown, title?: string) =>
        toast({
          type: "warning",
          title: title ?? t("toast.warningTitle"),
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
    [dismiss, t, toast, update],
  );
}
