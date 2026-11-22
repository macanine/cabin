/**
 * Geography for the network dashboard: where the sun is, day and night,
 * sunrise and sunset, flags and place names, the map projection, and the
 * route's links. Pure functions; times are Unix ms (UTC).
 */
import type { GeoInfo, NetworkRoute, RouteCheck } from "../services/ipc/generated/core";

const RAD = Math.PI / 180;
const DAY_MS = 86_400_000;
/** 2000-01-01 12:00 UTC. */
const J2000_MS = 946_728_000_000;

function wrap180(degrees: number): number {
  return ((((degrees + 180) % 360) + 360) % 360) - 180;
}

/** The sun's declination and the equation of time (low-precision almanac,
 * good to about a minute, plenty for a map). */
function sun(ms: number): { declination: number; eqTimeMin: number } {
  const n = (ms - J2000_MS) / DAY_MS;
  const meanLongitude = (280.46 + 0.9856474 * n) % 360;
  const anomaly = ((357.528 + 0.9856003 * n) % 360) * RAD;
  const eclipticLongitude = (meanLongitude + 1.915 * Math.sin(anomaly) + 0.02 * Math.sin(2 * anomaly)) * RAD;
  const obliquity = (23.439 - 0.0000004 * n) * RAD;
  const declination = Math.asin(Math.sin(obliquity) * Math.sin(eclipticLongitude)) / RAD;
  const rightAscension = Math.atan2(Math.cos(obliquity) * Math.sin(eclipticLongitude), Math.cos(eclipticLongitude)) / RAD;
  const eqTimeMin = 4 * wrap180(meanLongitude - rightAscension);
  return { declination, eqTimeMin };
}

/** The point on Earth where the sun is overhead at `ms`. */
export function subsolarPoint(ms: number): { lat: number; lon: number } {
  const { declination, eqTimeMin } = sun(ms);
  const utcHours = ((((ms % DAY_MS) + DAY_MS) % DAY_MS) / 3_600_000);
  return { lat: declination, lon: wrap180(-15 * (utcHours - 12 + eqTimeMin / 60)) };
}

/** The sun's altitude above the horizon at a place, in degrees. */
export function solarAltitude(lat: number, lon: number, ms: number): number {
  const s = subsolarPoint(ms);
  const value =
    Math.sin(lat * RAD) * Math.sin(s.lat * RAD) + Math.cos(lat * RAD) * Math.cos(s.lat * RAD) * Math.cos((lon - s.lon) * RAD);
  return Math.asin(Math.max(-1, Math.min(1, value))) / RAD;
}

/** The latitude of the day/night line at `lon`, and which side is night. */
export function terminatorAt(lon: number, ms: number): { lat: number; nightBelow: boolean } {
  const s = subsolarPoint(ms);
  const declination = Math.abs(s.lat) < 0.01 ? 0.01 : s.lat;
  const lat = Math.atan(-Math.cos((lon - s.lon) * RAD) / Math.tan(declination * RAD)) / RAD;
  // In northern summer the far south is dark, and the reverse.
  return { lat, nightBelow: declination > 0 };
}

export type SunDay =
  | { kind: "normal"; sunrise: number; sunset: number }
  | { kind: "polarDay" }
  | { kind: "polarNight" };

/** Sunrise and sunset for the solar day nearest `ms` at a place. */
export function sunTimes(lat: number, lon: number, ms: number): SunDay {
  const { declination, eqTimeMin } = sun(ms);
  const cosH =
    (Math.sin(-0.833 * RAD) - Math.sin(lat * RAD) * Math.sin(declination * RAD)) /
    (Math.cos(lat * RAD) * Math.cos(declination * RAD));
  if (cosH < -1) return { kind: "polarDay" };
  if (cosH > 1) return { kind: "polarNight" };
  const halfDayMs = (Math.acos(cosH) / RAD / 15) * 3_600_000;
  const dayStart = Math.floor(ms / DAY_MS) * DAY_MS;
  let noon = dayStart + (12 - lon / 15 - eqTimeMin / 60) * 3_600_000;
  while (noon - ms > DAY_MS / 2) noon -= DAY_MS;
  while (ms - noon > DAY_MS / 2) noon += DAY_MS;
  return { kind: "normal", sunrise: Math.round(noon - halfDayMs), sunset: Math.round(noon + halfDayMs) };
}

/** Where `ms` falls between sunrise (0) and sunset (1); outside is night. */
export function dayProgress(day: SunDay, ms: number): number | null {
  if (day.kind !== "normal") return null;
  return (ms - day.sunrise) / (day.sunset - day.sunrise);
}

