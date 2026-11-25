import { SegmentedControl, TextInput } from "@mantine/core";
import { type ChangeEvent, type KeyboardEvent as ReactKeyboardEvent, useEffect, useState } from "react";
import type { AgentKind, AppSettings } from "../../services/ipc/generated/core";
import { appIpc } from "../../services/ipc/app";
import { patchState, useApp } from "../../state/store";
import { attempt } from "../../lib/operations";
import { SettingGroup, SettingRow } from "../../components/common";
import { AgentSelect } from "../../components/AgentPicker";
import { SettingsPage } from "../SettingsView";
import { type QuitPref, readQuitPref, writeQuitPref } from "../../lib/quitPref";
import { type PerfPref, effectivePerf, readPerf, setPerf } from "../../lib/perfMode";

/** Each field saves on its own (select: on change, text: on blur or Enter);
 * a rejected value goes back to the saved one. */
export function GeneralSettings() {
  const settings = useApp((state) => state.settings);
  const agents = useApp((state) => state.agents);
  const dataDir = useApp((state) => state.dataDir);
  const [draft, setDraft] = useState<AppSettings | null>(settings);
  const [perf, setPerfState] = useState<PerfPref>(readPerf);
  const [quit, setQuit] = useState<QuitPref>(readQuitPref);

  useEffect(() => setDraft(settings), [settings]);
  if (!draft || !settings) return <div className="page" />;

  async function save(patch: Partial<AppSettings>) {
    if (!settings) return;
    const next = { ...settings, ...patch };
    if (JSON.stringify(next) === JSON.stringify(settings)) return;
    const saved = await attempt(appIpc.updateSettings(next), "Saved");
    if (saved) patchState((state) => ({ ...state, settings: saved }));
    else setDraft(settings);
  }

  const text = (key: "latencyTarget" | "checkTarget") => ({
    value: draft[key],
    onChange: (event: ChangeEvent<HTMLInputElement>) => setDraft({ ...draft, [key]: event.currentTarget.value }),
    onBlur: () => void save({ [key]: draft[key].trim() }),
    onKeyDown: (event: ReactKeyboardEvent<HTMLInputElement>) => {
      if (event.key === "Enter") event.currentTarget.blur();
    },
  });

  const installed = (kind: AgentKind) => agents.find((agent) => agent.kind === kind)?.installed ?? false;

  return (
    <SettingsPage title="General" subtitle="Defaults, effects and the targets the app measures against.">
      <SettingGroup title="Tasks">
        <SettingRow label="Default agent" hint="Selected first when you start a new task.">
          <AgentSelect value={draft.defaultAgent} installed={installed} onChange={(kind) => void save({ defaultAgent: kind })} width={220} />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title="App">
        <SettingRow label="When quitting" hint="The close button only hides the window; the app and machine keep running in the menu bar. Quit (⌘Q) uses this choice.">
          <SegmentedControl
            size="xs" aria-label="When quitting" value={quit}
            onChange={(value) => { setQuit(value as QuitPref); writeQuitPref(value as QuitPref); }}
            data={[{ value: "ask", label: "Ask" }, { value: "stop", label: "Stop the machine" }, { value: "background", label: "Keep running" }]}
          />
        </SettingRow>
        <SettingRow label="Effects" hint={perf === "auto" ? `Follows the system's reduced motion and transparency settings (now: ${effectivePerf("auto") === "lite" ? "lite" : "full"}).` : perf === "lite" ? "No blur, animation or window transparency; solid surfaces." : "Glass, blur, animation and window transparency."}>
          <SegmentedControl
            size="xs" aria-label="Effects" value={perf}
            onChange={(value) => { setPerfState(value as PerfPref); setPerf(value as PerfPref); }}
            data={[{ value: "auto", label: "Auto" }, { value: "full", label: "Full effects" }, { value: "lite", label: "Lite" }]}
          />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title="Network targets">
        <SettingRow label="Latency target" hint="The performance monitor connects here (host:port, plain TCP).">
          <TextInput size="xs" w={220} aria-label="Latency target" {...text("latencyTarget")} />
        </SettingRow>
        <SettingRow label="Connectivity check" hint="Used by “Check network” and proxy node tests.">
          <TextInput size="xs" w={220} aria-label="Connectivity check" {...text("checkTarget")} />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title="Data">
        <SettingRow label="Data folder" hint="Workspaces, the operation journal, backups and environment.sh live here, inside the machine. Set CABIN_HOME to move it.">
          <code className="selectable block rounded-lg bg-hover px-2.5 py-1.5 font-mono text-xs text-muted [overflow-wrap:anywhere]">{dataDir}</code>
        </SettingRow>
      </SettingGroup>
    </SettingsPage>
  );
}
