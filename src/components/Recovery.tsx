import { Button } from "@mantine/core";
import { IconAlertTriangle, IconPlugConnected, IconRefresh } from "@tabler/icons-react";
import { useState } from "react";
import { machineIpc } from "../services/ipc/app";
import { networkIpc } from "../services/ipc/network";
import { useApp } from "../state/store";
import { attempt } from "../lib/operations";
import { recoveryFor } from "../lib/recovery";

/** Stop, then start: the machine keeps its disk; sessions resume after. */
export async function restartMachine(): Promise<boolean> {
  if (!(await attempt(machineIpc.stop()))) return false;
  return (await attempt(machineIpc.start(), "Machine restarted")) != null;
}

/** A failure explained: what happened, what to try, and buttons for it. */
export function RecoveryPanel({ message, compact = false, retry = true }: { message: string; compact?: boolean; retry?: boolean }) {
  const busy = useApp((state) => state.machine?.busy != null);
  const [running, setRunning] = useState<"startNetwork" | "restart" | null>(null);
  const recovery = recoveryFor(message);

  async function run(action: "startNetwork" | "restart") {
    setRunning(action);
    if (action === "restart") await restartMachine();
    else await attempt(networkIpc.verify());
    setRunning(null);
  }

  return (
    <div className={`recovery${compact ? " compact" : ""}`} role="alert">
      <div className="recovery-head">
        <IconAlertTriangle size={16} />
        <strong>{recovery.title}</strong>
      </div>
      <div className="recovery-message mono">{message}</div>
      <ol className="recovery-tips">
        {recovery.tips.map((tip) => <li key={tip}>{tip}</li>)}
      </ol>
      {retry && recovery.actions.length > 0 && <div className="recovery-actions">
        {recovery.actions.includes("startNetwork") && (
          <Button size="xs" variant="default" leftSection={<IconPlugConnected size={14} />} loading={running === "startNetwork"} disabled={busy || running != null} onClick={() => void run("startNetwork")}>
            Check saved route
          </Button>
        )}
        {recovery.actions.includes("restart") && (
          <Button size="xs" variant={recovery.actions.length === 1 ? "filled" : "default"} leftSection={<IconRefresh size={14} />} loading={running === "restart"} disabled={busy || running != null} onClick={() => void run("restart")}>
            Restart machine
          </Button>
        )}
      </div>}
    </div>
  );
}
