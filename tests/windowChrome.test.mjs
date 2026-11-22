import assert from "node:assert/strict";
import test from "node:test";
import { chromeLayout, createWindowChromeRefresh, MAC_DEFAULT, STRIP_DEFAULT } from "../src/lib/windowChrome.ts";

const native = { controlsRight: 68.5, controlsCenterY: 20.5 };
const measured = { strip: 41, inset: 79, center: 20.5 };

test("absent or invalid controls use the normal strip and left inset", () => {
  const bare = { strip: STRIP_DEFAULT, inset: 8, center: STRIP_DEFAULT / 2 };
  assert.deepEqual(chromeLayout(null), bare);
  assert.deepEqual(chromeLayout({ ...native, controlsRight: 0 }), bare);
  assert.deepEqual(chromeLayout({ ...native, controlsRight: Infinity }), bare);
  assert.deepEqual(chromeLayout({ ...native, controlsCenterY: NaN }), bare);
});

test("the macOS first paint reserves space and measurement follows the native centre", () => {
  assert.ok(MAC_DEFAULT.inset > measured.inset);
  assert.ok(MAC_DEFAULT.strip > measured.strip);
  assert.deepEqual(chromeLayout(native), measured);
  assert.deepEqual(chromeLayout({ ...native, controlsCenterY: 12 }), { ...measured, strip: 32, center: 12 });
  assert.deepEqual(chromeLayout({ ...native, controlsCenterY: 40 }), { ...measured, strip: 80, center: 40 });
});

test("geometry events coalesce behind an in-flight read and discard its stale result", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const first = Promise.withResolvers();
  const latest = Promise.withResolvers();
  const read = t.mock.fn(() => latest.promise);
  read.mock.mockImplementationOnce(() => first.promise);
  const apply = t.mock.fn();
  const refresh = createWindowChromeRefresh(read, apply);

  const initial = refresh.measure();
  refresh.schedule();
  refresh.schedule();
  t.mock.timers.runAll();
  assert.equal(read.mock.callCount(), 1);

  first.resolve(null);
  await initial;
  assert.equal(apply.mock.callCount(), 0);
  assert.equal(read.mock.callCount(), 2);

  latest.resolve(native);
  await latest.promise;
  assert.deepEqual(apply.mock.calls.map(({ arguments: args }) => args), [[measured]]);
});

test("an event invalidates an older response before the debounce timer fires", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const first = Promise.withResolvers();
  const read = t.mock.fn(async () => null);
  read.mock.mockImplementationOnce(() => first.promise);
  const apply = t.mock.fn();
  const refresh = createWindowChromeRefresh(read, apply);

  const initial = refresh.measure();
  refresh.schedule();
  first.resolve(native);
  await initial;
  assert.equal(apply.mock.callCount(), 0);
  assert.equal(read.mock.callCount(), 1);

  t.mock.timers.runAll();
  await Promise.resolve();
  assert.deepEqual(apply.mock.calls.map(({ arguments: args }) => args), [[chromeLayout(null)]]);
});

test("cleanup cancels pending refreshes and ignores a late result", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const first = Promise.withResolvers();
  const read = t.mock.fn(() => first.promise);
  const apply = t.mock.fn();
  const refresh = createWindowChromeRefresh(read, apply);

  const initial = refresh.measure();
  refresh.schedule();
  refresh.dispose();
  t.mock.timers.runAll();
  first.resolve(native);
  await initial;
  assert.equal(read.mock.callCount(), 1);
  assert.equal(apply.mock.callCount(), 0);
});
