import test from "node:test";
import assert from "node:assert/strict";
import { customCandidate, flatModels, groupModels, matchesModel, modelLabel } from "../src/lib/modelList.ts";

const model = (id, provider = id.split("/")[0], name = id.split("/")[1] ?? id) => ({ id, name, provider, description: null, isDefault: false });
const models = [
  model("openai/gpt-5.3-codex"), model("opencode/big-pickle"), model("openai/gpt-5-mini"),
  model("anthropic/claude-haiku"), model("plain", null, "Plain"),
];

test("search matches every word across name, id and provider", () => {
  assert.equal(matchesModel(models[0], "openai codex"), true);
  assert.equal(matchesModel(models[0], "codex haiku"), false);
  assert.equal(matchesModel(models[3], "  "), true);
});

test("groups keep the guest's provider order and respect the row limit", () => {
  const { groups, shown, total } = groupModels(models, "");
  assert.deepEqual(groups.map((g) => g.provider), ["openai", "opencode", "anthropic", null]);
  assert.equal(groups[0].models.length, 2);
  assert.equal(shown, 5);
  assert.equal(total, 5);
  const limited = groupModels(models, "", 2);
  assert.equal(limited.shown, 2);
  assert.equal(limited.total, 5);
  assert.deepEqual(flatModels(limited.groups).map((m) => m.id), ["openai/gpt-5.3-codex", "openai/gpt-5-mini"]);
});

test("labels drop the provider prefix; custom ids are offered only when new", () => {
  assert.equal(modelLabel("opencode/big-pickle", models), "big-pickle");
  assert.equal(modelLabel("opencode/claude-haiku", []), "claude-haiku");
  assert.equal(modelLabel("gpt-5", []), "gpt-5");
  assert.equal(modelLabel(null, models), null);
  assert.equal(customCandidate("openai/gpt-6", models), "openai/gpt-6");
  assert.equal(customCandidate("openai/gpt-5-mini", models), null);
  assert.equal(customCandidate("two words", models), null);
});
