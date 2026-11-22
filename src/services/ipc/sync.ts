import type {
  SyncMode, SyncPreview, SyncProjectInput, SyncRules, SyncView,
} from "./generated/core";
import { invokeIpc } from "./common";

export type { GitHandling, SyncMode, SyncPreview, SyncPreviewItem, SyncRules, SyncStatus, SyncView } from "./generated/core";

/** Project sync: Mac folders kept in step with folders in the machine. */
export const syncIpc = {
  list: () => invokeIpc<SyncView[]>("sync_list"),
  preview: (hostPath: string, rules: SyncRules, mode: SyncMode) => invokeIpc<SyncPreview>("sync_preview", { hostPath, rules, mode }),
  create: (input: SyncProjectInput) => invokeIpc<SyncView>("sync_create", { input }),
  update: (id: string, change: { mode?: SyncMode; rules?: SyncRules; paused?: boolean }) =>
    invokeIpc<SyncView>("sync_update", { id, mode: change.mode ?? null, rules: change.rules ?? null, paused: change.paused ?? null }),
  remove: (id: string) => invokeIpc<void>("sync_remove", { id }),
  resolve: (id: string, path: string, keep: "mac" | "machine") => invokeIpc<void>("sync_resolve", { id, path, keep }),
};
