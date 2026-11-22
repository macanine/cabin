import assert from "node:assert/strict";
import test from "node:test";
import {
  baseLocale, dayProgress, environmentMatch, flag, flagInName, hopRole, stripFlags, insertAt, linkStates, moveItem, placeLabel,
  project, sameRoute, solarAltitude, subsolarPoint, sunTimes, terminatorAt,
} from "../src/lib/geo.ts";

const at = (iso) => Date.parse(iso);

test("the sun is overhead near the tropics at the solstices and the equator at the equinox", () => {
  const june = subsolarPoint(at("2026-06-21T12:00:00Z"));
  assert.ok(Math.abs(june.lat - 23.44) < 0.2, `${june.lat}`);
  assert.ok(Math.abs(june.lon) < 3, `${june.lon}`); // noon UTC: near Greenwich
  const december = subsolarPoint(at("2026-12-21T00:00:00Z"));
  assert.ok(Math.abs(december.lat + 23.44) < 0.2);
  assert.ok(Math.abs(Math.abs(december.lon) - 180) < 3); // midnight UTC: the date line
  assert.ok(Math.abs(subsolarPoint(at("2026-03-20T15:00:00Z")).lat) < 0.5);
});

test("day and night follow the sun", () => {
  const noonLondon = at("2026-06-21T12:00:00Z");
  assert.ok(solarAltitude(51.5, -0.1, noonLondon) > 55);
  assert.ok(solarAltitude(-33.9, 151.2, noonLondon) < 0, "Sydney is dark then");
  // Northern summer: the far south is the night side.
  assert.equal(terminatorAt(0, noonLondon).nightBelow, true);
});

test("sunrise and sunset are plausible, and the poles have none", () => {
  const seoul = sunTimes(37.57, 126.98, at("2026-06-21T03:00:00Z"));
  assert.equal(seoul.kind, "normal");
  // About 05:11 and 19:57 KST (UTC+9).
  assert.ok(Math.abs(seoul.sunrise - at("2026-06-20T20:11:00Z")) < 10 * 60_000, new Date(seoul.sunrise).toISOString());
  assert.ok(Math.abs(seoul.sunset - at("2026-06-21T10:57:00Z")) < 10 * 60_000, new Date(seoul.sunset).toISOString());
  assert.ok(dayProgress(seoul, at("2026-06-21T03:00:00Z")) > 0.3);
  assert.equal(sunTimes(78, 15, at("2026-06-21T12:00:00Z")).kind, "polarDay");
  assert.equal(sunTimes(78, 15, at("2026-12-21T12:00:00Z")).kind, "polarNight");
});

test("flags and place names", () => {
  assert.equal(flag("kr"), "KR");
  assert.equal(flag("KOR"), null);
  assert.equal(flagInName("🇯🇵 Tokyo 01"), "JP");
  assert.equal(flagInName("Tokyo 🇺🇸 01"), "US");
  assert.equal(flagInName("Tokyo 01"), null);
  assert.equal(stripFlags("🇯🇵 Tokyo  🇯🇵 01"), "Tokyo 01");
  assert.equal(stripFlags("Plain"), "Plain");
  assert.equal(placeLabel({ city: "Incheon", region: "Incheon", country: "South Korea", countryCode: "KR" }), "Incheon, South Korea");
  assert.equal(placeLabel({ city: "Singapore", region: null, country: "Singapore", countryCode: "SG" }), "Singapore");
  assert.equal(placeLabel({ city: null, region: "Bavaria", country: "Germany", countryCode: "DE" }), "Bavaria, Germany");
  assert.equal(placeLabel(null), "Unknown location");
});

test("the exit's zone and locale against the VM's", () => {
  const exit = { timezone: "Asia/Seoul", countryCode: "KR" };
  assert.deepEqual(environmentMatch(exit, "Asia/Seoul", "ko_KR.UTF-8"), {
    exitZone: "Asia/Seoul", exitLocale: "ko_KR", zoneMatches: true, localeMatches: true,
  });
  const off = environmentMatch(exit, "Europe/Berlin", "en_US.UTF-8");
  assert.equal(off.zoneMatches, false);
  assert.equal(off.localeMatches, false);
  assert.equal(environmentMatch(null, "UTC", null).zoneMatches, null);
  assert.equal(baseLocale("ko-KR"), "ko_KR");
});

test("projection keeps the map band", () => {
  assert.deepEqual(project(84, -180, 360, 144), { x: 0, y: 0 });
  assert.deepEqual(project(-90, 180, 360, 144), { x: 0, y: 144 }); // 180 wraps to -180; the south is clamped
  assert.equal(project(0, 0, 360, 144).x, 180);
});

