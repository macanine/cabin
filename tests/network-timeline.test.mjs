import assert from "node:assert/strict";
import test from "node:test";
import { networkTimeline } from "../src/lib/networkTimeline.ts";

const check = (id, label, value) => ({ id, evidence: [{ label, value }] });
const observation = (id, ...checks) => ({ id, at: id * 1_000, report: { checks } });
const address = (value) => check("transport", "Observed address", value);
const timezone = (value) => check("timezone", "Machine", value);
const change = (label, before, after) => ({ label, before, after });

test("timeline shows observed changes in newest-first order and ignores repeated checks", () => {
  assert.deepEqual(networkTimeline([
    observation(6, address("203.0.113.8"), timezone("UTC")),
    observation(5, address("203.0.113.8"), timezone("Europe/Paris")),
    observation(4, address("unknown"), timezone("unknown")),
    { id: 3, at: 3_000, report: null, message: "Probe failed" },
    observation(2, address("192.0.2.1"), timezone("UTC"), check("dns", "Resolution time", "2 ms")),
    observation(1, address("192.0.2.1"), timezone("UTC"), check("dns", "Resolution time", "1 ms")),
  ]), [
    { id: 6, at: 6_000, changes: [change("VM time zone", "Europe/Paris", "UTC")] },
    { id: 5, at: 5_000, changes: [change("Exit IP", "192.0.2.1", "203.0.113.8"), change("VM time zone", "UTC", "Europe/Paris")] },
  ]);
});

test("missing live browser observations preserve the last known value", () => {
  const live = (value) => check("browserLive", "Live language", value);
  assert.deepEqual(networkTimeline([
    observation(1, live("en-US")),
    observation(2),
    observation(3, live("en-US")),
    observation(4, live("fr-FR")),
  ]), [{ id: 4, at: 4_000, changes: [change("Live Chrome language", "en-US", "fr-FR")] }]);
});
