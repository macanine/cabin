#!/usr/bin/env node
/**
 * Portable zero-dependency CI validator for src/i18n/.
 *
 * Checks every locale registered in src/i18n/registry.ts against the fallback locale:
 * 1. Exact key parity (no missing or orphaned keys)
 * 2. No empty or whitespace-only values
 * 3. Balanced `{ ... }` braces in every template (including nested ICU plural branches)
 * 4. Identical interpolation & ICU plural variable names across all locales
 */
import { CATALOGS, FALLBACK_LOCALE, SUPPORTED_LOCALES } from "../src/i18n/registry.ts";

function findMatchingBrace(str, open) {
  let depth = 0;
  for (let i = open; i < str.length; i += 1) {
    if (str[i] === "{") depth += 1;
    else if (str[i] === "}") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

function inspectTemplate(template) {
  const vars = new Set();
  const issues = [];

  const scan = (str) => {
    let i = 0;
    while (i < str.length) {
      const open = str.indexOf("{", i);
      const strayClose = str.indexOf("}", i);
      if (strayClose >= 0 && (open < 0 || strayClose < open)) {
        issues.push(`unexpected closing brace '}' at index ${strayClose}`);
        break;
      }
      if (open < 0) break;
      const close = findMatchingBrace(str, open);
      if (close < 0) {
        issues.push(`unclosed brace '{' at index ${open}`);
        break;
      }
      const expr = str.slice(open + 1, close);
      const comma = expr.indexOf(",");
      if (comma < 0) {
        const name = expr.trim();
        if (!name) issues.push("empty placeholder '{}'");
        else vars.add(name);
      } else {
        const name = expr.slice(0, comma).trim();
        if (!name) issues.push("empty variable name in ICU expression");
        else vars.add(name);
        const rest = expr.slice(comma + 1).trim();
        const secondComma = rest.indexOf(",");
        if (secondComma >= 0) {
          const body = rest.slice(secondComma + 1);
          let j = 0;
          while (j < body.length) {
            const bOpen = body.indexOf("{", j);
            if (bOpen < 0) break;
            const bClose = findMatchingBrace(body, bOpen);
            if (bClose < 0) {
              issues.push("unclosed brace inside ICU plural branch");
              break;
            }
            scan(body.slice(bOpen + 1, bClose));
            j = bClose + 1;
          }
        }
      }
      i = close + 1;
    }
  };

  scan(template);
  return { vars: [...vars].sort(), issues };
}

const errors = [];
const baseCatalog = CATALOGS[FALLBACK_LOCALE];
if (!baseCatalog) {
  console.error(`FAIL: Fallback locale "${FALLBACK_LOCALE}" is missing from CATALOGS.`);
  process.exit(1);
}

const baseKeys = Object.keys(baseCatalog).sort();
const baseMeta = new Map();

for (const key of baseKeys) {
  const val = baseCatalog[key];
  if (typeof val !== "string" || val.trim().length === 0) {
    errors.push(`[${FALLBACK_LOCALE}] ${key}: empty or non-string value`);
    continue;
  }
  const inspected = inspectTemplate(val);
  for (const issue of inspected.issues) {
    errors.push(`[${FALLBACK_LOCALE}] ${key}: ${issue}`);
  }
  baseMeta.set(key, inspected.vars);
}

for (const meta of SUPPORTED_LOCALES) {
  const code = meta.code;
  const catalog = CATALOGS[code];
  if (!catalog) {
    errors.push(`[${code}] registered in SUPPORTED_LOCALES but missing from CATALOGS`);
    continue;
  }
  if (code === FALLBACK_LOCALE) continue;

  const keys = new Set(Object.keys(catalog));
  for (const key of baseKeys) {
    if (!keys.has(key)) {
      errors.push(`[${code}] missing key "${key}"`);
      continue;
    }
    const val = catalog[key];
    if (typeof val !== "string" || val.trim().length === 0) {
      errors.push(`[${code}] ${key}: empty translation`);
      continue;
    }
    const inspected = inspectTemplate(val);
    for (const issue of inspected.issues) {
      errors.push(`[${code}] ${key}: ${issue}`);
    }
    const expectedVars = baseMeta.get(key) ?? [];
    if (JSON.stringify(inspected.vars) !== JSON.stringify(expectedVars)) {
      errors.push(
        `[${code}] ${key}: variable mismatch (expected [${expectedVars.join(", ")}], got [${inspected.vars.join(", ")}])`,
      );
    }
  }
  for (const key of keys) {
    if (!Object.prototype.hasOwnProperty.call(baseCatalog, key)) {
      errors.push(`[${code}] extra key "${key}" not present in "${FALLBACK_LOCALE}"`);
    }
  }
}

if (errors.length > 0) {
  console.error(`i18n check failed with ${errors.length} issue(s):`);
  for (const err of errors) console.error(`  - ${err}`);
  process.exit(1);
}

console.log(
  `i18n check passed: ${SUPPORTED_LOCALES.length} locales (${SUPPORTED_LOCALES.map((l) => l.code).join(", ")}), ${baseKeys.length} keys verified.`,
);
