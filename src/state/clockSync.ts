// Keeps lib/clock.ts's guest offset current: measured when the guest link
// comes up, every few minutes, and when the window regains focus (after the
// Mac sleeps, a VM's clock can jump). Failures keep the last estimate.
import { appIpc } from "../services/ipc/app";
import { applyClock, estimateOffset, type ClockSample } from "../lib/clock";
import { appStore } from "./store";

const SAMPLES = 3;
const EVERY_MS = 5 * 60 * 1000;
/** Focus right after a sync does not sync again. */
const MIN_GAP_MS = 30 * 1000;

let started = false;
let running: Promise<void> | null = null;
let lastRun = 0;

async function measure(): Promise<void> {
  const samples: ClockSample[] = [];
  let zone: string | null = null;
  for (let i = 0; i < SAMPLES; i++) {
    const sent = Date.now();
    try {
      const reply = await appIpc.clock();
      samples.push({ sent, guest: reply.nowMs, received: Date.now() });
      zone = reply.timezone ?? zone;
    } catch {
      break; // Link down or an older guest: keep what we have.
    }
  }
  applyClock(estimateOffset(samples), zone);
}

export function syncClock(force = false): Promise<void> {
  if (!appStore.get().guestReady) return Promise.resolve();
  if (running) return running;
  if (!force && Date.now() - lastRun < MIN_GAP_MS) return Promise.resolve();
  lastRun = Date.now();
  running = measure().finally(() => { running = null; });
  return running;
}

export function startClockSync() {
  if (started) return;
  started = true;
  let ready = appStore.get().guestReady;
  if (ready) void syncClock(true);
  appStore.subscribe(() => {
    const now = appStore.get().guestReady;
    if (now && !ready) void syncClock(true);
    ready = now;
  });
  window.setInterval(() => void syncClock(true), EVERY_MS);
  window.addEventListener("focus", () => void syncClock());
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void syncClock();
  });
}
