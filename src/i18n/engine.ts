/**
 * Framework-agnostic, portable i18n engine.
 *
 * Features:
 * - Zero runtime dependencies (uses standard Web `Intl.PluralRules` & `Intl.DateTimeFormat`)
 * - Compile-time parameter inference from `{var}` and ICU `{count, plural, ...}` templates
 * - Balanced-brace ICU plural selector supporting exact `=0` / `=1` matches and CLDR categories
 * - BCP-47 locale negotiation (`zh-Hans-CN`, `zh-SG`, `en-GB`, etc.)
 * - Observable preference store with optional persistence and `<html lang dir>` sync
 */

export type TextDirection = "ltr" | "rtl";

export interface LocaleMeta<L extends string = string> {
  /** Canonical BCP-47 locale tag, e.g. `"en"`, `"zh-CN"`. */
  readonly code: L;
  /** Native autonym shown in language pickers, e.g. `"English"`, `"简体中文"`. */
  readonly nativeName: string;
  /** English name for accessibility & search. */
  readonly englishName: string;
  /** Writing direction. */
  readonly dir: TextDirection;
  /** Extra BCP-47 prefixes/aliases that negotiate to this locale (lowercased). */
  readonly matchPrefixes?: readonly string[];
}

export type LocalePref<L extends string> = "auto" | L;

// ---- Compile-time placeholder extraction -----------------------------------

type Trim<S extends string> =
  S extends ` ${infer R}` ? Trim<R> : S extends `${infer R} ` ? Trim<R> : S;

type ValidParamName<S extends string> =
  S extends "" | `#${string}` | `${string} ${string}` | `${string}{${string}` | `${string}}${string}`
    ? never
    : S;

/** Extracts placeholder variable names from `{var}` and `{count, plural, ...}` templates. */
export type ExtractParams<S extends string> =
  S extends `${string}{${infer Inner}}${infer Rest}`
    ? (Inner extends `${infer Name}, plural,${infer AfterPlural}`
        ? ValidParamName<Trim<Name>> | ExtractParams<AfterPlural> | ExtractParams<Rest>
        : Inner extends `${infer Name},${string}`
          ? ValidParamName<Trim<Name>> | ExtractParams<Rest>
          : ValidParamName<Trim<Inner>> | ExtractParams<Rest>)
    : never;

export type ParamsFor<Template extends string> =
  [ExtractParams<Template>] extends [never]
    ? []
    : [vars: Record<ExtractParams<Template>, string | number>];

// ---- ICU plural & interpolation runtime ------------------------------------

const pluralRulesCache = new Map<string, Intl.PluralRules>();

function getPluralRules(locale: string): Intl.PluralRules {
  let rules = pluralRulesCache.get(locale);
  if (!rules) {
    rules = new Intl.PluralRules(locale);
    pluralRulesCache.set(locale, rules);
  }
  return rules;
}

