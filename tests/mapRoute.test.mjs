import test from "node:test";
import assert from "node:assert/strict";
import { mapPointPositions, mapPopoverPlacement, mapRoute } from "../src/components/network/mapRoute.ts";
import { groupNodes, nodeKey, nodeMatches } from "../src/components/network/nodeCatalog.ts";

const origin = { key: "origin", kind: "origin", lat: 52.5, lon: 13.4 };
const entry = { key: "entry", kind: "hop", label: "1", lat: 1.35, lon: 103.8 };
const lastHop = { key: "last", kind: "hop", label: "2", lat: 37.5, lon: 126.7 };
const exit = { key: "exit", kind: "exit", lat: 37.5, lon: 126.7 };

test("subscription and custom filters share node search and distinguish refreshed configs", () => {
  const first = { id: "one", name: "Sydney", sourceId: "provider", sourceName: "Example provider", protocol: "socks5", configRevision: "old" };
  const second = { ...first, id: "two", name: "Melbourne", sourceId: "other" };
  const catalog = { nodes: [first, second], groups: [{ id: "provider", nodeIds: [second.id] }] };
  assert.deepEqual(groupNodes(catalog, "source:provider"), [first]);
  assert.deepEqual(groupNodes(catalog, "group:provider"), [second]);
  assert.ok(nodeMatches(first, "sydney"));
  assert.ok(nodeMatches(first, "example provider"));
  assert.ok(nodeMatches(first, "socks5"));
  assert.ok(!nodeMatches(second, "sydney"));
  assert.notEqual(nodeKey(first), nodeKey({ ...first, configRevision: "new" }));
});

test("the last hop and observed exit keep independent points and link states", () => {
  const single = mapRoute(origin, [lastHop], exit, ["bad"], true);
  assert.deepEqual(single.points.map((point) => point.key), ["origin", "last", "exit"]);
  assert.deepEqual(single.links.map((link) => [link.from.key, link.to.key, link.state]), [
    ["origin", "last", "bad"], ["last", "exit", "unknown"],
  ]);
});

test("missing hop locations leave geographic gaps", () => {
  const route = mapRoute(origin, [null, entry, lastHop], exit, ["bad", "unknown", "unknown"], true);
  assert.deepEqual(route.points.map((point) => point.key), ["origin", "entry", "last", "exit"]);
  assert.deepEqual(route.links.map((link) => [link.from.key, link.to.key, link.state]), [
    ["entry", "last", "unknown"], ["last", "exit", "unknown"],
  ]);
  const unknownLast = mapRoute(origin, [entry, null], exit, ["ok", "bad"], true);
  assert.equal(unknownLast.links.length, 1);
  assert.equal(unknownLast.links[0].to.key, "entry");
});

test("a draft keeps the observed exit visible and disconnected", () => {
  const draft = mapRoute(origin, [entry, lastHop], exit, ["ok", "ok"], true, true);
  assert.equal(draft.points.at(-1).key, "exit");
  assert.ok(draft.links.every((link) => link.to.key !== "exit"));
  const draftDirect = mapRoute(origin, [entry, lastHop], exit, ["ok", "ok"], false, true);
  assert.equal(draftDirect.links.length, 0);
  const direct = mapRoute(origin, [entry, lastHop], exit, ["bad", "unknown"], false);
  assert.deepEqual(direct.points.map((point) => point.key), ["origin", "exit"]);
  assert.equal(direct.links[0].state, "ok");
});

test("colocated origin, last hop and exit each have a clickable marker", () => {
  const sameOrigin = { ...origin, lat: exit.lat, lon: exit.lon };
  const positions = mapPointPositions([sameOrigin, lastHop, exit], 670, 268);
  const points = [...positions.values()];
  for (const [index, point] of points.entries()) {
    assert.ok(point.x >= 9 && point.x <= 661 && point.y >= 9 && point.y <= 259);
    assert.ok(points.slice(index + 1).every((other) => Math.hypot(point.x - other.x, point.y - other.y) >= 18));
  }
});

test("relay details avoid nearby pins on a narrow map", () => {
  const london = { key: "london", kind: "hop", label: "2", lat: 51.5, lon: -0.13 };
  const ashburn = { key: "exit", kind: "exit", lat: 39.04, lon: -77.49 };
  const points = [origin, entry, london, ashburn];
  const positions = mapPointPositions(points, 498, 199);
  const size = { width: 216, height: 122 };
  for (const key of ["entry", "london"]) {
    const at = positions.get(key);
    const placement = mapPopoverPlacement(key, points, positions, 498, 199, size);
    const left = placement.position === "left" ? at.x - 26 - size.width : at.x + 26;
    const top = at.y - size.height / 2 + placement.crossAxis;
    assert.ok(left >= 4 && left + size.width <= 494);
    assert.ok(top >= 4 && top + size.height <= 195);
    for (const point of points.filter((p) => p.key !== key)) {
      const other = positions.get(point.key);
      const radius = point.kind === "hop" ? 14 : 8;
      assert.ok(left >= other.x + radius || left + size.width <= other.x - radius || top >= other.y + radius || top + size.height <= other.y - radius, `${key} hides ${point.key}`);
    }
  }
});
