// i18n bootstrap for Tokoru. Uses react-i18next with the browser-language
// detector for the first-launch default, then falls back to whatever the
// user explicitly picked in Settings (persisted to `tokoru_locale` in
// localStorage AND to `app_locale` in sync_state so the backend can read
// it for outgoing Steam/GOG API calls).

import i18n from "i18next";
import LanguageDetector from "i18next-browser-languagedetector";
import { initReactI18next } from "react-i18next";

import en from "../locales/en/common.json";
import fr from "../locales/fr/common.json";
import es from "../locales/es/common.json";
import de from "../locales/de/common.json";
import it from "../locales/it/common.json";
import pt from "../locales/pt/common.json";
import ru from "../locales/ru/common.json";
import zh from "../locales/zh/common.json";
import ja from "../locales/ja/common.json";
import ko from "../locales/ko/common.json";

/// Locales Tokoru ships with. Order doubles as the order shown in the
/// Settings → Language picker. Native names match how each language
/// refers to itself (helps users who don't read English find their own).
export const SUPPORTED_LOCALES = [
  { code: "en", native: "English" },
  { code: "fr", native: "Français" },
  { code: "es", native: "Español" },
  { code: "de", native: "Deutsch" },
  { code: "it", native: "Italiano" },
  { code: "pt", native: "Português" },
  { code: "ru", native: "Русский" },
  { code: "zh", native: "中文" },
  { code: "ja", native: "日本語" },
  { code: "ko", native: "한국어" },
] as const;

export type LocaleCode = (typeof SUPPORTED_LOCALES)[number]["code"];

const STORAGE_KEY = "tokoru_locale";

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      en: { common: en },
      fr: { common: fr },
      es: { common: es },
      de: { common: de },
      it: { common: it },
      pt: { common: pt },
      ru: { common: ru },
      zh: { common: zh },
      ja: { common: ja },
      ko: { common: ko },
    },
    fallbackLng: "en",
    defaultNS: "common",
    interpolation: {
      // React already escapes — i18next double-escaping would garble
      // strings that legitimately contain `<`, `&`, etc.
      escapeValue: false,
    },
    detection: {
      // Order: explicit user pick (localStorage) > browser/OS locale
      order: ["localStorage", "navigator"],
      lookupLocalStorage: STORAGE_KEY,
      caches: ["localStorage"],
    },
    supportedLngs: SUPPORTED_LOCALES.map((l) => l.code),
    // `en-US` etc. fold to `en` since we don't ship regional variants
    // (PT here is pt-BR but i18next treats it as plain "pt").
    nonExplicitSupportedLngs: true,
    load: "languageOnly",
  });

export default i18n;
