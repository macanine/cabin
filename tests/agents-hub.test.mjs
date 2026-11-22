import assert from "node:assert/strict";
import test from "node:test";
import { agentState, attentionCount, defaultSection, isAgentSection, nodeProblem } from "../src/lib/agentsHub.ts";

const agent = (kind, patch = {}) => ({
  kind, name: kind, package: `${kind}-cli`, installed: true, binary: "/x", cliChecked: true, version: "1.0.0",
  configPath: "", configExists: true, configError: null, modelKey: "model", model: null, authenticated: true, authDetail: "",
  ...patch,
});

test("each agent's state in a few words, with where its next step is", () => {
  assert.deepEqual(agentState(undefined), { tone: "off", label: "Not installed", needsSetup: true });
  assert.equal(agentState(agent("claude")).label, "Credentials found");
  assert.equal(agentState(agent("claude", { claudeLogin: { state: "active", account: { verified: true } } })).label, "Signed in");
  assert.equal(agentState(agent("claude", { authenticated: false, claudeLogin: { state: "unavailable" } })).label, "Sign-in check unavailable");
  assert.equal(agentState(agent("codex")).label, "Signed in");
  assert.equal(agentState(agent("opencode", { authenticated: false })).tone, "info");
  assert.deepEqual(agentState(agent("pi", { authenticated: false })), { tone: "warn", label: "Sign-in needed", needsSetup: false });
  assert.equal(agentState(agent("codex", { version: null })).label, "Broken install");
  assert.equal(agentState(agent("codex", { configError: "bad toml" })).needsSetup, true);
});

test("an agent opens where it needs attention", () => {
  assert.equal(defaultSection(agent("codex")), "account");
  assert.equal(defaultSection(agent("codex", { authenticated: false })), "account");
  assert.equal(defaultSection(agent("codex", { installed: false })), "setup");
  assert.equal(isAgentSection("setup"), true);
  assert.equal(isAgentSection("usage"), false);
  assert.equal(isAgentSection("prices"), false);
});

test("attention counts installed agents that are broken or signed out", () => {
  assert.equal(attentionCount([
    agent("claude"), agent("codex", { authenticated: false }), agent("pi", { installed: false }), agent("opencode", { authenticated: false }),
  ]), 1);
});

test("Node.js must be ready before agents can be installed", () => {
  assert.equal(nodeProblem(null), null);
  assert.match(nodeProblem({ active: null, minimumMajor: 20 }), /no Node.js/);
  assert.match(nodeProblem({ active: { version: "18.1.0", npm: "9" }, minimumMajor: 20 }), /too old/);
  assert.equal(nodeProblem({ active: { version: "22.3.0", npm: "10" }, minimumMajor: 20 }), null);
});
