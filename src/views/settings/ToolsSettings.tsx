import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button, Loader } from "@mantine/core";
import { useCallback, useState } from "react";
import type { CheckStatus, SetupAction, SetupCheck } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { appIpc } from "../../services/ipc/app";
import { runOperation } from "../../lib/operations";
import { NodePanel } from "../../components/NodePanel";
import { Section, StateIcon, type StateTone } from "../../components/common";
import { SettingsPage } from "../SettingsView";

const CHECK_TONE: Record<CheckStatus, StateTone> = { ok: "ok", warning: "warn", missing: "bad" };

/** Facts about the installed machine that only matter when they are wrong. */
const QUIET_WHEN_OK = new Set(["os", "arch"]);

function actionLabel(action: SetupAction): string {
  if (action.kind === "installBasePackages") return "Install";
  if (action.kind === "installPython") return "Install Python + uv";
  return "Install";
}

/** Runtimes and system tools inside the machine. Agents have their own tab;
 * Node.js has its own panel (versions, default, wizard). */
export function ToolsSettings() {
  const [checks, setChecks] = useState<SetupCheck[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try { setChecks(await appIpc.setupChecks()); setError(null); }
    catch (cause) { setError(toIpcError(cause).message); }
  }, []);

  useAutoRefresh(() => { if (!busy) return load(); }, { interval: 30_000 });

  async function run(check: SetupCheck) {
    if (!check.action) return;
    setBusy(check.id);
    await runOperation(appIpc.runSetup(check.action), `${check.label}: installed`);
    setBusy(null);
    await load();
  }

  const rows = (checks ?? []).filter(
    (check) => !check.id.startsWith("agent-") && check.id !== "node" && !(QUIET_WHEN_OK.has(check.id) && check.status === "ok"),
  );
  const todo = rows.filter((check) => check.status !== "ok").length;
  // Git and the other base packages share one install action; offer it once.
  const seen = new Set<string>();

  return (
    <SettingsPage
      title="Tools"
      subtitle="Runtimes and system tools inside the machine. Installs use the proxy from Network."
    >
      <Section title="Node.js" hint="Some agent CLIs require Node.js. Official builds are installed into /opt/node.">
        <NodePanel openSignal={0} onChanged={load} />
      </Section>
      <Section
        title="System"
        hint={checks == null ? undefined : todo === 0 ? "Everything is in place." : `${todo} item${todo === 1 ? " needs" : "s need"} attention.`}
      >
        {error && <p className="text-sm text-warn" role="status">{error}</p>}
        {checks == null && !error ? (
          <div className="row muted">
            <Loader size="sm" /> Checking the machine…
          </div>
        ) : (
          <div className="list">
            {rows.map((check) => {
              const key = check.action ? JSON.stringify(check.action) : "";
              const showAction = check.action != null && !seen.has(key);
              if (check.action) seen.add(key);
              return (
                <div className="list-row" key={check.id}>
                  <StateIcon tone={CHECK_TONE[check.status]} size={18} />
                  <div className="grow">
                    <div>{check.label}</div>
                    <div className="muted fs-13">{check.detail}</div>
                  </div>
                  {showAction && check.action && (
                    <Button size="xs" loading={busy === check.id} disabled={busy != null} onClick={() => void run(check)}>
                      {actionLabel(check.action)}
                    </Button>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </Section>
    </SettingsPage>
  );
}