/** "kr" → "KR"; null for anything that is not a two-letter code. Flags are
 * drawn from the SVG set bundled with the app (`components/Flag.tsx`), never
 * from the system's emoji font, so they look the same everywhere. */
export function flag(countryCode: string | null | undefined): string | null {
  if (!countryCode || !/^[A-Za-z]{2}$/.test(countryCode)) return null;
  return countryCode.toUpperCase();
}

const FLAG_EMOJI = /[\u{1F1E6}-\u{1F1FF}]{2}/u;
const FLAG_EMOJI_ALL = /[\u{1F1E6}-\u{1F1FF}]{2}/gu;

/** The country of a flag emoji in a node's name ("🇯🇵 Tokyo 01" → "JP"),
 * as subscriptions often write them. */
export function flagInName(name: string): string | null {
  const pair = name.match(FLAG_EMOJI)?.[0];
  if (!pair) return null;
  return String.fromCharCode(...[...pair].map((c) => c.codePointAt(0)! - 0x1f1e6 + 65));
}

/** A name without flag emoji, which are shown as embedded flags instead. */
export function stripFlags(name: string): string {
  return name.replace(FLAG_EMOJI_ALL, " ").replace(/\s+/g, " ").trim() || name.trim();
}

/** "Incheon, South Korea": city (else region) and country, without repeats. */
export function placeLabel(geo: Pick<GeoInfo, "city" | "region" | "country" | "countryCode"> | null | undefined): string {
  if (!geo) return "Unknown location";
  const parts: string[] = [];
  for (const part of [geo.city, geo.city ? null : geo.region, geo.country ?? geo.countryCode]) {
    const value = part?.trim();
    if (value && !parts.some((seen) => seen.toLowerCase() === value.toLowerCase())) parts.push(value);
  }
  return parts.join(", ") || "Unknown location";
}

/** The usual system locale of a country, for comparing with the VM's. */
const COUNTRY_LOCALE: Record<string, string> = {
  US: "en_US", GB: "en_GB", CA: "en_CA", AU: "en_AU", NZ: "en_NZ", IE: "en_IE", IN: "en_IN", SG: "en_SG",
  JP: "ja_JP", KR: "ko_KR", CN: "zh_CN", TW: "zh_TW", HK: "zh_HK", MO: "zh_MO",
  DE: "de_DE", AT: "de_AT", CH: "de_CH", FR: "fr_FR", BE: "fr_BE", NL: "nl_NL", ES: "es_ES", MX: "es_MX",
  AR: "es_AR", IT: "it_IT", PT: "pt_PT", BR: "pt_BR", RU: "ru_RU", UA: "uk_UA", PL: "pl_PL", SE: "sv_SE",
  NO: "nb_NO", DK: "da_DK", FI: "fi_FI", TR: "tr_TR", IL: "he_IL", AE: "ar_AE", SA: "ar_SA", TH: "th_TH",
  VN: "vi_VN", ID: "id_ID", MY: "ms_MY", PH: "en_PH", CZ: "cs_CZ", HU: "hu_HU", RO: "ro_RO", GR: "el_GR",
};

export function localeForCountry(countryCode: string | null | undefined): string | null {
  return countryCode ? COUNTRY_LOCALE[countryCode.toUpperCase()] ?? null : null;
}

/** "ko_KR.UTF-8" → "ko_KR"; "ko-KR" → "ko_KR". */
export function baseLocale(locale: string | null | undefined): string | null {
  if (!locale) return null;
  return locale.split(".")[0].replace("-", "_");
}

export interface EnvironmentMatch {
  exitZone: string | null;
  exitLocale: string | null;
  zoneMatches: boolean | null;
  localeMatches: boolean | null;
}

/** The exit's timezone and locale against the VM's current settings. */
export function environmentMatch(exit: GeoInfo | null, vmZone: string | null, vmLanguage: string | null): EnvironmentMatch {
  const exitZone = exit?.timezone ?? null;
  const exitLocale = localeForCountry(exit?.countryCode);
  return {
    exitZone,
    exitLocale,
    zoneMatches: exitZone && vmZone ? exitZone === vmZone : null,
    localeMatches: exitLocale && vmLanguage ? exitLocale === baseLocale(vmLanguage) : null,
  };
}

// ---------------------------------------------------------------- map

/** The map's latitude band (the poles carry no routes). */
export const MAP_NORTH = 84;
export const MAP_SOUTH = -60;

