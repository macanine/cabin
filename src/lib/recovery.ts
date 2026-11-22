/**
 * What to tell the user when the machine or its network fails: a plain
 * title, a few tips in the order to try them, and the actions to offer.
 * Messages come from the backend; this only recognises the common kinds.
 */

export type RecoveryAction = "startNetwork" | "restart";

export interface Recovery {
  title: string;
  tips: string[];
  actions: RecoveryAction[];
}

/** The port in "127.0.0.1:12233 … in use", if the message names one. */
function portIn(message: string): string | null {
  return message.match(/127\.0\.0\.1:(\d{2,5})/)?.[1] ?? null;
}

export function recoveryFor(message: string): Recovery {
  const text = message.toLowerCase();
  if (/already in use|address in use|os error 48|eaddrinuse/.test(text)) {
    const port = portIn(message);
    return {
      title: "A port the machine needs is taken",
      tips: [
        "Another program holds it, often a second copy of Cabin or another VM.",
        port ? `Find it with: lsof -nP -iTCP:${port} -sTCP:LISTEN — and quit it.` : "Quit other copies of Cabin and other VMs.",
        "Then start the network again, or restart the machine.",
      ],
      actions: ["startNetwork", "restart"],
    };
  }
  if (/recheck timed out|could not confirm the verified public exit|tls handshake|certificate|name resolution|resolve host|no route to host/.test(text)) {
    return {
      title: "The network could not be verified",
      tips: ["Check that the Mac is online and the selected interface or proxy is reachable.", "In Network settings, run a fresh check on the saved route."],
      actions: ["startNetwork"],
    };
  }
  if (/could not be stopped|recorded machine process|already installed or has files to recover|no instance.json|reinstall with ubuntu|existing disk|disk was kept/.test(text)) {
    return {
      title: "The machine needs recovery",
      tips: ["Open Settings → Machine to review startup logs and the saved files.", "Use Verify or repair installation when available. Erase the machine only if you want a new disk."],
      actions: [],
    };
  }
  if (/gateway|shoes|tun/.test(text)) {
    return {
      title: "The machine's network is down",
      tips: [
        "Without its gateway the VM has no internet, and the app cannot reach it.",
        "Try connecting again. If it keeps failing, restart the machine.",
      ],
      actions: ["startNetwork", "restart"],
    };
  }
  if (/permission denied|publickey|host key/.test(text)) {
    return {
      title: "The machine refused the app's key",
      tips: [
        "The machine's SSH key no longer matches. Restarting usually fixes a half-finished start.",
        "If it persists, verify the installation under Settings → Machine.",
      ],
      actions: ["restart"],
    };
  }
  if (/cabind|connection refused|timed out|timeout|reset by peer|broken pipe|unreachable|cannot reach/.test(text)) {
    return {
      title: "The app cannot reach the machine",
      tips: [
        "The VM may still be booting; give it a minute.",
        "If nothing changes, restart the machine.",
        "Still stuck? Settings → Machine shows the last start, step by step.",
      ],
      actions: ["restart"],
    };
  }
  return {
    title: "Something went wrong with the machine",
    tips: ["Restarting the machine fixes most problems.", "Settings → Machine shows the last start, step by step."],
    actions: ["restart"],
  };
}
