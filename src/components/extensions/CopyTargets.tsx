import { Button, Checkbox, Modal } from "@mantine/core";
import { useEffect, useState } from "react";
import type { AgentKind } from "../../services/ipc/generated/core";
import { AGENT_KINDS } from "../AgentPicker";
import { AGENT_NAMES, AgentLogo } from "../common";

/** Pick the other agents a server or skill is copied to. */
export function CopyTargets({
  opened,
  kind,
  label,
  onClose,
  onCopy,
}: {
  opened: boolean;
  kind: AgentKind;
  label: string;
  onClose: () => void;
  onCopy: (targets: AgentKind[]) => Promise<void>;
}) {
  const [picked, setPicked] = useState<AgentKind[]>([]);
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    if (opened) setPicked([]);
  }, [opened]);

  async function copy() {
    if (!picked.length || saving) return;
    setSaving(true);
    try {
      await onCopy(picked);
      onClose();
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal opened={opened} onClose={onClose} title={`Copy ${label}`} centered size="sm">
      <p className="x-copy-hint">Choose the agents that should also have it.</p>
      <div className="x-copy-list">
        {AGENT_KINDS.filter((agent) => agent !== kind).map((agent) => (
          <label key={agent} className="x-copy-row">
            <Checkbox
              aria-label={AGENT_NAMES[agent]}
              checked={picked.includes(agent)}
              onChange={(event) => setPicked((old) => event.currentTarget.checked
                ? [...old, agent]
                : old.filter((entry) => entry !== agent))}
            />
            <AgentLogo agent={agent} size={18} />
            <span>{AGENT_NAMES[agent]}</span>
          </label>
        ))}
      </div>
      <div className="x-modal-actions">
        <Button variant="default" onClick={onClose}>Cancel</Button>
        <Button disabled={!picked.length} loading={saving} onClick={() => void copy()}>Copy</Button>
      </div>
    </Modal>
  );
}
