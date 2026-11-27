# Portable i18n System (`src/i18n/`)

Zero-dependency, compile-time type-safe internationalization built around Web `Intl` APIs and modular domain catalogs.

## Architecture

| File | Responsibility |
| --- | --- |
| `engine.ts` | Pure framework-agnostic runtime (`createI18n`, `formatMessage`, `negotiateLocale`, `ExtractParams`, `ParamsFor`). Copy verbatim to any TypeScript project. |
| `registry.ts` | Single source of truth for `SUPPORTED_LOCALES`, `FALLBACK_LOCALE`, and `CATALOGS`. |
| `locales/<lang>/*.ts` | Domain-split message catalogs (`common`, `nav`, `home`, `settings`, `network`, `features`) with `as const satisfies` compile-time key parity. |
| `index.ts` | Thin React binding (`useI18n`, `useLocale`, `t`, `StaticMessageKey`). Swap this 55-line file to adapt to Vue, Svelte, Solid, or CLI. |
| `scripts/check-i18n.mjs` | Zero-dependency CI guardrail (`npm run check:i18n`) verifying key parity, balanced braces, non-empty values, and placeholder parity across all locales. |

## Adding a new language (2 steps)

1. Copy `src/i18n/locales/zh-CN/` to `src/i18n/locales/<bcp47>/` (for example `ja/` or `zh-TW/`) and translate the domain files. TypeScript immediately flags any missing or misspelled key via `satisfies`.
2. Register the locale once in `src/i18n/registry.ts` (`SUPPORTED_LOCALES` + `CATALOGS`). The language picker in Settings updates automatically, `<html lang dir>` syncs automatically, and `npm run check:i18n` validates it automatically.

## Transplanting to another project

Copy `src/i18n/engine.ts` and `scripts/check-i18n.mjs`, define your own `registry.ts` + `locales/`, and bind `createI18n(...)` to your UI framework's external store hook.