/** Find matching `}` index for an opening `{` at `openIndex`, or -1 if unbalanced. */
function findMatchingBrace(text: string, openIndex: number): number {
  let depth = 0;
  for (let i = openIndex; i < text.length; i += 1) {
    if (text[i] === "{") depth += 1;
    else if (text[i] === "}") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/** Parse `=0 {none} one {# item} other {# items}` branches inside a plural expression. */
function parsePluralBranches(body: string): Map<string, string> {
  const branches = new Map<string, string>();
  let i = 0;
  while (i < body.length) {
    while (i < body.length && /\s/.test(body[i])) i += 1;
    if (i >= body.length) break;
    const keyStart = i;
    while (i < body.length && !/[\s{]/.test(body[i])) i += 1;
    const selector = body.slice(keyStart, i).trim();
    while (i < body.length && /\s/.test(body[i])) i += 1;
    if (body[i] !== "{") break;
    const close = findMatchingBrace(body, i);
    if (close < 0) break;
    branches.set(selector, body.slice(i + 1, close));
    i = close + 1;
  }
  return branches;
}

/** Format a message template with `{var}` interpolation and ICU `{count, plural, ...}`. */
export function formatMessage(
  template: string,
  vars: Record<string, string | number> | undefined,
  locale: string,
): string {
  if (!vars || !template.includes("{")) return template;
  let out = "";
  let i = 0;
  while (i < template.length) {
    const open = template.indexOf("{", i);
    if (open < 0) {
      out += template.slice(i);
      break;
    }
    out += template.slice(i, open);
    const close = findMatchingBrace(template, open);
    if (close < 0) {
      out += template.slice(open);
      break;
    }
    const expr = template.slice(open + 1, close);
    const comma = expr.indexOf(",");
    if (comma < 0) {
      const name = expr.trim();
      out += Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : `{${expr}}`;
    } else {
      const name = expr.slice(0, comma).trim();
      const rest = expr.slice(comma + 1).trim();
      const secondComma = rest.indexOf(",");
      const kind = secondComma >= 0 ? rest.slice(0, secondComma).trim() : rest;
      if (kind === "plural" && secondComma >= 0 && Object.prototype.hasOwnProperty.call(vars, name)) {
        const rawValue = Number(vars[name]);
        const branches = parsePluralBranches(rest.slice(secondComma + 1));
        const exact = branches.get(`=${rawValue}`);
        const category = Number.isFinite(rawValue) ? getPluralRules(locale).select(rawValue) : "other";
        const chosen = exact ?? branches.get(category) ?? branches.get("other") ?? "";
        const withHash = chosen.replace(/#/g, String(vars[name]));
        out += formatMessage(withHash, vars, locale);
      } else {
        out += Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : `{${expr}}`;
      }
    }
    i = close + 1;
  }
  return out;
}

// ---- BCP-47 locale negotiation ---------------------------------------------

export function negotiateLocale<L extends string>(
  candidates: readonly string[],
  locales: readonly LocaleMeta<L>[],
  fallback: L,
): L {
  const byLower = new Map<string, L>();
  for (const meta of locales) byLower.set(meta.code.toLowerCase(), meta.code);

  for (const raw of candidates) {
    const tag = raw.trim().toLowerCase().replace(/_/g, "-");
    if (!tag) continue;
    const exact = byLower.get(tag);
    if (exact) return exact;
    for (const meta of locales) {
      if (meta.matchPrefixes?.some((prefix) => tag === prefix || tag.startsWith(`${prefix}-`))) {
        return meta.code;
      }
    }
    const primary = tag.split("-")[0];
    const primaryMatch = byLower.get(primary);
    if (primaryMatch) return primaryMatch;
    const prefixMatch = locales.find((meta) => meta.code.toLowerCase().split("-")[0] === primary);
    if (prefixMatch) return prefixMatch.code;
  }
  return fallback;
}

// ---- Observable i18n instance factory --------------------------------------

export interface I18nEngineConfig<L extends string, M extends Record<string, string>> {
  readonly locales: readonly LocaleMeta<L>[];
  readonly fallback: L;
  readonly catalogs: Record<L, M>;
  readonly storageKey?: string;
}

export function createI18n<L extends string, M extends Record<string, string>>(
  config: I18nEngineConfig<L, M>,
) {
  const { locales, fallback, catalogs, storageKey = "app.locale" } = config;
  const validCodes = new Set<string>(locales.map((l) => l.code));
  const metaMap = new Map<L, LocaleMeta<L>>(locales.map((l) => [l.code, l]));

  function detectSystemLocale(): L {
    if (typeof navigator === "undefined") return fallback;
    const candidates = [...(navigator.languages ?? []), navigator.language].filter(Boolean);
    return negotiateLocale(candidates, locales, fallback);
  }

  function resolve(pref: LocalePref<L>): L {
    return pref === "auto" ? detectSystemLocale() : pref;
  }

  function readStored(): LocalePref<L> {
    try {
      const raw = typeof localStorage !== "undefined" ? localStorage.getItem(storageKey) : null;
      if (raw === "auto") return "auto";
      if (raw && validCodes.has(raw)) return raw as L;
    } catch {
      // Restricted storage (sandbox/SSR/Node).
    }
    return "auto";
  }

  let pref: LocalePref<L> = readStored();
  let locale: L = resolve(pref);
  const listeners = new Set<() => void>();

  function syncDocument() {
    if (typeof document !== "undefined" && document.documentElement) {
      document.documentElement.lang = locale;
      document.documentElement.dir = metaMap.get(locale)?.dir ?? "ltr";
    }
  }
  syncDocument();

  function notify() {
    listeners.forEach((listener) => listener());
  }

  function getLocale(): L {
    return locale;
  }

  function getPref(): LocalePref<L> {
    return pref;
  }

  function getDir(target: L = locale): TextDirection {
    return metaMap.get(target)?.dir ?? "ltr";
  }

  function setPref(nextPref: LocalePref<L>): void {
    const nextLocale = resolve(nextPref);
    if (nextPref === pref && nextLocale === locale) return;
    pref = nextPref;
    locale = nextLocale;
    try {
      if (typeof localStorage !== "undefined") localStorage.setItem(storageKey, nextPref);
    } catch {
      // Ignore storage failures.
    }
    syncDocument();
    notify();
  }

  function subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  }

  function translate<K extends keyof M & string>(
    key: K,
    ...args: ParamsFor<M[K]>
  ): string {
    return translateFor(locale, key, ...args);
  }

  function translateFor<K extends keyof M & string>(
    targetLocale: L,
    key: K,
    ...args: ParamsFor<M[K]>
  ): string {
    const template = catalogs[targetLocale]?.[key] ?? catalogs[fallback][key] ?? key;
    const vars = args[0] as Record<string, string | number> | undefined;
    return formatMessage(template, vars, targetLocale);
  }

  return {
    locales,
    fallback,
    catalogs,
    getLocale,
    getPref,
    getDir,
    setPref,
    subscribe,
    detectSystemLocale,
    t: translate,
    tFor: translateFor,
  };
}
