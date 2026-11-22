import type { NetworkTraffic } from "../services/ipc/generated/core";

export const TRAFFIC_WINDOW = 30;

export function appendTraffic(history: readonly NetworkTraffic[], sample: NetworkTraffic): readonly NetworkTraffic[] {
  const previous = history[history.length - 1];
  if (previous?.gatewaySession === sample.gatewaySession) {
    if (sample.sampledAt <= previous.sampledAt) return history;
    if (sample.sampledAt - previous.sampledAt <= 2_500 && sample.rxBytes >= previous.rxBytes && sample.txBytes >= previous.txBytes) {
      return [...history.filter((entry) => sample.sampledAt - entry.sampledAt < TRAFFIC_WINDOW * 1000).slice(-(TRAFFIC_WINDOW - 1)), sample];
    }
  }
  return [sample];
}

/** Paths in a 100 × 40 view box. Horizontal tangents keep the curve within its sample range. */
export function trafficCurve(history: readonly NetworkTraffic[], direction: "rxBps" | "txBps") {
  const latest = history[history.length - 1];
  const points = history.flatMap((sample) => {
    const value = sample[direction];
    return value == null ? [] : [{ value, age: (latest.sampledAt - sample.sampledAt) / 1000 }];
  });
  if (points.length < 2 || points.every((point) => point.value === 0)) return null;
  const max = Math.max(8 * 1024, ...points.map(({ value }) => value));
  const coordinates = points.map(({ age, value }) => ({
    x: Math.max(0, 100 - age * 100 / (TRAFFIC_WINDOW - 1)),
    y: 40 - value / max * 34,
  }));
  const first = coordinates[0];
  const last = coordinates[coordinates.length - 1];
  let line = `M${first.x.toFixed(1)},${first.y.toFixed(1)}`;
  for (let index = 1; index < coordinates.length; index++) {
    const a = coordinates[index - 1];
    const b = coordinates[index];
    const mid = ((a.x + b.x) / 2).toFixed(1);
    line += ` C${mid},${a.y.toFixed(1)} ${mid},${b.y.toFixed(1)} ${b.x.toFixed(1)},${b.y.toFixed(1)}`;
  }
  return { line, fill: `${line} L${last.x.toFixed(1)},40 H${first.x.toFixed(1)} Z` };
}
