import assert from "node:assert/strict";
import test from "node:test";
import { recoveryFor } from "../src/lib/recovery.ts";

test("a taken port names the port and offers both fixes", () => {
  const r = recoveryFor("127.0.0.1:12233 (the machine's SSH port) is already in use by another program");
  assert.match(r.title, /port/);
  assert.ok(r.tips.some((tip) => tip.includes("lsof -nP -iTCP:12233")));
  assert.deepEqual(r.actions, ["startNetwork", "restart"]);
  assert.match(recoveryFor("I/O error: Address already in use (os error 48)").title, /port/);
});

test("a stopped gateway, an unreachable machine and a refused key", () => {
  assert.deepEqual(recoveryFor("The machine's network gateway is not running: boom").actions, ["startNetwork", "restart"]);
  assert.match(recoveryFor("Cannot reach cabind: Connection refused").title, /cannot reach/);
  assert.match(recoveryFor("Permission denied (publickey).").title, /key/);
  assert.deepEqual(recoveryFor("something odd").actions, ["restart"]);
});
