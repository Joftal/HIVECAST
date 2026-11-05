//! 界面密度等「纯显示」偏好：不写数据库，换设备重新选即可。
import { create } from "zustand";
import { persist } from "zustand/middleware";

export type Density = "comfortable" | "compact";

type Store = {
  density: Density;
  setDensity: (density: Density) => void;
};

export const useUiPrefs = create<Store>()(
  persist(
    (set) => ({
      density: "comfortable",
      setDensity: (density) => set({ density }),
    }),
    { name: "hivecast.ui" },
  ),
);
