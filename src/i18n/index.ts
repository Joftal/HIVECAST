import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { invoke } from "../lib/invoke";
import { isTauri } from "../lib/ipc";
import zhCN from "./zh-CN.json";
import enUS from "./en-US.json";

export const LANGUAGES = ["zh-CN", "en-US"] as const;
export type Language = (typeof LANGUAGES)[number];

const STORAGE_KEY = "hivecast.lang";

// 只区分中英两套，其余回落到英文。
export function detectSystemLanguage(): Language {
  const nav = navigator.language.toLowerCase();
  return nav.startsWith("zh") ? "zh-CN" : "en-US";
}

// 默认跟随系统语言；localStorage 里存的是「偏好」（具体语言或 system）。
export function detectLanguage(): Language {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === "system") return detectSystemLanguage();
  if (saved && (LANGUAGES as readonly string[]).includes(saved)) return saved as Language;
  return detectSystemLanguage();
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
  applyLanguage(lang);
}

/// 保存「跟随系统 / 具体语言」这个偏好，并立即应用解析后的语言。
export function setLanguagePreference(preference: "system" | Language) {
  localStorage.setItem(STORAGE_KEY, preference);
  applyLanguage(preference === "system" ? detectSystemLanguage() : preference);
}

function applyLanguage(lang: Language) {
  void i18n.changeLanguage(lang);
  document.documentElement.lang = lang;
  // 后端错误与系统通知也要跟着切换；浏览器预览里没有 IPC。
  if (isTauri()) {
    void invoke("ui_language_set", { language: lang }).catch(() => {});
  }
}

export default i18n;
