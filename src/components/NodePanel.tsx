import { useAutoRefresh } from "../lib/useAutoRefresh";
import { Button, Loader, Menu } from "@mantine/core";
import { IconBrandNodejs, IconDots, IconPlus } from "@tabler/icons-react";
import { useCallback, useEffect, useState } from "react";
import type { NodeSource, NodeState } from "../services/ipc/generated/core";
import { nodeIpc } from "../services/ipc/node";
import { runOperation } from "../lib/operations";
import { majorOf } from "../lib/nodeVersions";
import { SettingRow } from "./common";
import { NodeVersionWizard } from "./NodeVersionWizard";

const SOURCE_LABEL: Record<NodeSource, string> = {
  managed: "managed by cabin",
  system: "Ubuntu package",
  other: "on PATH",
};

/** Node.js inside the machine: what sessions use, the installed versions, and
 * the wizard to install or switch. `openSignal` opens the wizard when it
 * changes (the Setup check's button). */
export function NodePanel({ openSignal, onChanged }: { openSignal: number; onChanged: () => void }) {
  const [state, setState] = useState<NodeState | null>(null);
  const [wizard, setWizard] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    const result = await nodeIpc.state().catch(() => null);
    if (result) setState(result);
  }, []);

  useAutoRefresh(() => { if (!busy) return load(); }, { interval: 30_000 });

  useEffect(() => {
    if (openSignal > 0) setWizard(true);
  }, [openSignal]);

  const changed = () => {
    void load();
    onChanged();
  };

  async function act(version: string, task: () => Promise<unknown>) {
    setBusy(version);
    await task();
    setBusy(null);
    changed();
  }

  const active = state?.active ?? null;
  const activeMajor = active ? majorOf(active.version) : null;
  const tooOld = activeMajor != null && state != null && activeMajor < state.minimumMajor;

  return (
    <>
      <SettingRow
        label={state == null ? <span className="inline-flex items-center gap-2 text-muted"><Loader size="xs" /> Reading Node.js…</span>
          : <span className="inline-flex flex-wrap items-center gap-2">
            <IconBrandNodejs size={18} color={active && !tooOld ? "var(--ok)" : "var(--warn)"} />
            {active ? <>Node.js {active.version}{active.npm && <span className="text-muted">npm {active.npm}</span>}
              <span className={`pill ${tooOld ? "warn" : active.source === "managed" ? "ok" : ""}`}>{tooOld ? "End-of-life" : SOURCE_LABEL[active.source]}</span>
            </> : "No Node.js on PATH"}
          </span>}
        hint={active ? <span className="mono">{active.path}</span> : "Some agent CLIs require Node.js. Official builds are installed into /opt/node."}
      >
        <Button size="xs" variant={active && !tooOld ? "default" : "filled"} leftSection={<IconPlus size={14} />} onClick={() => setWizard(true)} disabled={state == null}>
          {active && !tooOld ? "Install or switch…" : `Install Node.js ${state?.recommendedMajor ?? ""} LTS…`}
        </Button>
      </SettingRow>
      {state?.installed.map((item) => (
        <SettingRow key={item.version} label={<span className="inline-flex items-center gap-2"><span className="mono">{item.version}</span>{item.current && <span className="pill ok">Default</span>}</span>}>
          {!item.current ? (
            <span className="flex items-center gap-1">
              <Button
                size="xs"
                variant="default"
                loading={busy === item.version}
                disabled={busy != null}
                onClick={() => act(item.version, () => runOperation(nodeIpc.use(item.version), `Node.js ${item.version} is now the default`))}
              >
                Make default
              </Button>
              <Menu position="bottom-end" withinPortal>
                <Menu.Target>
                  <Button size="xs" variant="subtle" color="gray" aria-label={`More for ${item.version}`} disabled={busy != null}>
                    <IconDots size={14} />
                  </Button>
                </Menu.Target>
                <Menu.Dropdown>
                  <Menu.Item
                    color="red"
                    onClick={() => act(item.version, () => runOperation(nodeIpc.remove(item.version), `Removed Node.js ${item.version}`))}
                  >
                    Remove {item.version}
                  </Menu.Item>
                </Menu.Dropdown>
              </Menu>
            </span>
          ) : undefined}
        </SettingRow>
      ))}
      {state?.system && active?.source !== "system" && (
        <SettingRow label="Ubuntu's Node.js" hint={`${state.system.version} is also installed (${state.system.path}) but not used.`} />
      )}
      <NodeVersionWizard opened={wizard} state={state} onClose={() => setWizard(false)} onChanged={changed} />
    </>
  );
}
