import assert from "node:assert/strict";
import test from "node:test";
import { blockingChecks, checkFix, exitIpTypeLabel, networkGuidance, networkSummary, selectNetworkSummary, splitChecks } from "../src/lib/networkSummary.ts";

const check = (id, status, required = true) => ({
  id, title: id, category: "environment", status, required, summary: "", evidence: [],
});
const report = (checks) => ({
  id: "r", revision: "x", startedAt: 1, finishedAt: 2, checks,
  passedRequired: checks.filter((c) => c.required && c.status === "pass").length,
  totalRequired: checks.filter((c) => c.required).length, canOpen: false,
});
const status = (extra = {}) => ({
  available: true, route: { enabled: true, chain: ["a", "b"] }, running: true, error: null, gateError: null,
  gate: "closed", connectivity: "normal", failedChecks: 0, connectivityError: null, gatewaySession: 1,
  paused: false, autoRecover: true, revision: "x", generation: 1, checkedAt: null,
  expiresAt: null, nextRetryAt: null, retryAttempt: 0, report: null, ...extra,
});

test("a stopped machine is offline and a stopped gateway blocks a running machine", () => {
  assert.equal(networkSummary(status({ gate: "open" }), false).title, "Offline");
  assert.equal(networkSummary(status({ running: false }), true).title, "Blocked");
});

test("an open gate with normal connectivity reports the allowed route", () => {
  const summary = networkSummary(status({ gate: "open" }), true);
  assert.equal(summary.tone, "ok");
  assert.equal(summary.title, "Allowed");
  assert.equal(summary.detail, "Through 2 nodes");
  assert.equal(networkSummary(status({ gate: "open", route: { enabled: false, chain: [] } }), true).detail, "Direct connection");
});

test("pause and gateway errors win over the gate", () => {
  assert.equal(networkSummary(status({ paused: true, gate: "open" }), true).detail, "Internet access is paused");
  assert.equal(networkSummary(status({ error: "exited", gate: "open" }), true).detail, "exited");
});

test("rechecking shows the next connectivity attempt even when a grant exists", () => {
  const rechecking = (failedChecks) => networkSummary(status({ gate: "open", connectivity: "rechecking", failedChecks }), true);
  assert.equal(rechecking(0).title, "Rechecking");
  assert.equal(rechecking(0).detail, "Attempt 1 of 3");
  assert.equal(rechecking(1).detail, "Attempt 2 of 3");
  assert.equal(rechecking(3).detail, "Attempt 3 of 3");
  assert.equal(networkSummary(status({ running: false, error: "gateway disconnected", connectivity: "rechecking", failedChecks: 1 }), true).title, "Rechecking");
});

test("a connectivity block shows the host reason ahead of a full report", () => {
  const summary = networkSummary(status({
    gate: "checking", connectivity: "blocked", connectivityError: "The TCP check timed out",
    report: report([check("timezone", "fail")]),
  }), true);
  assert.equal(summary.title, "Blocked");
  assert.equal(summary.detail, "The TCP check timed out");
});

test("a closed gate names the most fundamental failing required check", () => {
  const summary = networkSummary(status({
    report: report([check("timezone", "fail"), check("browser", "skipped", false), check("dns", "fail"), check("route", "pass")]),
  }), true);
  assert.equal(summary.tone, "bad");
  assert.equal(summary.blocker.id, "dns");
  assert.equal(summary.detail, "Names do not resolve");
});

test("a time zone mismatch alone is explained as such", () => {
  const summary = networkSummary(status({ report: report([check("route", "pass"), check("timezone", "fail")]) }), true);
  assert.equal(summary.detail, "The VM's time zone does not match the exit");
  assert.deepEqual(checkFix(summary.blocker), { label: "Fix in Region", target: "region" });
});

test("without a report the gate error is shown", () => {
  assert.equal(networkSummary(status({ gateError: "route changed" }), true).detail, "route changed");
  assert.equal(networkSummary(status(), true).detail, "Not verified yet");
});

