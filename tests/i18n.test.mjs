import assert from "node:assert/strict";
import test from "node:test";
import { createI18n, formatMessage, negotiateLocale } from "../src/i18n/engine.ts";
import { CATALOGS, FALLBACK_LOCALE, SUPPORTED_LOCALES } from "../src/i18n/registry.ts";
import { getLocale, getLocaleDir, setLocalePref, t } from "../src/i18n/index.ts";

/** Extract top-level placeholder names from `{var}` and `{count, plural, ...}` (including nested vars). */
function extractPlaceholderNames(template) {
  const names = new Set();
  const findClose = (str, open) => {
    let depth = 0;
    for (let i = open; i < str.length; i += 1) {
      if (str[i] === "{") depth += 1;
      else if (str[i] === "}") {
        depth -= 1;
        if (depth === 0) return i;
      }
    }
    return -1;
  };
  const scan = (str) => {
    let i = 0;
    while (i < str.length) {
      const open = str.indexOf("{", i);
      if (open < 0) break;
      const close = findClose(str, open);
      assert.ok(close > open, `Unbalanced braces in template: ${template}`);
      const expr = str.slice(open + 1, close);
      const comma = expr.indexOf(",");
      if (comma < 0) {
        names.add(expr.trim());
      } else {
        names.add(expr.slice(0, comma).trim());
        const rest = expr.slice(comma + 1).trim();
        const secondComma = rest.indexOf(",");
        if (secondComma >= 0) {
          const body = rest.slice(secondComma + 1);
          // Scan inside each plural branch `{...}`
          let j = 0;
          while (j < body.length) {
            const bOpen = body.indexOf("{", j);
            if (bOpen < 0) break;
            const bClose = findClose(body, bOpen);
            assert.ok(bClose > bOpen, `Unbalanced plural branch in template: ${template}`);
            scan(body.slice(bOpen + 1, bClose));
            j = bClose + 1;
          }
        }
      }
      i = close + 1;
    }
  };
  scan(template);
  return [...names].sort();
}

test("all registered locale catalogs have 100% key parity, balanced braces, and matching variables", () => {
  const baseCatalog = CATALOGS[FALLBACK_LOCALE];
  const baseKeys = Object.keys(baseCatalog).sort();
  assert.ok(baseKeys.length > 200, `Expected >200 i18n keys, found ${baseKeys.length}`);

  for (const meta of SUPPORTED_LOCALES) {
    const catalog = CATALOGS[meta.code];
    assert.ok(catalog, `Missing catalog for registered locale ${meta.code}`);
    const keys = Object.keys(catalog).sort();
    assert.deepEqual(keys, baseKeys, `Key mismatch in locale ${meta.code}`);

    for (const key of baseKeys) {
      const value = catalog[key];
      assert.equal(typeof value, "string", `Non-string value for ${meta.code}.${key}`);
      assert.ok(value.trim().length > 0, `Empty translation for ${meta.code}.${key}`);
      assert.deepEqual(
        extractPlaceholderNames(value),
        extractPlaceholderNames(baseCatalog[key]),
        `Placeholder mismatch for ${meta.code}.${key}`,
      );
    }
  }
});

test("formatMessage supports interpolation and balanced-brace ICU pluralization", () => {
  const tpl = "{count, plural, =0 {No tasks} one {# task in {workspace}} other {# tasks in {workspace}}}";
  assert.equal(formatMessage(tpl, { count: 0, workspace: "cabin" }, "en"), "No tasks");
  assert.equal(formatMessage(tpl, { count: 1, workspace: "cabin" }, "en"), "1 task in cabin");
  assert.equal(formatMessage(tpl, { count: 5, workspace: "cabin" }, "en"), "5 tasks in cabin");

  const zhTpl = "{count, plural, =0 {暂无任务} other {{workspace} 中有 # 个任务}}";
  assert.equal(formatMessage(zhTpl, { count: 0, workspace: "cabin" }, "zh-CN"), "暂无任务");
  assert.equal(formatMessage(zhTpl, { count: 3, workspace: "cabin" }, "zh-CN"), "cabin 中有 3 个任务");
});

test("negotiateLocale resolves BCP-47 tags, script aliases, and fallbacks", () => {
  assert.equal(negotiateLocale(["zh-Hans-CN"], SUPPORTED_LOCALES, FALLBACK_LOCALE), "zh-CN");
  assert.equal(negotiateLocale(["zh_SG"], SUPPORTED_LOCALES, FALLBACK_LOCALE), "zh-CN");
  assert.equal(negotiateLocale(["en-GB", "zh-CN"], SUPPORTED_LOCALES, FALLBACK_LOCALE), "en");
  assert.equal(negotiateLocale(["fr-FR", "de-DE"], SUPPORTED_LOCALES, FALLBACK_LOCALE), "en");
  assert.equal(negotiateLocale(["fr-FR", "zh-CN"], SUPPORTED_LOCALES, FALLBACK_LOCALE), "zh-CN");
});

test("t() interpolates variables, ICU plurals, and reacts to locale switching", () => {
  setLocalePref("en");
  assert.equal(getLocale(), "en");
  assert.equal(getLocaleDir(), "ltr");
  assert.equal(t("home.agentsSummary", { ready: 2, total: 5 }), "2 of 5 agents");
  assert.equal(t("quit.runningTasks", { count: 1 }), " with 1 task in progress");
  assert.equal(t("quit.runningTasks", { count: 3 }), " with 3 tasks in progress");
  assert.equal(t("usage.modelsAgentsMeta", { models: 1, agents: 2 }), "1 model · 2 agents");

  setLocalePref("zh-CN");
  assert.equal(getLocale(), "zh-CN");
  assert.equal(getLocaleDir(), "ltr");
  assert.equal(t("home.agentsSummary", { ready: 2, total: 5 }), "已装 2/5 个智能体");
  assert.equal(t("quit.runningTasks", { count: 3 }), "（当前有 3 个进行中的任务）");
  assert.match(t("network.guide.rechecking.head"), /暂挂.*加速复检/);

  // Reset to English so sibling tests keep default behavior.
  setLocalePref("en");
});

test("createI18n instance is portable and isolated per storageKey", () => {
  const custom = createI18n({
    locales: [
      { code: "en", nativeName: "English", englishName: "English", dir: "ltr" },
      { code: "ar", nativeName: "العربية", englishName: "Arabic", dir: "rtl" },
    ],
    fallback: "en",
    catalogs: {
      en: { greeting: "Hello {name}" },
      ar: { greeting: "مرحباً {name}" },
    },
    storageKey: "test.custom.locale",
  });
  custom.setPref("ar");
  assert.equal(custom.getLocale(), "ar");
  assert.equal(custom.getDir(), "rtl");
  assert.equal(custom.t("greeting", { name: "Cabin" }), "مرحباً Cabin");
});
