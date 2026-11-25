import { Modal } from "@mantine/core";
import type { ReactNode } from "react";
import { contextFill } from "../../lib/agentControls";
import { formatInteger } from "../../lib/format";
import type { SessionCommand } from "../../lib/sessionCommands";
import type { ChatContextUse, ChatPermissionMode, ChatState } from "../../services/ipc/generated/core";
import { permissionLabel } from "../PermissionPicker";

const stateLabels: Record<ChatState["state"], string> = {
  starting: "Connecting", ready: "Ready", running: "Running", waiting: "Waiting for input",
  switching: "Switching model", stopped: "Stopped", error: "Error",
};

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[auto_minmax(0,1fr)] items-baseline gap-x-4 border-t border-line py-2.5 first:border-t-0">
      <dt className="text-muted">{label}</dt>
      <dd className="m-0 text-right font-medium [overflow-wrap:anywhere]">{children}</dd>
    </div>
  );
}

export function SessionCommandDialog({ command, opened, onClose, onExited, state, agentName, model, permission, context }: {
  command: SessionCommand;
  opened: boolean;
  onClose: () => void;
  onExited: () => void;
  state: ChatState;
  agentName: string;
  model: string | null;
  permission: ChatPermissionMode;
  context: ChatContextUse | null;
}) {
  const fill = contextFill(context);
  return (
    <Modal
      opened={opened} onClose={onClose} onExitTransitionEnd={onExited} returnFocus={false}
      title={command === "status" ? "Session status" : "Context window"}
      closeButtonProps={{ "aria-label": "Close session details" }} centered size="sm"
    >
      {command === "status" ? (
        <>
          <dl className="m-0 text-sm">
            <Detail label="State">{stateLabels[state.state]}</Detail>
            <Detail label="Agent">{agentName}</Detail>
            <Detail label="Model">{model ?? "No model reported"}</Detail>
            <Detail label="Permissions">{permissionLabel(permission)}</Detail>
          </dl>
          {state.state === "error" && state.message && <p className="mb-0 rounded-lg bg-bad/8 px-3 py-2 text-sm text-bad [overflow-wrap:anywhere]">{state.message}</p>}
        </>
      ) : context && fill != null ? (
        <div className="rounded-xl border border-line px-4 py-3">
          <p className="m-0 font-semibold tracking-tight text-fg">
            <span className="text-2xl tabular-nums">{Math.round(fill * 100)}%</span> used
          </p>
          <p className="mt-1 mb-0 text-sm tabular-nums text-muted">
            {formatInteger(context.used)} of {formatInteger(context.size)} tokens
          </p>
        </div>
      ) : (
        <p className="m-0 pb-1 text-sm text-muted">The agent has yet to report context window usage for this session.</p>
      )}
    </Modal>
  );
}
