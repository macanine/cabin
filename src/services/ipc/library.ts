import type {
  AgentKind, AgentModel, ChatAttachmentData, ChatAttachmentInput, ChatPermissionMode, ChatSnapshot, Session,
  PlanDecision, PlanRespondResult, SessionPatch, TerminalSnapshot, Workspace, WorkspacePatch, WorktreeRefs,
} from "./generated/core";
import { invokeGuest } from "./common";

export const libraryIpc = {
  createWorkspace: (name: string, path: string) => invokeGuest<Workspace>("workspace_create", { name, path }),
  updateWorkspace: (id: string, patch: WorkspacePatch) => invokeGuest<Workspace>("workspace_update", { id, patch }),
  duplicateWorkspace: (id: string) => invokeGuest<Workspace>("workspace_duplicate", { id }),
  deleteWorkspace: (id: string) => invokeGuest<void>("workspace_delete", { id }),

  createSession: (workspaceId: string, agent: AgentKind, prompt: string | null, cols = 100, rows = 30) =>
    invokeGuest<Session>("session_create", { workspaceId, agent, name: null, prompt, cols, rows }),
  createChat: (
    workspaceId: string,
    agent: AgentKind,
    prompt: string | null,
    model: string | null,
    attachments: ChatAttachmentInput[],
    permission: ChatPermissionMode,
    worktreeBase: string | null = null,
  ) => invokeGuest<Session>("chat_create", { workspaceId, agent, prompt, model, attachments, permission, worktreeBase }),
  worktreeRefs: (workspaceId: string) => invokeGuest<WorktreeRefs>("worktree_refs", { workspaceId }),
  removeWorktree: (id: string) => invokeGuest<Session>("worktree_remove", { id }),
  chatSnapshot: (id: string) => invokeGuest<ChatSnapshot>("chat_snapshot", { id }),
  agentModels: (workspaceId: string, agent: AgentKind) => invokeGuest<AgentModel[]>("agent_models", { workspaceId, agent }),
  chatSetModel: (id: string, model: string | null) => invokeGuest<void>("chat_set_model", { id, model }),
  chatSetPermission: (id: string, permission: ChatPermissionMode) =>
    invokeGuest<void>("chat_set_permission", { id, permission }),
  chatAttachment: (id: string, attachmentId: string) => invokeGuest<ChatAttachmentData>("chat_attachment", { id, attachmentId }),
  chatSend: (id: string, text: string, attachments: ChatAttachmentInput[]) => invokeGuest<void>("chat_send", { id, text, attachments }),
  /** A message for the running turn (falls back to a new prompt when the agent is idle). */
  chatSteer: (id: string, text: string, attachments: ChatAttachmentInput[]) => invokeGuest<void>("chat_steer", { id, text, attachments }),
  /** Change one of the agent's live session options (mode, thought level, …). */
  chatSetOption: (id: string, optionId: string, value: string) => invokeGuest<void>("chat_set_option", { id, optionId, value }),
  chatStopTask: (id: string, taskId: string) => invokeGuest<boolean>("chat_stop_task", { id, taskId }),
  chatGoalControl: (id: string, action: "pause" | "clear") =>
    invokeGuest<void>("chat_goal_control", { id, action }),
  chatRespond: (id: string, requestId: string, decision: string, answers: Record<string, string[]>) =>
    invokeGuest<void>("chat_respond", { id, requestId, decision, answers }),
  chatPlanRespond: (
    id: string,
    planId: string,
    expectedRevision: number,
    decision: PlanDecision,
    feedback?: string,
  ) => invokeGuest<PlanRespondResult>("chat_plan_respond", {
    id, planId, expectedRevision, decision, feedback: feedback ?? null,
  }),
  chatInterrupt: (id: string) => invokeGuest<void>("chat_interrupt", { id }),
  /** Workspace files for `@` mentions in the composer. */
  workspaceFiles: (workspaceId: string, sessionId: string | null = null) => invokeGuest<string[]>("workspace_files", { workspaceId, sessionId }),
  /** A plain terminal (login shell) in the workspace folder. */
  createTerminal: (workspaceId: string) => invokeGuest<Session>("terminal_create", { workspaceId }),
  terminalSnapshot: (id: string) => invokeGuest<TerminalSnapshot>("terminal_snapshot", { id }),
  /** Saves a pasted image in the machine; returns the path to type. */
  terminalPasteImage: (id: string, mimeType: string, data: string) =>
    invokeGuest<{ path: string }>("terminal_paste_image", { id, mimeType, data }),
  attachTerminal: (id: string, generation: number, retry: boolean, cols: number, rows: number) =>
    invokeGuest<TerminalSnapshot>("terminal_attach", { id, generation, retry, cols, rows }),
  startSession: (id: string, resume: boolean, cols: number, rows: number) =>
    invokeGuest<Session>("session_start", { id, resume, cols, rows }),
  stopSession: (id: string) => invokeGuest<void>("session_stop", { id }),
  writeSession: (id: string, generation: number, data: string) =>
    invokeGuest<void>("session_write", { id, generation, data }),
  resizeSession: (id: string, generation: number, cols: number, rows: number) =>
    invokeGuest<void>("session_resize", { id, generation, cols, rows }),
  updateSession: (id: string, patch: SessionPatch) => invokeGuest<Session>("session_update", { id, patch }),
  duplicateSession: (id: string) => invokeGuest<Session>("session_duplicate", { id }),
  deleteSession: (id: string) => invokeGuest<void>("session_delete", { id }),
};
