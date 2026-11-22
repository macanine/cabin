import { useAutoRefresh } from "../lib/useAutoRefresh";
import { Button, Loader, Menu } from "@mantine/core";
import { IconBrandNodejs, IconDots, IconPlus } from "@tabler/icons-react";
import { useCallback, useEffect, useState } from "react";
import type { NodeSource, NodeState } from "../services/ipc/generated/core";
import { nodeIpc } from "../services/ipc/node";
import { runOperation } from "../lib/operations";
import { majorOf } from "../lib/nodeVersions";
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
      <div className="list" style={{ marginBottom: 16 }}>
        <div className="list-row">
          <IconBrandNodejs size={20} color={active && !tooOld ? "var(--ok)" : "var(--warn)"} />
          <div className="grow">
            {state == null ? (
              <span className="row muted">
                <Loader size="xs" /> Reading Node.js…
              </span>
            ) : active ? (
              <>
                <div>
                  Node.js <strong>{active.version}</strong>
                  {active.npm && <span className="muted"> · npm {active.npm}</span>}{" "}
                  <span className={`pill ${tooOld ? "warn" : active.source === "managed" ? "ok" : ""}`}>
                    {tooOld ? "End-of-life" : SOURCE_LABEL[active.source]}
                  </span>
                </div>
                <div className="muted mono fs-12">{active.path}</div>
              </>
            ) : (
              <div>No Node.js on PATH</div>
            )}
          </div>
          <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => setWizard(true)} disabled={state == null}>
            {active && !tooOld ? "Install or switch…" : `Install Node.js ${state?.recommendedMajor ?? ""} LTS…`}
          </Button>
        </div>
        {state?.installed.map((item) => (
          <div className="list-row" key={item.version} style={{ paddingLeft: 46 }}>
            <div className="grow">
              <span className="mono">{item.version}</span>
              {item.current && <span className="pill ok" style={{ marginLeft: 8 }}>Default</span>}
            </div>
            {!item.current && (
              <>
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
              </>
            )}
          </div>
        ))}
        {state?.system && active?.source !== "system" && (
          <div className="list-row faint" style={{ paddingLeft: 46, fontSize: 13 }}>
            Ubuntu's node {state.system.version} is also installed ({state.system.path}) but not used.
          </div>
        )}
      </div>
      <NodeVersionWizard opened={wizard} state={state} onClose={() => setWizard(false)} onChanged={changed} />
    </>
  );
}
