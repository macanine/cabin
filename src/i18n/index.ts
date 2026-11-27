import { useSyncExternalStore } from "react";
import { en, type MessageKey } from "./en.ts";
import { zhCN } from "./zh-CN.ts";

export type { MessageKey };
export type Locale = "en" | "zh-CN";
export type LocalePref = "auto" | Locale;

const STORAGE_KEY = "cabin.locale";
const MESSAGES: Record<Locale, Record<MessageKey, string>> = {
  en,
  "zh-CN": zhCN,
};

export function detectSystemLocale(): Locale {
  if (typeof navigator === "undefined") return "en";
  const langs = [...(navigator.languages ?? []), navigator.language].filter(Boolean);
  for (const lang of langs) {
    if (lang.toLowerCase().startsWith("zh")) return "zh-CN";
  }
  return "en";
}

export function resolveLocale(pref: LocalePref): Locale {
  return pref === "auto" ? detectSystemLocale() : pref;
}

function readStoredPref(): LocalePref {
  try {
    const raw = typeof localStorage !== "undefined" ? localStorage.getItem(STORAGE_KEY) : null;
    if (raw === "en" || raw === "zh-CN" || raw === "auto") return raw;
  } catch {
    // Ignore restricted storage.
  }
  return "auto";
}

let currentPref: LocalePref = readStoredPref();
let currentLocale: Locale = resolveLocale(currentPref);
const listeners = new Set<() => void>();

function syncDocumentLang() {
  if (typeof document !== "undefined" && document.documentElement) {
    document.documentElement.lang = currentLocale;
  }
}
syncDocumentLang();

function notify() {
  listeners.forEach((listener) => listener());
}

export function getLocalePref(): LocalePref {
  return currentPref;
}

export function getLocale(): Locale {
  return currentLocale;
}

export function setLocalePref(pref: LocalePref): void {
  const nextLocale = resolveLocale(pref);
  if (pref === currentPref && nextLocale === currentLocale) return;
  currentPref = pref;
  currentLocale = nextLocale;
  try {
    if (typeof localStorage !== "undefined") localStorage.setItem(STORAGE_KEY, pref);
  } catch {
    // Ignore restricted storage.
  }
  syncDocumentLang();
  notify();
}

export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Translate a typed message key with optional `{var}` interpolation. */
export function t(key: MessageKey, vars?: Record<string, string | number>, locale: Locale = currentLocale): string {
  const template = MESSAGES[locale][key] ?? MESSAGES.en[key] ?? key;
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (_, name: string) =>
    Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : `{${name}}`,
  );
}

/** Reactive hook for components: re-renders automatically when the user switches language. */
export function useI18n() {
  const locale = useSyncExternalStore(subscribeLocale, getLocale, () => "en" as Locale);
  const pref = useSyncExternalStore(subscribeLocale, getLocalePref, () => "auto" as LocalePref);
  return {
    locale,
    pref,
    setLocalePref,
    t: (key: MessageKey, vars?: Record<string, string | number>) => t(key, vars, locale),
  };
}