/** Equirectangular position of a place in a `width` × `height` map. */
export function project(lat: number, lon: number, width: number, height: number): { x: number; y: number } {
  const clamped = Math.max(MAP_SOUTH, Math.min(MAP_NORTH, lat));
  return {
    x: ((wrap180(lon) + 180) / 360) * width,
    y: ((MAP_NORTH - clamped) / (MAP_NORTH - MAP_SOUTH)) * height,
  };
}

/** Control point of a link's curve: lifted off the straight line in
 * proportion to its length, always bowing upwards (north). */
export function arcControl(a: { x: number; y: number }, b: { x: number; y: number }): { x: number; y: number } {
  const mx = (a.x + b.x) / 2;
  const my = (a.y + b.y) / 2;
  const length = Math.hypot(b.x - a.x, b.y - a.y);
  return { x: mx, y: my - Math.min(length * 0.35, 80) };
}

/** A point along a quadratic curve. */
export function onCurve(a: { x: number; y: number }, c: { x: number; y: number }, b: { x: number; y: number }, t: number) {
  const u = 1 - t;
  return { x: u * u * a.x + 2 * u * t * c.x + t * t * b.x, y: u * u * a.y + 2 * u * t * c.y + t * t * b.y };
}

// -------------------------------------------------------------- route

export type LinkState = "ok" | "bad" | "unknown" | "idle";

/** The state of each link of a chain of `length` hops: link `i` leads into
 * hop `i` (link 0 starts at the Mac's virtual NIC). After a broken link
 * nothing is known. */
export function linkStates(length: number, check: RouteCheck | null, chain: string[]): LinkState[] {
  const fresh = check && check.hops.length > 0 && check.hops.every((hop, i) => hop.nodeId === chain[i]);
  return Array.from({ length }, (_, i) => {
    if (!fresh) return "idle";
    if (check.brokenAt != null) return i < check.brokenAt ? "ok" : i === check.brokenAt ? "bad" : "unknown";
    return check.hops[i]?.ok ? "ok" : "idle";
  });
}

/** `list` with the item at `from` moved to `to`. */
export function moveItem<T>(list: readonly T[], from: number, to: number): T[] {
  const next = [...list];
  if (!Number.isInteger(from) || from < 0 || from >= list.length) return next;
  const [item] = next.splice(from, 1);
  next.splice(Math.max(0, Math.min(next.length, to)), 0, item);
  return next;
}

/** `list` with `item` at `index`, without a duplicate elsewhere. */
export function insertAt<T>(list: readonly T[], index: number, item: T): T[] {
  const existing = list.indexOf(item);
  if (existing >= 0) return moveItem(list, existing, existing < index ? index - 1 : index);
  const next = [...list];
  next.splice(Math.max(0, Math.min(next.length, index)), 0, item);
  return next;
}

export function sameRoute(a: NetworkRoute, b: NetworkRoute): boolean {
  return a.enabled === b.enabled
    && ((a as Partial<import("./networkRoute").NetworkRoute>).bindInterface ?? null) === ((b as Partial<import("./networkRoute").NetworkRoute>).bindInterface ?? null)
    && a.chain.length === b.chain.length && a.chain.every((id, i) => id === b.chain[i]);
}

export function hopRole(index: number, length: number): string {
  if (length === 1) return "Only hop";
  if (index === 0) return "Entry";
  if (index === length - 1) return "Last hop";
  return "Relay";
}

// ------------------------------------------------------------ privacy

/** How much of the exit IP is shown: all blurred, the middle blurred, all shown. */
export type IpVisibility = "hidden" | "partial" | "shown";

export const NEXT_VISIBILITY: Record<IpVisibility, IpVisibility> = { hidden: "partial", partial: "shown", shown: "hidden" };

/** The address in parts (with their separators), each marked blurred or
 * not. "partial" blurs the 2nd and 3rd parts of IPv4 and the middle groups
 * of IPv6, so the network and host ends stay recognisable. */
export function maskIp(ip: string, visibility: IpVisibility): { text: string; blur: boolean }[] {
  const v6 = ip.includes(":");
  const parts = ip.split(v6 ? ":" : ".");
  const separator = v6 ? ":" : ".";
  return parts.map((part, index) => {
    const middle = v6 ? index > 0 && index < parts.length - 1 : index === 1 || index === 2;
    const blur = visibility === "hidden" || (visibility === "partial" && middle);
    return { text: index < parts.length - 1 ? part + separator : part, blur };
  });
}

// ------------------------------------------------------------ daylight

/** Local solar time at a longitude (0–24): noon is when the sun is highest. */
export function solarHour(lon: number, ms: number): number {
  const utcHours = ((((ms % DAY_MS) + DAY_MS) % DAY_MS) / 3_600_000);
  return (((utcHours + lon / 15) % 24) + 24) % 24;
}

