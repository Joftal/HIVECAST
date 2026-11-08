import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** 合并条件类名并消除 Tailwind 冲突，shadcn / Arc 组件的通用入口。 */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
