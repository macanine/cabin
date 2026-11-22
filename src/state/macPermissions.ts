import { useSyncExternalStore } from "react";
import { toIpcError } from "../services/ipc/common";
import type { MacProjectAccess } from "../services/ipc/generated/common";
import { macPermissionsIpc } from "../services/ipc/permissions";

let state: { projects: MacProjectAccess[] | null; checking: boolean; error: string | null } = {
  projects: null, checking: false, error: null,
};
let revision = 0;
const listeners = new Set<() => void>();

function set(next: typeof state) {
  state = next;
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function useMacPermissions() {
  return useSyncExternalStore(subscribe, () => state);
}

export async function refreshMacPermissions() {
  const requested = ++revision;
  set({ ...state, checking: true, error: null });
  try {
    const projects = await macPermissionsIpc.list();
    if (requested === revision) set({ projects, checking: false, error: null });
  } catch (error) {
    if (requested === revision) set({ ...state, checking: false, error: toIpcError(error).message });
  }
}
