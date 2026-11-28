import { Channel, invoke } from "@tauri-apps/api/core";
import type { AppError, IpcCommand, TaskEvent } from "./generated/common";

export class IpcError extends Error {
  readonly code: string;
  readonly details?: string;

  constructor(error: AppError) {
    super(error.message);
    this.name = "IpcError";
    this.code = error.code;
    this.details = error.details;
  }
}

export function toIpcError(error: unknown): IpcError {
  if (error instanceof IpcError) return error;
  if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
    const value = error as AppError;
    return new IpcError({ code: String(value.code), message: String(value.message), details: value.details });
  }
  return new IpcError({ code: "unknown", message: error instanceof Error ? error.message : String(error) });
}

export async function invokeIpc<T>(command: IpcCommand, args: Record<string, unknown> = {}): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw toIpcError(error);
  }
}

type FrontendDiagnostic = "viewCrash" | "subscriberCrash" | "scriptError" | "unhandledRejection";

export function reportFrontendDiagnostic(kind: FrontendDiagnostic): void {
  void invokeIpc<void>("frontend_diagnostic", { kind }).catch(() => {});
}

/** Invoke a long-running command that reports its result on a channel. */
export function invokeTask<T>(command: IpcCommand, args: Record<string, unknown> = {}): Promise<T> {
  return new Promise((resolve, reject) => {
    const channel = new Channel<TaskEvent<T>>((event) => {
      if (event.type === "completed") resolve(event.result);
      else reject(new IpcError(event.error));
    });
    invokeIpc<void>(command, { channel, ...args }).catch(reject);
  });
}

/** Methods served by cabind inside the VM (see core/src/rpc.rs). */
export type GuestMethod =
  | "app_snapshot" | "clock" | "machine_observation" | "monitor_processes"
  | "workspace_create" | "workspace_update" | "workspace_duplicate" | "workspace_delete"
  | "terminal_create" | "terminal_snapshot" | "terminal_attach" | "terminal_paste_image"
  | "agent_terminal_open" | "session_create" | "session_start" | "session_stop" | "session_write" | "session_resize"
  | "session_update" | "session_duplicate" | "session_delete"
  | "worktree_refs" | "worktree_remove"
  | "chat_create" | "chat_snapshot" | "chat_attachment" | "chat_send" | "chat_steer" | "chat_set_option" | "chat_stop_task" | "chat_goal_control" | "chat_respond" | "chat_plan_respond" | "chat_interrupt" | "chat_retry"
  | "agent_models" | "chat_set_model" | "chat_set_permission"
  | "agents_refresh" | "agent_chat_capabilities"
  | "acp_runtimes" | "acp_runtime_versions" | "acp_runtime_install" | "acp_runtime_select" | "acp_runtime_remove" | "agent_config" | "agent_backups" | "agent_health"
  | "agent_set_model" | "agent_save_config" | "agent_restore_backup"
  | "agent_extensions" | "agent_catalog" | "workspace_files" | "agent_mcp_save" | "agent_mcp_remove" | "agent_mcp_set_enabled"
  | "mcp_library" | "mcp_library_edit" | "mcp_library_export" | "mcp_library_parse" | "mcp_library_change"
  | "skill_library" | "skill_library_read" | "skill_library_repositories" | "skill_library_discover"
  | "skill_library_registry" | "skill_library_install" | "skill_library_change" | "skill_library_updates"
  | "agent_mcp_copy" | "agent_skill_read" | "agent_skill_delete" | "agent_skill_copy"
  | "claude_usage" | "codex_accounts" | "codex_import_account" | "codex_export_account" | "codex_switch_account" | "codex_remove_account"
  | "codex_refresh_quota" | "codex_use_reset_card"
  | "environment_state" | "environment_review" | "environment_apply" | "network_probe"
  | "proxy_import" | "proxy_add_url" | "proxy_refresh" | "proxy_refresh_from_host" | "proxy_remove" | "proxy_probe"
  | "proxy_catalog" | "proxy_group_save" | "proxy_group_remove" | "proxy_node_test"
  | "network_state" | "network_apply" | "network_verify" | "network_pause" | "network_auto_recover" | "network_region_follow" | "network_system_route"
  | "network_check" | "network_interfaces" | "network_exit" | "network_exit_type" | "network_origin" | "network_locate" | "network_traffic" | "network_history"
  | "usage_dashboard" | "usage_sync" | "usage_sync_prices" | "usage_save_price"
  | "setup_checks" | "setup_run"
  | "node_state" | "node_releases" | "node_install" | "node_use" | "node_remove"
  | "operations_list" | "operation_cancel" | "operation_retry"
  | "fs_list" | "fs_read" | "git_status" | "git_diff" | "git_stage" | "git_unstage" | "git_discard" | "git_commit"
  | "listening_ports"
  | "settings_update";

/** Call cabind in the VM through the Mac app's SSH link. */
export function invokeGuest<T>(method: GuestMethod, params: Record<string, unknown> = {}): Promise<T> {
  return invokeIpc<T>("guest_invoke", { method, params });
}

export function subscribe<E, T>(command: IpcCommand, onEvent: (event: E) => void): Promise<T> {
  const channel = new Channel<E>(onEvent);
  return invokeIpc<T>(command, { channel });
}
