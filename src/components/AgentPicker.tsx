import { Combobox, InputBase, SegmentedControl, useCombobox } from "@mantine/core";
import { IconCheck, IconChevronDown } from "@tabler/icons-react";
import type { AgentKind } from "../services/ipc/generated/core";
import { AgentLogo, AGENT_NAMES } from "./common";

/** Every supported agent, in the order the app shows them. */
export const AGENT_KINDS: AgentKind[] = ["claude", "codex", "opencode", "pi", "antigravity"];

export function isAgentKind(value: string | null | undefined): value is AgentKind {
  return value != null && (AGENT_KINDS as string[]).includes(value);
}

/**
 * A dropdown of agents, each with its logo and name. Agents that are not
 * installed stay choosable (a default may be set before installing) but say so.
 */
export function AgentSelect({ value, onChange, installed, width = 260 }: {
  value: AgentKind;
  onChange: (kind: AgentKind) => void;
  installed?: (kind: AgentKind) => boolean;
  width?: number;
}) {
  const combobox = useCombobox({ onDropdownClose: () => combobox.resetSelectedOption() });
  return (
    <Combobox store={combobox} withinPortal onOptionSubmit={(next) => { onChange(next as AgentKind); combobox.closeDropdown(); }}>
      <Combobox.Target>
        <InputBase
          component="button"
          type="button"
          pointer
          style={{ maxWidth: width }}
          leftSection={<AgentLogo agent={value} size={18} />}
          rightSection={<IconChevronDown size={14} />}
          rightSectionPointerEvents="none"
          onClick={() => combobox.toggleDropdown()}
          aria-label="Agent"
        >
          {AGENT_NAMES[value]}
        </InputBase>
      </Combobox.Target>
      <Combobox.Dropdown>
        <Combobox.Options>
          {AGENT_KINDS.map((kind) => {
            const missing = installed && !installed(kind);
            return (
              <Combobox.Option value={kind} key={kind} active={kind === value}>
                <span className="agent-option">
                  <AgentLogo agent={kind} size={20} />
                  <span className="grow">
                    {AGENT_NAMES[kind]}
                    {missing && <span className="faint"> · not installed</span>}
                  </span>
                  {kind === value && <IconCheck size={14} />}
                </span>
              </Combobox.Option>
            );
          })}
        </Combobox.Options>
      </Combobox.Dropdown>
    </Combobox>
  );
}

/** Filter by agent: one text segmented control whose pill glides to the choice; `null` is "All". */
export function AgentChips({ value, onChange, kinds = AGENT_KINDS }: {
  value: AgentKind | null;
  onChange: (kind: AgentKind | null) => void;
  kinds?: AgentKind[];
}) {
  return (
    <SegmentedControl
      className="agent-chips"
      size="xs"
      aria-label="Agent"
      value={value ?? ""}
      onChange={(next) => onChange(isAgentKind(next) ? next : null)}
      data={[{ value: "", label: "All" }, ...kinds.map((kind) => ({ value: kind, label: AGENT_NAMES[kind] }))]}
    />
  );
}
