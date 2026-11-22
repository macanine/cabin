// Pure logic for the install setup: presets, defaults and validation (the
// same rules as vm/src/machine.rs and config.rs).
import type { MachineInstallOptions, OsChoice } from "../services/ipc/generated/core";

/** Mac checks that only matter for one optional feature. */
export const OPTIONAL_CHECKS = ["socketVmnet"];
export const isOptionalCheck = (id: string) => OPTIONAL_CHECKS.includes(id);

export interface ResourcePreset {
  key: string;
  label: string;
  cpus: number;
  memoryMb: number;
  diskGb: number;
  note: string;
}

export const RESOURCE_PRESETS: ResourcePreset[] = [
  { key: "light", label: "Light", cpus: 2, memoryMb: 2048, diskGb: 12, note: "One agent at a time" },
  { key: "balanced", label: "Balanced", cpus: 4, memoryMb: 4096, diskGb: 20, note: "Most computers" },
  { key: "roomy", label: "Roomy", cpus: 8, memoryMb: 8192, diskGb: 40, note: "Several agents, big repos" },
];

export const DEFAULT_PRESET = "balanced";

export function defaultOptions(): MachineInstallOptions {
  const preset = RESOURCE_PRESETS.find((p) => p.key === DEFAULT_PRESET)!;
  return {
    cpus: preset.cpus,
    memoryMb: preset.memoryMb,
    diskSize: `${preset.diskGb}G`,
    username: null,
    hostname: null,
    device: false,
    aptMirror: null,
    os: null,
  };
}

/** The preset matching the options, or null for custom values. */
export function matchingPreset(options: MachineInstallOptions): ResourcePreset | null {
  return (
    RESOURCE_PRESETS.find(
      (p) => p.cpus === options.cpus && p.memoryMb === options.memoryMb && diskGb(options.diskSize) === p.diskGb,
    ) ?? null
  );
}

export function diskGb(size: string | null | undefined): number | null {
  const match = /^(\d+)G$/i.exec((size ?? "").trim());
  return match ? Number(match[1]) : null;
}

export function usernameProblem(name: string): string | null {
  if (name === "") return null;
  if (name.length > 32) return "Use at most 32 characters.";
  if (!/^[a-z_]/.test(name)) return "Start with a lowercase letter or _.";
  if (!/^[a-z_][a-z0-9_-]*$/.test(name)) return "Use only lowercase letters, digits, - and _.";
  if (name === "root") return "root is reserved; choose a normal user name.";
  return null;
}

export function hostnameProblem(name: string): string | null {
  if (name === "") return null;
  if (name.length > 63) return "Use at most 63 characters.";
  if (!/^[a-z0-9-]+$/.test(name)) return "Use only lowercase letters, digits and -.";
  if (name.startsWith("-") || name.endsWith("-")) return "It cannot start or end with -.";
  return null;
}

export function resourceProblem(options: MachineInstallOptions): string | null {
  const disk = diskGb(options.diskSize);
  if (!options.cpus || options.cpus < 1 || options.cpus > 64) return "Choose between 1 and 64 CPUs.";
  if (!options.memoryMb || options.memoryMb < 1024) return "Give the VM at least 1 GB of memory.";
  if (disk == null || disk < 8) return "Give the VM at least 8 GB of disk.";
  return null;
}

/** The chosen system among the backend's choices (the first one is the default). */
export function chosenOs(options: MachineInstallOptions, choices: OsChoice[]): OsChoice | null {
  return choices.find((c) => c.id === options.os) ?? choices[0] ?? null;
}

/** What keeps the install from starting, or null. */
export function formProblem(options: MachineInstallOptions): string | null {
  return usernameProblem(options.username ?? "") ?? hostnameProblem(options.hostname ?? "") ?? resourceProblem(options);
}

const gb = (mb: number) => `${mb / 1024 % 1 === 0 ? mb / 1024 : (mb / 1024).toFixed(1)} GB`;

/** "Ubuntu 24.04 LTS · 4 CPU · 4 GB memory · 20 GB disk" */
export function installSummary(options: MachineInstallOptions, os: OsChoice | null): string {
  return [
    os?.name ?? "Ubuntu",
    `${options.cpus ?? "?"} CPU`,
    `${gb(options.memoryMb ?? 0)} memory`,
    `${diskGb(options.diskSize) ?? "?"} GB disk`,
  ].join(" · ");
}

/** A mirror URI's host, for one-line summaries. */
export function mirrorHost(uri: string | null | undefined): string | null {
  if (!uri) return null;
  try { return new URL(uri).host; } catch { return uri; }
}
