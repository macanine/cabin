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
import { type LocalePref, useI18n } from "../../i18n";

/** Each field saves on its own (select: on change, text: on blur or Enter);
 * a rejected value goes back to the saved one. */
export function GeneralSettings() {
  const { t, pref: localePref, setLocalePref } = useI18n();
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
    const saved = await attempt(appIpc.updateSettings(next), t("common.saved"));
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
  const activeModeLabel = effectivePerf("auto") === "lite" ? t("settings.general.effectsLite") : t("settings.general.effectsFull");

  return (
    <SettingsPage title={t("settings.tab.general")} subtitle={t("settings.general.subtitle")}>
      <SettingGroup title={t("settings.general.languageGroup")}>
        <SettingRow label={t("settings.general.language")} hint={t("settings.general.languageHint")}>
          <SegmentedControl
            size="xs"
            aria-label={t("settings.general.language")}
            value={localePref}
            onChange={(value) => setLocalePref(value as LocalePref)}
            data={[
              { value: "auto", label: t("settings.general.languageAuto") },
              { value: "en", label: "English" },
              { value: "zh-CN", label: "简体中文" },
            ]}
          />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title={t("settings.general.tasksGroup")}>
        <SettingRow label={t("settings.general.defaultAgent")} hint={t("settings.general.defaultAgentHint")}>
          <AgentSelect value={draft.defaultAgent} installed={installed} onChange={(kind) => void save({ defaultAgent: kind })} width={220} />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title={t("settings.general.appGroup")}>
        <SettingRow label={t("settings.general.whenQuitting")} hint={t("settings.general.whenQuittingHint")}>
          <SegmentedControl
            size="xs" aria-label={t("settings.general.whenQuitting")} value={quit}
            onChange={(value) => { setQuit(value as QuitPref); writeQuitPref(value as QuitPref); }}
            data={[
              { value: "ask", label: t("settings.general.quitAsk") },
              { value: "stop", label: t("settings.general.quitStop") },
              { value: "background", label: t("settings.general.quitBackground") },
            ]}
          />
        </SettingRow>
        <SettingRow
          label={t("settings.general.effects")}
          hint={perf === "auto"
            ? t("settings.general.effectsHintAuto", { mode: activeModeLabel })
            : perf === "lite"
              ? t("settings.general.effectsHintLite")
              : t("settings.general.effectsHintFull")}
        >
          <SegmentedControl
            size="xs" aria-label={t("settings.general.effects")} value={perf}
            onChange={(value) => { setPerfState(value as PerfPref); setPerf(value as PerfPref); }}
            data={[
              { value: "auto", label: t("common.auto") },
              { value: "full", label: t("settings.general.effectsFull") },
              { value: "lite", label: t("settings.general.effectsLite") },
            ]}
          />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title={t("settings.general.targetsGroup")}>
        <SettingRow label={t("settings.general.latencyTarget")} hint={t("settings.general.latencyTargetHint")}>
          <TextInput size="xs" w={220} aria-label={t("settings.general.latencyTarget")} {...text("latencyTarget")} />
        </SettingRow>
        <SettingRow label={t("settings.general.checkTarget")} hint={t("settings.general.checkTargetHint")}>
          <TextInput size="xs" w={220} aria-label={t("settings.general.checkTarget")} {...text("checkTarget")} />
        </SettingRow>
      </SettingGroup>
      <SettingGroup title={t("settings.general.dataGroup")}>
        <SettingRow label={t("settings.general.dataFolder")} hint={t("settings.general.dataFolderHint")}>
          <code className="selectable block rounded-lg bg-hover px-2.5 py-1.5 font-mono text-xs text-muted [overflow-wrap:anywhere]">{dataDir}</code>
        </SettingRow>
      </SettingGroup>
    </SettingsPage>
  );
}
