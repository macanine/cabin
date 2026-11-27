import assert from "node:assert/strict";
import test from "node:test";
import { en } from "../src/i18n/en.ts";
import { zhCN } from "../src/i18n/zh-CN.ts";
import { getLocale, setLocalePref, t } from "../src/i18n/index.ts";

test("en and zh-CN dictionaries have 100% key parity and matching interpolation variables", () => {
  const enKeys = Object.keys(en).sort();
  const zhKeys = Object.keys(zhCN).sort();
  assert.deepEqual(zhKeys, enKeys);

  const varsOf = (text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
  for (const key of enKeys) {
    assert.ok(zhCN[key].trim().length > 0, `Empty zh-CN translation for ${key}`);
    assert.deepEqual(varsOf(zhCN[key]), varsOf(en[key]), `Variable mismatch for ${key}`);
  }
});

test("t() interpolates variables and reacts to locale changes", () => {
  setLocalePref("en");
  assert.equal(getLocale(), "en");
  assert.equal(t("home.agentsSummary", { ready: 2, total: 5 }), "2 of 5 agents");

  setLocalePref("zh-CN");
  assert.equal(getLocale(), "zh-CN");
  assert.equal(t("home.agentsSummary", { ready: 2, total: 5 }), "已装 2/5 个智能体");
  assert.match(t("network.guide.rechecking.head"), /暂挂.*加速复检/);

  // Reset to English so sibling tests keep default behavior.
  setLocalePref("en");
});
