import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Checkbox, Loader, Switch, TextInput, Tooltip } from "@mantine/core";
import { IconSearch } from "@tabler/icons-react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import type { AgentKind, OperationRecord } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { invalidateCatalogs } from "../../state/agentCatalog";
import { uiActions } from "../../state/ui";
import { AGENT_KINDS } from "../AgentPicker";
import { AgentLogo, AGENT_NAMES } from "../common";

export function useExtensionLibrary<T>(load: () => Promise<T>) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const running = useRef(false);
  const generation = useRef(0);
  const reload = useCallback(async () => {
    const id = ++generation.current;
    try {
      const value = await load();
      if (generation.current === id) { setData(value); setError(""); }
    } catch (cause) {
      if (generation.current === id) setError(toIpcError(cause).message);
    }
  }, [load]);
  useAutoRefresh(() => { if (!running.current) return reload(); }, { interval: 30_000 });
  useEffect(() => () => { generation.current++; }, [reload]);

  async function change(action: () => Promise<OperationRecord>, onError?: (message: string) => void): Promise<boolean> {
    if (running.current) return false;
    running.current = true;
    setBusy(true);
    const toast = uiActions.toast("Updating extensions…", "loading");
    function fail(message: string) {
      if (onError) {
        uiActions.dismissToast(toast);
        onError(message);
      } else uiActions.updateToast(toast, { tone: "error", message });
      return false;
    }
    try {
      const record = await action();
      if (record.state !== "succeeded") return fail(record.error ?? record.title);
      uiActions.updateToast(toast, { tone: "success", message: record.title });
      invalidateCatalogs();
      return true;
    } catch (cause) {
      return fail(toIpcError(cause).message);
    } finally {
      await reload();
      running.current = false;
      setBusy(false);
    }
  }
  return { data, error, reload, busy, change };
}

export function ExtensionSearch({ value, onChange, placeholder }: {
  value: string; onChange: (value: string) => void; placeholder: string;
}) {
  return <TextInput aria-label={placeholder} placeholder={placeholder} value={value}
    leftSection={<IconSearch size={16} />} onChange={(event) => onChange(event.currentTarget.value)} />;
}

export function ExtensionStatus({ error }: { error: string }) {
  return error ? <div className="extension-empty" role="alert"><p>{error}</p></div>
    : <div className="extension-empty"><Loader size="sm" aria-label="Loading extensions" /></div>;
}

export function AgentTargets({ targets, onChange, disabled = [], hint }: {
  targets: AgentKind[]; onChange: (targets: AgentKind[]) => void; disabled?: AgentKind[]; hint?: string;
}) {
  return <fieldset className="extension-targets" aria-describedby={hint ? "extension-target-hint" : undefined}>
    <legend>Enable for</legend>
    {AGENT_KINDS.map((kind) => <Checkbox key={kind} disabled={disabled.includes(kind)}
      checked={targets.includes(kind)} label={<span className="extension-agent-name"><span aria-hidden="true"><AgentLogo agent={kind} size={16} /></span>{AGENT_NAMES[kind]}</span>}
      onChange={(event) => onChange(event.currentTarget.checked ? [...targets, kind] : targets.filter((target) => target !== kind))} />)}
    {hint && <span id="extension-target-hint" className="extension-target-hint">{hint}</span>}
  </fieldset>;
}

export function AgentColumn({ kind }: { kind: AgentKind }) {
  return <Tooltip label={AGENT_NAMES[kind]}><span className="extension-agent-column" aria-label={AGENT_NAMES[kind]}>
    <AgentLogo agent={kind} size={19} />
  </span></Tooltip>;
}

export function BindingSwitch({ kind, name, state, wanted, disabled, onChange, note }: {
  kind: AgentKind; name: string; state: string; wanted: boolean;
  disabled: boolean; onChange: (enabled: boolean) => void; note?: string;
}) {
  const label = `${AGENT_NAMES[kind]} · ${name}`;
  const statuses: Record<string, string> = { unreadable: "Unreadable", conflict: "Different native configuration", unsupported: "Unsupported", local: "Local installation" };
  const status = statuses[state];
  if (status || note) return <Tooltip label={note ?? status}><span className="extension-unavailable" aria-label={`${label}: ${note ?? status}`}>
    {state === "local" && !note ? "Local" : "–"}
  </span></Tooltip>;
  const active = state === "enabled";
  return <Tooltip label={active !== wanted && !disabled ? `Saved setting: ${wanted ? "on" : "off"}` : label}>
    <span className="extension-switch"><Switch size="xs" aria-label={label} checked={active} disabled={disabled}
      onChange={(event) => onChange(event.currentTarget.checked)} />{active !== wanted && !disabled && <span className="extension-drift" />}</span>
  </Tooltip>;
}

/** Filter a library by agent: everything, or what one agent has enabled. */
export function AgentRail({ value, total, count, onChange, children, label }: {
  value: AgentKind | null;
  total: number;
  count: (kind: AgentKind) => number;
  onChange: (kind: AgentKind | null) => void;
  label: string;
  /** More filters under the agents. */
  children?: ReactNode;
}) {
  return (
    <nav className="split-list !w-[208px] max-[860px]:!w-auto" aria-label={label}>
      <button type="button" className="split-choice press" aria-current={value == null ? "true" : undefined} onClick={() => onChange(null)}>
        <span>All</span><span className="split-count">{total}</span>
      </button>
      <div className="split-label">Enabled for</div>
      {AGENT_KINDS.map((kind) => (
        <button key={kind} type="button" className="split-choice press" aria-current={value === kind ? "true" : undefined} onClick={() => onChange(value === kind ? null : kind)}>
          <span>{AGENT_NAMES[kind]}</span><span className="split-count">{count(kind)}</span>
        </button>
      ))}
      {children}
    </nav>
  );
}