test("links: ok up to the break, red at it, unknown after", () => {
  const chain = ["a", "b", "c"];
  const check = { hops: [{ nodeId: "a", ok: true }, { nodeId: "b", ok: false }], brokenAt: 1, checkedAt: 0 };
  assert.deepEqual(linkStates(3, check, chain), ["ok", "bad", "unknown"]);
  assert.deepEqual(linkStates(3, check, ["a", "c", "b"]), ["idle", "idle", "idle"], "a check of another order says nothing");
  const good = { hops: chain.map((nodeId) => ({ nodeId, ok: true })), brokenAt: null, checkedAt: 0 };
  assert.deepEqual(linkStates(3, good, chain), ["ok", "ok", "ok"]);
  assert.deepEqual(linkStates(2, null, chain), ["idle", "idle"]);
});

test("editing a chain", () => {
  assert.deepEqual(moveItem(["a", "b", "c"], 0, 2), ["b", "c", "a"]);
  assert.deepEqual(insertAt(["a", "c"], 1, "b"), ["a", "b", "c"]);
  assert.deepEqual(insertAt(["a", "b", "c"], 3, "a"), ["b", "c", "a"], "inserting an existing node moves it");
  assert.equal(sameRoute({ enabled: true, chain: ["a"] }, { enabled: true, chain: ["a"] }), true);
  assert.equal(sameRoute({ enabled: true, chain: ["a"] }, { enabled: false, chain: ["a"] }), false);
  assert.equal(sameRoute({ enabled: true, chain: ["a"] }, { enabled: true, chain: ["a"], bindInterface: null }), true);
  assert.equal(sameRoute({ enabled: true, chain: ["a"], bindInterface: "en0" }, { enabled: true, chain: ["a"], bindInterface: "en1" }), false);
  assert.equal(hopRole(0, 1), "Only hop");
  assert.equal(hopRole(1, 3), "Relay");
  assert.equal(hopRole(2, 3), "Last hop");
  assert.deepEqual(moveItem(["a", "b"], -1, 0), ["a", "b"], "a stale drag cannot alter the chain");
});

test("interface choices include generic hardware and retain unavailable selections", async () => {
  const { interfaceOptions, normalizeRoute } = await import("../src/lib/networkRoute.ts");
  const interfaces = [
    { name: "en0", label: "Wi-Fi", kind: "wifi", available: true, ipv4: ["192.0.2.2"] },
    { name: "en1", label: "Ethernet", kind: "ethernet", available: false, ipv4: [] },
    { name: "en8", label: "USB adapter (en8)", kind: "hardware", available: true, ipv4: ["192.0.2.3"] },
    { name: "utun0", label: "VPN", kind: "vpn", available: true, ipv4: [] },
  ];
  assert.deepEqual(interfaceOptions(interfaces, null), [
    { value: "en0", label: "Wi-Fi (en0)", disabled: false },
    { value: "en1", label: "Ethernet (en1) · Unavailable", disabled: true },
    { value: "en8", label: "USB adapter (en8)", disabled: false },
  ]);
  assert.deepEqual(interfaceOptions(interfaces, "en9").at(-1), { value: "en9", label: "en9 · Unavailable", disabled: true });
  assert.equal(normalizeRoute({ enabled: false, chain: [] }).bindInterface, null);
  assert.equal(normalizeRoute({ enabled: true, chain: [], bindInterface: "en0" }).bindInterface, "en0");
});

test("route and node test results are scoped to the selected interface", async () => {
  const { routeCheckKey, nodeTestKey } = await import("../src/lib/networkRoute.ts");
  const revisions = new Map([["a", "r1"]]);
  const route = { enabled: true, chain: ["a"], bindInterface: null };
  assert.notEqual(routeCheckKey(route, revisions), routeCheckKey({ ...route, bindInterface: "en0" }, revisions));
  assert.notEqual(routeCheckKey(route, revisions), routeCheckKey(route, new Map([["a", "r2"]])));
  assert.notEqual(routeCheckKey(route, revisions), routeCheckKey({ ...route, enabled: false }, revisions));
  const node = { id: "a", configRevision: "r1" };
  assert.notEqual(nodeTestKey(node, "latency", null), nodeTestKey(node, "latency", "en0"));
  assert.notEqual(nodeTestKey(node, "download", "en0"), nodeTestKey(node, "latency", "en0"));
  assert.notEqual(nodeTestKey(node, "latency", "en0"), nodeTestKey({ ...node, configRevision: "r2" }, "latency", "en0"));
});

test("the land mask has the continents where they belong", async () => {
  const { landDots } = await import("../src/lib/worldDots.ts");
  const dots = landDots();
  assert.equal(dots.length, 2511);
  const near = (lat, lon) => dots.some((d) => Math.abs(d.lat - lat) < 2.5 && Math.abs(d.lon - lon) < 2.5);
  assert.ok(near(48.9, 2.3), "Paris");
  assert.ok(near(35.7, 139.7), "Tokyo");
  assert.ok(near(-23.5, -46.6), "São Paulo");
  assert.ok(!near(30, -40), "mid-Atlantic");
});

