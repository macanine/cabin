import type { AppEvent, SessionStatus, TerminalSnapshot } from "../services/ipc/generated/core";

export type TerminalEvent = Extract<AppEvent, { type: "sessionOutput" | "terminalStatus" }>;

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { ignoreBOM: true });
const statusOrder: Record<SessionStatus["state"], number> = {
  stopped: 0, starting: 1, running: 2, exited: 3, failed: 3,
};

/** Position of the emulator in one generation of the guest's UTF-8 stream. */
export class TerminalReplay {
  generation: number | undefined;
  status: SessionStatus | undefined;
  private end = 0;

  /** Apply snapshots monotonically. An expired tail requires a fresh screen. */
  snapshot(snapshot: TerminalSnapshot): { reset: boolean; data: string } {
    if (this.generation != null && snapshot.generation < this.generation) return { reset: false, data: "" };
    const sameGeneration = snapshot.generation === this.generation;
    const reset = !sameGeneration || this.end < snapshot.startOffset;
    if (!sameGeneration || !this.status || statusOrder[snapshot.status.state] >= statusOrder[this.status.state]) {
      this.status = snapshot.status;
    }
    this.generation = snapshot.generation;
    if (reset || snapshot.endOffset > this.end) {
      const data = reset
        ? snapshot.data
        : decoder.decode(encoder.encode(snapshot.data).subarray(this.end - snapshot.startOffset));
      this.end = snapshot.endOffset;
      return { reset, data };
    }
    return { reset: false, data: "" };
  }

  /** A null result means history must be fetched before more output is used. */
  event(event: TerminalEvent): string | null {
    const generation = this.generation;
    if (generation == null || event.generation > generation) return null;
    if (event.generation < generation) return "";
    if (event.type === "terminalStatus") {
      if (!this.status || statusOrder[event.status.state] >= statusOrder[this.status.state]) this.status = event.status;
      return "";
    }
    if (event.offset > this.end) return null;
    const bytes = encoder.encode(event.data);
    const end = event.offset + bytes.length;
    if (end <= this.end) return "";
    const data = event.offset === this.end ? event.data : decoder.decode(bytes.subarray(this.end - event.offset));
    this.end = end;
    return data;
  }
}
