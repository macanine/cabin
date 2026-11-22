import { Button, Menu, Modal, Tooltip } from "@mantine/core";
import { CHIP, Tile } from "./ComposerChip";
import {
  IconCheck, IconChevronDown, IconListCheck, IconLock, IconPencil, IconShieldCheck, IconShieldOff,
} from "@tabler/icons-react";
import { useState } from "react";
import { useChatCapabilities } from "../state/chatCapabilities";
import type { AgentKind, ChatPermissionMode } from "../services/ipc/generated/core";
import { toIpcError } from "../services/ipc/common";
import { uiActions } from "../state/ui";

const PERMISSIONS: ReadonlyArray<{
  value: ChatPermissionMode;
  label: string;
  description: string;
  icon: typeof IconLock;
  /** How much the mode lets the agent do unasked: drives its colour. */
  tone: "neutral" | "info" | "ok" | "warn" | "bad";
}> = [
  {
    value: "approval-required",
    label: "Supervised",
    tone: "ok",
    description: "Ask before commands and file changes.",
    icon: IconLock,
  },
  {
    value: "auto-accept-edits",
    label: "Auto-accept edits",
    tone: "warn",
    description: "Approve file edits inside the workspace; ask before other actions.",
    icon: IconPencil,
  },
  {
    value: "auto",
    label: "Auto",
    tone: "warn",
    description: "The agent handles routine actions and still asks for risky ones.",
    icon: IconShieldCheck,
  },
  {
    value: "plan",
    label: "Plan first",
    tone: "info",
    description: "Read and plan without changing the workspace; approve the plan before execution.",
    icon: IconListCheck,
  },
  {
    value: "full-access",
    label: "Full access",
    tone: "bad",
    description: "Run commands and edit files without approval prompts.",
    icon: IconShieldOff,
  },
];

export const DEFAULT_CHAT_PERMISSION: ChatPermissionMode = "approval-required";

export function PermissionPicker({
  agent, value, disabled, onChange,
}: {
  agent: AgentKind;
  value: ChatPermissionMode;
  disabled?: boolean;
  onChange: (permission: ChatPermissionMode) => void | Promise<void>;
}) {
  const capabilities = useChatCapabilities(agent);
  const [opened, setOpened] = useState(false);
  const [confirmFullAccess, setConfirmFullAccess] = useState(false);
  const [applying, setApplying] = useState(false);
  const current = PERMISSIONS.find((permission) => permission.value === value) ?? PERMISSIONS[0];
  const CurrentIcon = current.icon;
  const chipTone = { neutral: "", info: "!text-info", ok: "", warn: "", bad: "!text-bad" }[current.tone];

  if (capabilities.permissionModes.length === 0) return null;

  async function apply(permission: ChatPermissionMode) {
    if (permission === value || applying) return;
    setApplying(true);
    try {
      await onChange(permission);
    } catch (error) {
      uiActions.toast(toIpcError(error).message, "error");
    } finally {
      setApplying(false);
    }
  }

  function choose(permission: ChatPermissionMode) {
    setOpened(false);
    if (permission === "full-access" && value !== "full-access") {
      setConfirmFullAccess(true);
      return;
    }
    void apply(permission);
  }

  return (
    <>
      <Menu opened={opened} onChange={setOpened} position="top-start" width={330} withinPortal shadow="md">
        <Menu.Target>
          <Tooltip label={current.description} openDelay={300} withinPortal disabled={opened}>
            <button type="button" className={`${CHIP} ${chipTone}`} disabled={disabled || applying} aria-label={`Permissions: ${current.label}`}>
              <CurrentIcon size={14} className="shrink-0" />
              <span className="truncate max-sm:hidden">{current.label}</span>
              <IconChevronDown size={12} className="shrink-0 opacity-60" />
            </button>
          </Tooltip>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Label>Permissions</Menu.Label>
          {PERMISSIONS.filter((permission) => capabilities.permissionModes.includes(permission.value)).map((permission) => {
            const OptionIcon = permission.icon;
            const selected = permission.value === value;
            return (
              <Menu.Item
                key={permission.value}
                onClick={() => choose(permission.value)}
                className={selected ? "!bg-accent-soft/60" : ""}
                leftSection={<Tile tone={permission.tone === "neutral" ? "neutral" : permission.tone}><OptionIcon size={15} /></Tile>}
                rightSection={selected ? <IconCheck size={15} className="text-accent" /> : null}
              >
                <span className="grid gap-px whitespace-normal">
                  <strong className={`text-[13px] font-semibold ${permission.tone === "bad" ? "text-bad" : ""}`}>{permission.label}</strong>
                  <small className="text-xs leading-snug text-muted">{permission.description}</small>
                </span>
              </Menu.Item>
            );
          })}
        </Menu.Dropdown>
      </Menu>
      <Modal
        opened={confirmFullAccess}
        onClose={() => setConfirmFullAccess(false)}
        title="Enable full access?"
        centered
        size="sm"
      >
        <div className="mb-4 flex items-start gap-3">
          <Tile tone="bad"><IconShieldOff size={16} /></Tile>
          <p className="m-0 text-[13px] leading-relaxed text-muted">
            The agent will be able to run commands and change files in this chat without asking for approval.
            Choose Supervised when you want to review each sensitive action.
          </p>
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="default" onClick={() => setConfirmFullAccess(false)}>Cancel</Button>
          <Button
            color="red"
            loading={applying}
            onClick={() => {
              setConfirmFullAccess(false);
              void apply("full-access");
            }}
          >
            Enable full access
          </Button>
        </div>
      </Modal>
    </>
  );
}
