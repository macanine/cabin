import { useSyncExternalStore } from "react";
import { createI18n, type LocalePref as EngineLocalePref, type ParamsFor } from "./engine.ts";
import { CATALOGS, FALLBACK_LOCALE, SUPPORTED_LOCALES, type EnMessages, type Locale } from "./registry.ts";
import type { MessageKey } from "./locales/en/index.ts";

export type { Locale, MessageKey };
export type LocalePref = EngineLocalePref<Locale>;
/** Message keys that take zero interpolation variables (safe for lookup tables). */
export type StaticMessageKey = {
  [K in MessageKey]: ParamsFor<EnMessages[K]> extends [] ? K : never;
}[MessageKey];
export { SUPPORTED_LOCALES };

export const i18n = createI18n<Locale, EnMessages>({
  locales: SUPPORTED_LOCALES,
  fallback: FALLBACK_LOCALE,
  catalogs: CATALOGS as Record<Locale, EnMessages>,
  storageKey: "cabin.locale",
});

export const getLocale = i18n.getLocale;
export const getLocalePref = i18n.getPref;
export const getLocaleDir = i18n.getDir;
export const setLocalePref = i18n.setPref;
export const subscribeLocale = i18n.subscribe;
export const detectSystemLocale = i18n.detectSystemLocale;

/**
 * Compile-time type-safe translator:
 * - Parameterless keys forbid extra arguments.
 * - Keys with `{var}` or ICU `{count, plural, ...}` enforce exact variable names.
 */
export function t<K extends MessageKey>(key: K, ...args: ParamsFor<EnMessages[K]>): string {
  return i18n.t(key, ...args);
}

/** Reactive locale selector (lightweight; ideal for date/time formatting). */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribeLocale, getLocale, () => FALLBACK_LOCALE);
}

/** Reactive React hook returning a locale-bound `t()`, active locale, preference, and setter. */
export function useI18n() {
  const locale = useLocale();
  const pref = useSyncExternalStore(subscribeLocale, getLocalePref, () => "auto" as LocalePref);
  const dir = getLocaleDir(locale);

  function translate<K extends MessageKey>(key: K, ...args: ParamsFor<EnMessages[K]>): string {
    return i18n.tFor(locale, key, ...args);
  }

  return {
    locale,
    pref,
    dir,
    locales: SUPPORTED_LOCALES,
    setLocalePref,
    t: translate,
  };
}