/** How lit the land is by the sun (0 at the horizon, 1 with the sun overhead). */
export function sunlight(altitude: number): number {
  return altitude <= 0 ? 0 : Math.sin(Math.min(90, altitude) * RAD);
}

/** How dark it is (0 day … 1 full night), easing through the three
 * twilights (civil to -6°, nautical to -12°, astronomical to -18°). */
export function darkness(altitude: number): number {
  if (altitude >= 0) return 0;
  if (altitude <= -18) return 1;
  const t = -altitude / 18;
  return t * t * (3 - 2 * t);
}

export type LightPhase =
  | "day" | "golden hour" | "civil twilight" | "nautical twilight" | "astronomical twilight"
  | "evening" | "after midnight" | "before dawn";

/** The phase of the day at a place, from the sun's altitude and solar time. */
export function lightPhase(altitude: number, hour: number): LightPhase {
  if (altitude >= 6) return "day";
  if (altitude >= -0.833) return "golden hour";
  if (altitude >= -6) return "civil twilight";
  if (altitude >= -12) return "nautical twilight";
  if (altitude >= -18) return "astronomical twilight";
  if (hour >= 12) return "evening";
  return hour < 3 ? "after midnight" : "before dawn";
}

/** How many city lights are on (0–1): they come on at dusk, are brightest
 * in the evening, thin out after midnight and are fewest before dawn. */
export function cityLights(altitude: number, hour: number): number {
  if (altitude > -3) return 0;
  const dusk = Math.min(1, (-3 - altitude) / 6); // fade in through civil twilight
  // Activity by solar hour: evening peak, a midnight drop, a small pre-dawn rise.
  const activity =
    hour >= 17 ? 1
      : hour >= 0 && hour < 1 ? 0.75
        : hour >= 1 && hour < 4 ? 0.45 - (hour - 1) * 0.06
          : hour >= 4 && hour < 7 ? 0.3 + (hour - 4) * 0.08
            : 0.6;
  return dusk * activity;
}

/** A stable pseudo-random number in [0, 1) for a map cell (which "cities"
 * light first). */
export function cellNoise(index: number): number {
  let x = (index + 1) * 0x9e3779b1;
  x = Math.imul(x ^ (x >>> 16), 0x85ebca6b);
  x = Math.imul(x ^ (x >>> 13), 0xc2b2ae35);
  return ((x ^ (x >>> 16)) >>> 0) / 4294967296;
}

// ------------------------------------------------------------- hop places

export interface HopPlace {
  geo: GeoInfo | null;
  host: string | null;
  error: string | null;
}

function isLocated(geo: GeoInfo | null | undefined): geo is GeoInfo {
  return geo != null && (geo.countryCode != null || geo.country != null || geo.latitude != null);
}

export function resolveHopPlaces(
  chain: readonly string[],
  places: ReadonlyMap<string, { geo: GeoInfo | null; host: string; error: string | null }>,
): Map<string, HopPlace> {
  return new Map(chain.map((id) => {
    const place = places.get(id);
    return [id, {
      geo: isLocated(place?.geo) ? place.geo : null,
      host: place?.host || null,
      error: place?.error ?? null,
    }];
  }));
}

// -------------------------------------------------------- daylight text

const LIGHT_WORDS: [number, string][] = [[0.6, "city lights bright"], [0.35, "city lights on"], [0.05, "city lights low"]];

export interface Daylight {
  /** Whether the sun is up. */
  day: boolean;
  /** "golden hour, sun 4° up", "evening, city lights bright". */
  phase: string;
  sunrise: number | null;
  sunset: number | null;
  /** "midnight sun" / "polar night" when there is no sunrise or sunset. */
  polar: string | null;
}

/** The light at a place right now, in words. */
export function daylight(lat: number, lon: number, now: number): Daylight {
  const altitude = solarAltitude(lat, lon, now);
  const hour = solarHour(lon, now);
  const phase = lightPhase(altitude, hour);
  const lights = LIGHT_WORDS.find(([level]) => cityLights(altitude, hour) >= level)?.[1];
  const day = sunTimes(lat, lon, now);
  return {
    day: altitude > -0.833,
    phase: altitude > 0 ? `${phase}, sun ${Math.round(altitude)}° up` : lights ? `${phase}, ${lights}` : phase,
    sunrise: day.kind === "normal" ? day.sunrise : null,
    sunset: day.kind === "normal" ? day.sunset : null,
    polar: day.kind === "polarDay" ? "midnight sun" : day.kind === "polarNight" ? "polar night" : null,
  };
}
