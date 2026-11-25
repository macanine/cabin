import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button, Loader } from "@mantine/core";
import { useCallback, useState } from "react";
import type { CheckStatus, SetupAction, SetupCheck } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { appIpc } from "../../services/ipc/app";
import { runOperation } from "../../lib/operations";
import { NodePanel } from "../../components/NodePanel";
import { SettingGroup, SettingRow, StateIcon, type StateTone } from "../../components/common";
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
      <SettingGroup title="Node.js">
        <NodePanel openSignal={0} onChanged={load} />
      </SettingGroup>
      <SettingGroup title={checks == null ? "System" : todo === 0 ? "System · everything is in place" : `System · ${todo} item${todo === 1 ? " needs" : "s need"} attention`}>
        {error && <SettingRow label={<span className="text-warn" role="status">{error}</span>} />}
        {checks == null && !error && <SettingRow label={<span className="inline-flex items-center gap-2 text-muted"><Loader size="xs" /> Checking the machine…</span>} />}
        {rows.map((check) => {
          const key = check.action ? JSON.stringify(check.action) : "";
          const showAction = check.action != null && !seen.has(key);
          if (check.action) seen.add(key);
          return (
            <SettingRow key={check.id} hint={check.detail} label={<span className="inline-flex items-center gap-2"><StateIcon tone={CHECK_TONE[check.status]} size={16} />{check.label}</span>}>
              {showAction && check.action ? (
                <Button size="xs" loading={busy === check.id} disabled={busy != null} onClick={() => void run(check)}>{actionLabel(check.action)}</Button>
              ) : undefined}
            </SettingRow>
          );
        })}
      </SettingGroup>
    </SettingsPage>
  );
}
