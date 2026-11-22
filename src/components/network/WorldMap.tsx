import { Popover } from "@mantine/core";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  arcControl, cellNoise, cityLights, darkness, MAP_NORTH, MAP_SOUTH, onCurve, project,
  solarAltitude, solarHour, subsolarPoint, sunlight,
} from "../../lib/geo";
import { landDots } from "../../lib/worldDots";
import { mapPointPositions, mapPopoverPlacement, type MapLink, type MapPoint } from "./mapRoute";

const MAP_RATIO = 360 / 144; // the band is 360° × 144°
const reducedMotion = () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

function tokens(el: HTMLElement) {
  const style = getComputedStyle(el);
  const read = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  return {
    dot: read("--map-dot", "#b8b2a8"),
    night: read("--map-dot-night", "#8d877e"),
    shade: read("--map-shade", "rgba(30, 28, 40, 0.07)"),
    arc: read("--map-arc", "#c2622d"),
    idle: read("--map-arc-idle", "rgba(194, 98, 45, 0.45)"),
    unknown: read("--map-arc-unknown", "rgba(120, 120, 120, 0.45)"),
    bad: read("--bad", "#c0392b"),
    node: read("--map-node", "#1d1c1a"),
    halo: read("--map-halo", "#ffffff"),
    glow: read("--map-glow", "#e8a33d"),
    sun: read("--map-sun", "rgba(255, 196, 92, 0.22)"),
    lightHalo: Number.parseFloat(read("--map-light-halo", "0.28")) || 0,
  };
}

let shadeGrid: HTMLCanvasElement | null = null;
const GRID_W = 180; // 2° cells, smoothed when scaled up
const GRID_H = 72;

/** The slow layer, drawn once per size, theme and minute: twilight shade,
 * the sun, land lit by its altitude, and city lights on the night side. */
function drawBase(ctx: CanvasRenderingContext2D, w: number, h: number, now: number, t: ReturnType<typeof tokens>) {
  ctx.clearRect(0, 0, w, h);

  // Night and the three twilights as a smooth gradient: darkness per 2°
  // cell, scaled up with smoothing.
  shadeGrid ??= document.createElement("canvas");
  shadeGrid.width = GRID_W;
  shadeGrid.height = GRID_H;
  const grid = shadeGrid.getContext("2d");
  if (grid) {
    grid.clearRect(0, 0, GRID_W, GRID_H);
    grid.fillStyle = t.shade;
    for (let j = 0; j < GRID_H; j++) {
      const lat = MAP_NORTH - ((j + 0.5) * (MAP_NORTH - MAP_SOUTH)) / GRID_H;
      for (let i = 0; i < GRID_W; i++) {
        const lon = -180 + ((i + 0.5) * 360) / GRID_W;
        const dark = darkness(solarAltitude(lat, lon, now));
        if (dark <= 0) continue;
        grid.globalAlpha = dark;
        grid.fillRect(i, j, 1, 1);
      }
    }
    ctx.save();
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(shadeGrid, 0, 0, w, h);
    ctx.restore();
  }

  // The sun: a soft glow where it stands overhead.
  const sun = subsolarPoint(now);
  const s = project(sun.lat, sun.lon, w, h);
  const radius = w * 0.16;
  const glow = ctx.createRadialGradient(s.x, s.y, 0, s.x, s.y, radius);
  glow.addColorStop(0, t.sun);
  glow.addColorStop(1, "transparent");
  ctx.fillStyle = glow;
  ctx.fillRect(s.x - radius, s.y - radius, radius * 2, radius * 2);

  // Land: brighter the higher the sun; city lights after dusk.
  const dot = Math.max(0.9, (w / 144) * 0.3);
  landDots().forEach((cell, index) => {
    const { x, y } = project(cell.lat, cell.lon, w, h);
    const altitude = solarAltitude(cell.lat, cell.lon, now);
    const lit = sunlight(altitude);
    const dark = darkness(altitude);
    ctx.fillStyle = altitude > 0 ? t.dot : t.night;
    ctx.globalAlpha = altitude > 0 ? 0.5 + 0.5 * lit : 0.75 - 0.35 * dark;
    ctx.beginPath();
    ctx.arc(x, y, dot, 0, Math.PI * 2);
    ctx.fill();

    const lights = cityLights(altitude, solarHour(cell.lon, now));
    // Sparse, like a night photo: at most about a fifth of the land lit,
    // the same "cities" first; the brightest keep theirs longest.
    if (lights > 0 && cellNoise(index) < lights * 0.2) {
      const level = Math.min(1, lights * (1.2 - cellNoise(index * 7 + 3) * 0.6));
      const halo = ctx.createRadialGradient(x, y, 0, x, y, dot * 2.6);
      halo.addColorStop(0, t.glow);
      halo.addColorStop(1, "transparent");
      ctx.globalAlpha = t.lightHalo * level;
      ctx.fillStyle = halo;
      ctx.fillRect(x - dot * 2.6, y - dot * 2.6, dot * 5.2, dot * 5.2);
      ctx.globalAlpha = 0.9 * level;
      ctx.fillStyle = t.glow;
      ctx.beginPath();
      ctx.arc(x, y, dot * 0.75, 0, Math.PI * 2);
      ctx.fill();
    }
  });
  ctx.globalAlpha = 1;
}

