import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import zhCN from "./zh-CN.json";
import enUS from "./en-US.json";

export const LANGUAGES = ["zh-CN", "en-US"] as const;
export type Language = (typeof LANGUAGES)[number];

const STORAGE_KEY = "hivecast.lang";

// 默认跟随系统语言；只区分中英两套，其余回落到英文。
export function detectLanguage(): Language {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved && (LANGUAGES as readonly string[]).includes(saved)) return saved as Language;
  const nav = navigator.language.toLowerCase();
  return nav.startsWith("zh") ? "zh-CN" : "en-US";
}

void i18n.use(initReactI18next).init({
  resources: {
    "zh-CN": { translation: zhCN },
    "en-US": { translation: enUS },
  },
  lng: detectLanguage(),
  fallbackLng: "en-US",
  interpolation: { escapeValue: false },
  returnNull: false,
});

export function setLanguage(lang: Language) {
  localStorage.setItem(STORAGE_KEY, lang);
  void i18n.changeLanguage(lang);
  document.documentElement.lang = lang;
}

export default i18n;
