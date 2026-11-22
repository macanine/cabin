import { project, type LinkState } from "../../lib/geo.ts";

export interface MapPoint {
  lat: number;
  lon: number;
  kind: "origin" | "hop" | "exit";
  key: string;
  label?: string;
  description?: string;
}

export interface MapLink {
  from: MapPoint;
  to: MapPoint;
  state: LinkState;
}

export function mapRoute(origin: MapPoint | null, hops: (MapPoint | null)[], exit: MapPoint | null, states: LinkState[], enabled: boolean, draft = false) {
  const points: MapPoint[] = origin ? [origin] : [];
  const links: MapLink[] = [];
  let previous = origin;

  if (enabled) {
    hops.forEach((hop, index) => {
      if (hop) {
        points.push(hop);
        if (previous) links.push({ from: previous, to: hop, state: states[index] ?? "idle" });
      }
      // An unknown location leaves a gap in the geographic path.
      previous = hop;
    });
  }
  if (exit) {
    points.push(exit);
    // The live exit remains visible during draft edits, disconnected from the preview.
    if (previous && !draft) links.push({ from: previous, to: exit, state: enabled ? "unknown" : "ok" });
  }
  return { points, links };
}

/** Separate colocated markers while retaining their geographic anchor. */
export function mapPointPositions(points: MapPoint[], width: number, height: number): Map<string, { x: number; y: number }> {
  const positions = new Map<string, { x: number; y: number }>();
  const occupied: { x: number; y: number }[] = [];
  for (const point of [...points.filter((p) => p.kind !== "hop"), ...points.filter((p) => p.kind === "hop")]) {
    const original = project(point.lat, point.lon, width, height);
    const clamp = (value: number, max: number) => Math.max(9, Math.min(max - 9, value));
    let position = { x: clamp(original.x, width), y: clamp(original.y, height) };
    if (occupied.some((other) => Math.hypot(other.x - position.x, other.y - position.y) < 18)) {
      for (let i = 0; i < 48; i++) {
        const radius = 20 * (1 + Math.floor(i / 8));
        const angle = (i % 8) * Math.PI / 4;
        const candidate = { x: clamp(original.x + Math.cos(angle) * radius, width), y: clamp(original.y + Math.sin(angle) * radius, height) };
        if (occupied.every((other) => Math.hypot(other.x - candidate.x, other.y - candidate.y) >= 18)) {
          position = candidate;
          break;
        }
      }
    }
    positions.set(point.key, position);
    occupied.push(position);
  }
  return positions;
}

/** Prefer a position beside the selected hop that leaves other pins visible. */
export function mapPopoverPlacement(key: string, points: MapPoint[], positions: Map<string, { x: number; y: number }>, width: number, height: number, popup: { width: number; height: number }) {
  const selected = positions.get(key);
  if (!selected) return { position: "left" as const, crossAxis: 0 };
  const minTop = 4;
  const maxTop = Math.max(minTop, height - popup.height - 4);
  const clampTop = (value: number) => Math.max(minTop, Math.min(maxTop, value));
  const centerTop = clampTop(selected.y - popup.height / 2);
  const obstacles = points.filter((point) => point.key !== key).flatMap((point) => {
    const at = positions.get(point.key);
    return at ? [{ ...at, radius: point.kind === "hop" ? 14 : 8 }] : [];
  });
  const tops = [centerTop, minTop, maxTop, ...obstacles.flatMap((p) => [p.y + p.radius, p.y - p.radius - popup.height])].map(clampTop);
  let best = { position: "left" as "left" | "right", crossAxis: 0, cost: Infinity };
  for (const position of ["left", "right"] as const) {
    const left = position === "left" ? selected.x - 26 - popup.width : selected.x + 26;
    const overflow = Math.max(0, 4 - left) + Math.max(0, left + popup.width - width + 4);
    for (const top of tops) {
      const overlaps = obstacles.filter((p) => left < p.x + p.radius && left + popup.width > p.x - p.radius && top < p.y + p.radius && top + popup.height > p.y - p.radius).length;
      const cost = overflow * 100_000 + overlaps * 10_000 + Math.abs(top - centerTop);
      if (cost < best.cost) best = { position, crossAxis: top - (selected.y - popup.height / 2), cost };
    }
  }
  return { position: best.position, crossAxis: best.crossAxis };
}
