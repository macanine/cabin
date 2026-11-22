import { invokeGuest } from "./common";
import type { ProcessList } from "./generated/core";

export type { ProcessList };

export const monitorIpc = {
  /** The busiest processes by CPU and by memory (asked for only while shown). */
  processes: (limit = 8) => invokeGuest<ProcessList>("monitor_processes", { limit }),
};
