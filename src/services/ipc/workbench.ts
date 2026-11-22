import { invokeGuest, invokeIpc } from "./common";
import type {
  FsFile, FsListing, GitCommitResult, GitFileDiff, GitStatus, ListeningPort,
} from "./generated/core";

/** Where the panel works: the session's worktree, else its workspace folder (decided by the guest). */
export interface WorkbenchRoot { workspaceId: string; sessionId: string | null }

export interface PreviewForward { port: number; url: string }

export const workbenchIpc = {
  list: (root: WorkbenchRoot, path: string) => invokeGuest<FsListing>("fs_list", { ...root, path }),
  read: (root: WorkbenchRoot, path: string) => invokeGuest<FsFile>("fs_read", { ...root, path }),
  gitStatus: (root: WorkbenchRoot) => invokeGuest<GitStatus>("git_status", { ...root }),
  gitDiff: (root: WorkbenchRoot, path: string, staged: boolean) => invokeGuest<GitFileDiff>("git_diff", { ...root, path, staged }),
  stage: (root: WorkbenchRoot, paths: string[]) => invokeGuest<GitStatus>("git_stage", { ...root, paths }),
  unstage: (root: WorkbenchRoot, paths: string[]) => invokeGuest<GitStatus>("git_unstage", { ...root, paths }),
  discard: (root: WorkbenchRoot, paths: string[]) => invokeGuest<GitStatus>("git_discard", { ...root, paths }),
  commit: (root: WorkbenchRoot, message: string, all: boolean) => invokeGuest<GitCommitResult>("git_commit", { ...root, message, all }),
  ports: () => invokeGuest<ListeningPort[]>("listening_ports"),
  /** A Mac-local URL that reaches the VM's `localhost:<port>`. */
  forward: (port: number) => invokeIpc<PreviewForward>("preview_forward", { port }),
  closeForward: (port: number) => invokeIpc<void>("preview_close", { port }),
};