/** Route, particles and nodes over the base. */
function drawRoute(ctx: CanvasRenderingContext2D, w: number, h: number, links: MapLink[], points: MapPoint[], positions: Map<string, { x: number; y: number }>, selected: string | null, time: number, t: ReturnType<typeof tokens>, animate: boolean, draft: boolean) {
  for (const link of links) {
    const colocated = link.from.lat === link.to.lat && link.from.lon === link.to.lon;
    const a = (colocated && positions.get(link.from.key)) || project(link.from.lat, link.from.lon, w, h);
    const b = (colocated && positions.get(link.to.key)) || project(link.to.lat, link.to.lon, w, h);
    const c = arcControl(a, b);
    ctx.save();
    ctx.lineWidth = link.state === "ok" ? 1.6 : 1.3;
    ctx.strokeStyle = link.state === "ok" ? t.arc : link.state === "bad" ? t.bad : link.state === "unknown" ? t.unknown : t.idle;
    if (draft || link.state === "bad" || link.state === "unknown") ctx.setLineDash([4, 4]);
    if (!draft && link.state === "ok") { ctx.shadowColor = t.arc; ctx.shadowBlur = 6; }
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.quadraticCurveTo(c.x, c.y, b.x, b.y);
    ctx.stroke();
    ctx.restore();
    if (link.state === "bad") {
      // Mark the break in the middle of the link.
      const m = onCurve(a, c, b, 0.5);
      ctx.save();
      ctx.strokeStyle = t.bad;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(m.x - 4, m.y - 4); ctx.lineTo(m.x + 4, m.y + 4);
      ctx.moveTo(m.x + 4, m.y - 4); ctx.lineTo(m.x - 4, m.y + 4);
      ctx.stroke();
      ctx.restore();
    } else if (!draft && link.state === "ok") {
      const count = animate ? 3 : 0;
      ctx.fillStyle = t.arc;
      for (let k = 0; k < count; k++) {
        const p = onCurve(a, c, b, (time / 1800 + k / count) % 1);
        ctx.beginPath();
        ctx.arc(p.x, p.y, 1.8, 0, Math.PI * 2);
        ctx.fill();
      }
    }
  }
  for (const point of points) {
    const actual = project(point.lat, point.lon, w, h);
    const { x, y } = positions.get(point.key) ?? actual;
    ctx.save();
    if (Math.hypot(x - actual.x, y - actual.y) > 1) {
      ctx.strokeStyle = t.node;
      ctx.globalAlpha = 0.5;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(actual.x, actual.y);
      ctx.lineTo(x, y);
      ctx.stroke();
      ctx.globalAlpha = 1;
    }
    if (point.kind === "exit") {
      // At night the exit's city lights up: a warm glow under the pin.
      if (solarAltitude(point.lat, point.lon, Date.now()) < -0.833) {
        const glow = ctx.createRadialGradient(x, y, 0, x, y, 16);
        glow.addColorStop(0, t.glow);
        glow.addColorStop(1, "transparent");
        ctx.globalAlpha = 0.55;
        ctx.fillStyle = glow;
        ctx.beginPath();
        ctx.arc(x, y, 16, 0, Math.PI * 2);
        ctx.fill();
        ctx.globalAlpha = 1;
      }
      const pulse = animate ? (time / 1600) % 1 : 0.4;
      ctx.strokeStyle = t.arc;
      ctx.globalAlpha = 1 - pulse;
      ctx.lineWidth = 1.2;
      ctx.beginPath();
      ctx.arc(x, y, 4 + pulse * 9, 0, Math.PI * 2);
      ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.fillStyle = t.arc;
    }
    if (point.key === selected) {
      // The point shown in the panel: a soft accent ring.
      ctx.strokeStyle = t.arc;
      ctx.lineWidth = 2;
      ctx.globalAlpha = 0.9;
      ctx.beginPath();
      ctx.arc(x, y, point.label ? 11 : 9, 0, Math.PI * 2);
      ctx.stroke();
      ctx.globalAlpha = 1;
    }
    if (point.kind === "hop" && point.label) {
      // Each configured hop has its own numbered badge.
      ctx.fillStyle = t.node;
      ctx.strokeStyle = t.halo;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(x, y, 7, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = t.halo;
      ctx.font = "600 9px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(point.label, x, y + 0.5);
    } else {
      ctx.fillStyle = point.kind === "exit" ? t.arc : t.halo;
      ctx.strokeStyle = point.kind === "origin" ? t.node : t.halo;
      ctx.lineWidth = point.kind === "origin" ? 2 : 1.5;
      ctx.beginPath();
      ctx.arc(x, y, point.kind === "origin" ? 4 : 3.6, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
    }
    ctx.restore();
  }
}

/** A dotted world map with the live day/night shadow and route. */
export function WorldMap({ links, points, selected, onSelect, onDismiss, hopDetail, draft }: {
  links: MapLink[];
  points: MapPoint[];
  draft: boolean;
  selected: string | null;
  onSelect?: (key: string) => void;
  onDismiss?: () => void;
  hopDetail?: ReactNode;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const base = useRef<HTMLCanvasElement | null>(null);
  const baseKey = useRef("");
  const [size, setSize] = useState({ w: 0, h: 0, dpr: 1 });
  const [minute, setMinute] = useState(() => Math.floor(Date.now() / 60_000));
  const [theme, setTheme] = useState(0);
  const [popup, setPopup] = useState<HTMLDivElement | null>(null);
  const [popupSize, setPopupSize] = useState({ width: 216, height: 125 });
  const positions = useMemo(() => mapPointPositions(points, size.w, size.h), [points, size.w, size.h]);

  useEffect(() => {
    if (!popup) return;
    const observer = new ResizeObserver(() => {
      const { width, height } = popup.getBoundingClientRect();
      setPopupSize((old) => Math.abs(old.width - width) < 0.5 && Math.abs(old.height - height) < 0.5 ? old : { width, height });
    });
    observer.observe(popup);
    return () => observer.disconnect();
  }, [popup]);

  // Size to the slot; the band keeps its 2.5 : 1 shape.
  useEffect(() => {
    const el = canvas.current?.parentElement;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      const w = Math.floor(entries[0].contentRect.width);
      setSize({ w, h: Math.round(w / MAP_RATIO), dpr: window.devicePixelRatio || 1 });
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // The terminator moves; theme changes recolour.
  useEffect(() => {
    const timer = window.setInterval(() => setMinute(Math.floor(Date.now() / 60_000)), 30_000);
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    const bump = () => setTheme((n) => n + 1);
    media?.addEventListener("change", bump);
    const observer = new MutationObserver(bump);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "data-mantine-color-scheme"] });
    return () => { window.clearInterval(timer); media?.removeEventListener("change", bump); observer.disconnect(); };
  }, []);

  useEffect(() => {
    const el = canvas.current;
    if (!el || !size.w) return;
    const { w, h, dpr } = size;
    el.width = Math.round(w * dpr);
    el.height = Math.round(h * dpr);
    const t = tokens(el);
    // The base layer is slow: redraw it only for a new size, minute or
    // theme, not for each route, selection or measurement update.
    const key = `${el.width}x${el.height}:${dpr}:${minute}:${theme}`;
    const offscreen = base.current ?? document.createElement("canvas");
    base.current = offscreen;
    if (baseKey.current !== key) {
      offscreen.width = el.width;
      offscreen.height = el.height;
      const bctx = offscreen.getContext("2d");
      if (!bctx) return;
      bctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      drawBase(bctx, w, h, minute * 60_000, t);
      baseKey.current = key;
    }

    const ctx = el.getContext("2d");
    if (!ctx) return;
    const animate = !reducedMotion() && (points.some((point) => point.kind === "exit") || (!draft && links.some((link) => link.state === "ok")));
    let frame = 0;
    const paint = (time: number) => {
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, el.width, el.height);
      ctx.drawImage(offscreen, 0, 0);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      drawRoute(ctx, w, h, links, points, positions, selected, time, t, animate, draft);
      if (animate) frame = requestAnimationFrame(paint);
    };
    paint(performance.now());
    return () => cancelAnimationFrame(frame);
  }, [size, minute, theme, links, points, positions, selected, draft]);

  return (
    <div className="net-map-canvas">
      <canvas
        ref={canvas}
        style={{ width: size.w, height: size.h }}
        role="img"
        aria-label={draft ? "World map showing the draft path and the observed exit" : "World map showing the active route and the observed exit"}
      />
      {size.w > 0 && points.map((point) => {
        const position = positions.get(point.key)!;
        const marker = (
          <button
            type="button"
            className="net-map-marker"
            style={{ left: position.x, top: position.y }}
            aria-label={point.description}
            aria-pressed={selected === point.key}
            title={point.description}
            onClick={() => point.kind === "hop" && selected === point.key ? onDismiss?.() : onSelect?.(point.key)}
          />
        );
        if (point.kind !== "hop") return <div key={point.key}>{marker}</div>;
        const placement = selected === point.key
          ? mapPopoverPlacement(point.key, points, positions, size.w, size.h, popupSize)
          : { position: "left" as const, crossAxis: 0 };
        const boundary = canvas.current ?? undefined;
        return (
          <Popover key={point.key} opened={selected === point.key} onChange={(opened) => opened ? onSelect?.(point.key) : onDismiss?.()} position={placement.position} offset={{ mainAxis: 8, crossAxis: placement.crossAxis }} middlewares={{ flip: { boundary, padding: 4 }, shift: { boundary, padding: 4 } }} preventPositionChangeWhenVisible={false} withinPortal withArrow returnFocus shadow="md" width={216}>
            <Popover.Target>{marker}</Popover.Target>
            <Popover.Dropdown className="net-map-popover" ref={selected === point.key ? setPopup : undefined}>
              {selected === point.key ? hopDetail : null}
            </Popover.Dropdown>
          </Popover>
        );
      })}
    </div>
  );
}
