/**
 * Locale registry: adding a new language requires only:
 * 1. Adding a folder under `src/i18n/locales/<code>/`
 * 2. Registering its metadata & catalog below.
 */
import type { LocaleMeta } from "./engine.ts";
import { en, type EnMessages, type LocaleCatalog } from "./locales/en/index.ts";
import { zhCN } from "./locales/zh-CN/index.ts";

export const SUPPORTED_LOCALES = [
  {
    code: "en",
    nativeName: "English",
    englishName: "English",
    dir: "ltr",
    matchPrefixes: ["en"],
  },
  {
    code: "zh-CN",
    nativeName: "简体中文",
    englishName: "Chinese (Simplified)",
    dir: "ltr",
    matchPrefixes: ["zh", "zh-hans", "zh-cn", "zh-sg"],
  },
] as const satisfies readonly LocaleMeta[];

export type Locale = (typeof SUPPORTED_LOCALES)[number]["code"];

export const FALLBACK_LOCALE: Locale = "en";

export const CATALOGS: Record<Locale, LocaleCatalog> = {
  en,
  "zh-CN": zhCN,
};

export type { EnMessages, LocaleCatalog };