test("the exit IP blurs in three levels", async () => {
  const { maskIp, NEXT_VISIBILITY } = await import("../src/lib/geo.ts");
  const blurred = (ip, v) => maskIp(ip, v).map((p) => (p.blur ? "*" : p.text)).join("|");
  assert.equal(blurred("207.152.72.158", "hidden"), "*|*|*|*");
  assert.equal(blurred("207.152.72.158", "partial"), "207.|*|*|158");
  assert.equal(blurred("207.152.72.158", "shown"), "207.|152.|72.|158");
  assert.equal(maskIp("207.152.72.158", "shown").map((p) => p.text).join(""), "207.152.72.158");
  assert.equal(blurred("2001:db8:0:1::5", "partial"), "2001:|*|*|*|*|5"); // "::" is an empty group
  assert.equal(NEXT_VISIBILITY[NEXT_VISIBILITY[NEXT_VISIBILITY.hidden]], "hidden", "the cycle returns to hidden");
});

test("daylight: twilight bands, phases and city lights", async () => {
  const { darkness, sunlight, lightPhase, cityLights, solarHour, cellNoise } = await import("../src/lib/geo.ts");
  assert.equal(darkness(10), 0);
  assert.equal(darkness(-20), 1);
  assert.ok(darkness(-3) < darkness(-9) && darkness(-9) < darkness(-15), "darker through each twilight");
  assert.ok(sunlight(60) > sunlight(10) && sunlight(-1) === 0);
  assert.equal(lightPhase(30, 13), "day");
  assert.equal(lightPhase(3, 18), "golden hour");
  assert.equal(lightPhase(-8, 19), "nautical twilight");
  assert.equal(lightPhase(-40, 21), "evening");
  assert.equal(lightPhase(-40, 1), "after midnight");
  assert.equal(lightPhase(-30, 4.5), "before dawn");
  assert.equal(cityLights(5, 18), 0, "no lights in daylight");
  assert.ok(cityLights(-30, 21) > cityLights(-30, 2) && cityLights(-30, 2) > cityLights(-30, 4), "evening > midnight > pre-dawn");
  assert.ok(Math.abs(solarHour(0, Date.parse("2026-01-01T12:00:00Z")) - 12) < 1e-9);
  assert.ok(Math.abs(solarHour(135, Date.parse("2026-01-01T12:00:00Z")) - 21) < 1e-9);
  const noise = Array.from({ length: 1000 }, (_, i) => cellNoise(i));
  assert.ok(noise.every((n) => n >= 0 && n < 1) && noise.filter((n) => n < 0.5).length > 400, "spread evenly");
});

test("hop places use a node's own lookup and preserve unknown last hops", async () => {
  const { resolveHopPlaces } = await import("../src/lib/geo.ts");
  const bare = { ip: "207.152.72.158", countryCode: null, country: null, city: null, latitude: null, longitude: null };
  const tokyo = { ip: "203.0.113.10", countryCode: "JP", country: "Japan", city: "Tokyo", latitude: 35.7, longitude: 139.7 };
  const chain = resolveHopPlaces(
    ["a", "b", "c"],
    new Map([["a", { geo: tokyo, host: "203.0.113.10", error: null }], ["b", { geo: null, host: "relay.example", error: "Could not resolve" }], ["c", { geo: bare, host: "last.example", error: null }]]),
  );
  assert.equal(chain.get("a").geo.city, "Tokyo");
  assert.equal(chain.get("b").geo, null);
  assert.equal(chain.get("b").error, "Could not resolve");
  assert.deepEqual(chain.get("c"), { geo: null, host: "last.example", error: null });
  assert.equal(resolveHopPlaces(["x"], new Map()).get("x").geo, null);
});

test("daylight in words", async () => {
  const { daylight } = await import("../src/lib/geo.ts");
  const noon = daylight(51.5, 0, Date.parse("2026-06-21T12:00:00Z"));
  assert.equal(noon.day, true);
  assert.match(noon.phase, /^day, sun \d+° up$/);
  assert.ok(noon.sunrise < Date.parse("2026-06-21T12:00:00Z") && noon.sunset > Date.parse("2026-06-21T12:00:00Z"));
  const night = daylight(51.5, 0, Date.parse("2026-11-16T21:00:00Z"));
  assert.equal(night.day, false);
  assert.match(night.phase, /^evening, city lights/);
  assert.equal(daylight(78, 15, Date.parse("2026-12-21T12:00:00Z")).polar, "polar night");
});
