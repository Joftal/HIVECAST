//! 观看页的显示偏好：紧凑模式只影响布局，不写数据库。
//!
//! 为什么放前端：这是「这台机器怎么看」的偏好，换设备本来就该重新选；
//! 存 localStorage 比走 app_setting 表少一次 IPC，也不用重新导出类型。
import { create } from "zustand";
import { persist } from "zustand/middleware";

type Store = {
  /** 紧凑：去掉外圈与格子之间的缝，画面彼此相接。 */
  compact: boolean;
  setCompact: (compact: boolean) => void;
};

export const useWatchPrefs = create<Store>()(
  persist(
    (set) => ({
      compact: false,
      setCompact: (compact) => set({ compact }),
    }),
    { name: "hivecast.watch" },
  ),
);