test("a report from a different route cannot explain a block", () => {
  const summary = networkSummary(status({ revision: "y", gateError: "Route changed", report: report([check("dns", "fail")]) }), true);
  assert.equal(summary.detail, "Route changed");
  assert.equal(summary.blocker, null);
});

test("host state drives the summary while the guest is disconnected", () => {
  const app = { machine: { state: "running" }, guestReady: false, network: status({ gate: "open" }) };
  assert.equal(selectNetworkSummary({ ...app, networkLoaded: false }).title, "Loading network");
  assert.equal(selectNetworkSummary({ ...app, networkLoaded: false, networkLoadError: "Could not read status" }).title, "Status unavailable");
  assert.equal(selectNetworkSummary({ ...app, networkLoaded: true }).title, "Allowed");
});

test("exit IP labels preserve the provider classification and missing values", () => {
  assert.equal(exitIpTypeLabel("residential"), "Residential");
  assert.equal(exitIpTypeLabel("residential", true), "Residential (estimated)");
  assert.equal(exitIpTypeLabel("mobile"), "Mobile network");
  assert.equal(exitIpTypeLabel("commercial"), "Business");
  assert.equal(exitIpTypeLabel("server"), "Data center");
  assert.equal(exitIpTypeLabel("unknown"), "Unknown type");
  assert.equal(exitIpTypeLabel(null), "Unknown type");
});

test("optional and passing checks never block", () => {
  assert.deepEqual(blockingChecks(report([check("browser", "fail", false), check("dns", "pass")])), []);
  assert.equal(checkFix(check("dns", "pass")), null);
  assert.equal(checkFix(check("dns", "unknown")), null);
});

test("a report splits into blockers, optional notes and passed checks", () => {
  const check = (id, status, required = true) => ({ id, title: id, category: "dns", status, required, summary: "", evidence: [] });
  const parts = splitChecks({ id: "r", revision: "1", startedAt: 0, finishedAt: 0, passedRequired: 1, totalRequired: 3, canOpen: false, checks: [
    check("browser", "fail"), check("dns", "pass"), check("route", "unknown"), check("language", "fail", false), check("identity", "skipped", false),
  ] });
  assert.deepEqual(parts.problems.map((c) => c.id), ["route", "browser"]);
  assert.deepEqual(parts.notes.map((c) => c.id), ["language"]);
  assert.deepEqual(parts.passed.map((c) => c.id), ["dns", "identity"]);
});

test("an open Chrome on other values blocks; both browser checks open its panel", () => {
  const parts = splitChecks(report([check("dns", "pass"), check("browser", "pass"), check("browserLive", "fail")]));
  assert.deepEqual(parts.problems.map((c) => c.id), ["browserLive"]);
  assert.deepEqual(checkFix(check("browserLive", "fail", false)), { label: "Open browser environment", target: "browser" });
  assert.deepEqual(checkFix(check("browser", "fail")), { label: "Open browser environment", target: "browser" });
  // A closed or sleeping Chrome is skipped: nothing to fix.
  assert.equal(checkFix(check("browserLive", "skipped", false)), null);
});

test("networkGuidance explains static exit IP blocks, pauses, region mismatches, and open gates", () => {
  const ipChanged = status({ exitPolicyError: "Static exit changed from 1.1.1.1 to 2.2.2.2." });
  const ipGuide = networkGuidance(ipChanged, networkSummary(ipChanged, true));
  assert.match(ipGuide.headline, /public IP changed/i);
  assert.match(ipGuide.explanation, /Static IP protection/i);
  assert.match(ipGuide.tip ?? "", /Dynamic residential IP/i);

  const paused = status({ paused: true, gate: "open" });
  assert.match(networkGuidance(paused, networkSummary(paused, true)).headline, /paused/i);

  const tz = status({ report: report([check("route", "pass"), check("timezone", "fail")]) });
  assert.match(networkGuidance(tz, networkSummary(tz, true)).headline, /region mismatch/i);

  const open = status({ gate: "open" });
  assert.match(networkGuidance(open, networkSummary(open, true)).headline, /verified/i);
});
