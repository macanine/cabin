import { ActionIcon, Badge, Button, Loader, Switch, TextInput, Tooltip } from "@mantine/core";
import {
  IconCheck,
  IconCopy,
  IconFolderPlus,
  IconRefresh,
  IconTerminal2,
  IconTrash,
  IconUnlink,
} from "@tabler/icons-react";
import { type ChangeEvent, type KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useState } from "react";
import { SettingGroup, SettingRow } from "../../components/common";
import { useI18n } from "../../i18n";
import { attempt } from "../../lib/operations";
import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { toIpcError } from "../../services/ipc/common";
import { chooseFolder, clipboard, confirmAction } from "../../services/ipc/dialogs";
import {
  hostCliIpc,
  type HostCliConfig,
  type HostCliStatus,
  type LauncherState,
  type NativeChannelState,
} from "../../services/ipc/hostCli";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { SettingsPage } from "../SettingsView";

function CopySnippet({ label, code, hint }: { label: string; code: string; hint?: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  async function copy() {
    await attempt(clipboard.write(code));
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1800);
  }

  return (
    <div className="px-4 py-3.5 [&+&]:border-t [&+&]:border-line/75">
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="min-w-0">
          <strong className="text-md font-medium text-fg">{label}</strong>
          {hint && <div className="text-xs text-muted">{hint}</div>}
        </div>
        <Button
          size="compact-xs"
          variant="default"
          leftSection={copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
          onClick={() => void copy()}
        >
          {copied ? t("common.copied") : t("common.copy")}
        </Button>
      </div>
      <pre className="selectable m-0 overflow-x-auto rounded-lg border border-line/70 bg-bg px-3 py-2.5 font-mono text-xs leading-relaxed text-fg">
        <code>{code}</code>
      </pre>
    </div>
  );
}

const LAUNCHER_COLOR: Record<LauncherState, string> = {
  installed: "green",
  notInstalled: "gray",
  conflict: "yellow",
  outdated: "yellow",
  unsupported: "gray",
};

const CHANNEL_COLOR: Record<NativeChannelState, string> = {
  ready: "green",
  needsRestart: "yellow",
  vmStopped: "gray",
  disabled: "gray",
  unsupported: "gray",
};

export function HostCliSettings() {
  const { t } = useI18n();
  const machine = useApp((state) => state.machine);
  const [status, setStatus] = useState<HostCliStatus | null>(null);
  const [dirDraft, setDirDraft] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await hostCliIpc.status();
      setStatus(next);
      setDirDraft(next.config.launcherDir ?? "");
      setError(null);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, []);

  useAutoRefresh(() => {
    if (!busy) return load();
  }, { interval: 15_000 });

  useEffect(() => {
    void load();
  }, [load, machine?.state, machine?.connection]);

  async function saveConfig(patch: Partial<HostCliConfig>) {
    if (!status) return;
    const next: HostCliConfig = { ...status.config, ...patch };
    if (JSON.stringify(next) === JSON.stringify(status.config)) return;
    setBusy("config");
    const updated = await attempt(hostCliIpc.saveConfig(next), t("common.saved"));
    setBusy(null);
    if (updated) {
      setStatus(updated);
      setDirDraft(updated.config.launcherDir ?? "");
    } else {
      setDirDraft(status.config.launcherDir ?? "");
    }
  }

  async function installLauncher(replace: boolean) {
    if (!status) return;
    if (replace) {
      const ok = await confirmAction(
        t("cli.confirmReplace", { path: status.launcherPath }),
        t("cli.action.replace"),
      );
      if (!ok) return;
    }
    setBusy("install");
    const updated = await attempt(hostCliIpc.installLauncher(replace), t("cli.toastInstalled"));
    setBusy(null);
    if (updated) setStatus(updated);
  }

  async function uninstallLauncher() {
    setBusy("uninstall");
    const updated = await attempt(hostCliIpc.uninstallLauncher(), t("cli.toastUninstalled"));
    setBusy(null);
    if (updated) setStatus(updated);
  }

  async function repairChannels() {
    setBusy("repair");
    const updated = await attempt(hostCliIpc.repairChannels(), t("cli.toastRepaired"));
    setBusy(null);
    if (updated) setStatus(updated);
  }

  async function bindFolder() {
    const folder = await chooseFolder();
    if (!folder) return;
    setBusy("bind");
    const updated = await attempt(hostCliIpc.bindFolder(folder), t("cli.toastBound"));
    setBusy(null);
    if (updated) setStatus(updated);
  }

  async function unbindFolder(id: string, name: string) {
    const ok = await confirmAction(
      t("cli.confirmUnbind", { name }),
      t("cli.action.unbind"),
    );
    if (!ok) return;
    setBusy(`unbind:${id}`);
    const updated = await attempt(hostCliIpc.unbindFolder(id), t("cli.toastUnbound"));
    setBusy(null);
    if (updated) setStatus(updated);
  }

  async function copyInline(value: string) {
    await attempt(clipboard.write(value), t("common.copied"));
  }

  function launcherLabel(state: LauncherState): string {
    switch (state) {
      case "installed": return t("cli.launcher.installed");
      case "notInstalled": return t("cli.launcher.notInstalled");
      case "conflict": return t("cli.launcher.conflict");
      case "outdated": return t("cli.launcher.outdated");
      case "unsupported": return t("cli.launcher.unsupported");
    }
  }

  function channelLabel(state: NativeChannelState): string {
    switch (state) {
      case "ready": return t("cli.channel.ready");
      case "needsRestart": return t("cli.channel.needsRestart");
      case "vmStopped": return t("cli.channel.vmStopped");
      case "disabled": return t("cli.channel.disabled");
      case "unsupported": return t("cli.channel.unsupported");
    }
  }

  function heroSummary(s: HostCliStatus): { tone: "ok" | "warn" | "muted"; badge: string; desc: string } {
    if (!s.supported) {
      return { tone: "muted", badge: t("cli.state.unsupported"), desc: t("cli.heroDescUnsupported") };
    }
    if (!s.config.enabled) {
      return { tone: "muted", badge: t("cli.state.disabled"), desc: t("cli.heroDescDisabled") };
    }
    if (s.activeProjectId) {
      return { tone: "ok", badge: t("cli.state.active"), desc: t("cli.heroDescActive") };
    }
    if (s.launcherState !== "installed") {
      return { tone: "warn", badge: t("cli.state.needsSetup"), desc: t("cli.heroDescNeedsLauncher") };
    }
    if (s.channelState === "needsRestart") {
      return { tone: "warn", badge: t("cli.state.needsRestart"), desc: t("cli.heroDescNeedsRestart") };
    }
    if (s.channelState === "vmStopped") {
      return { tone: "muted", badge: t("cli.state.stopped"), desc: t("cli.heroDescStopped") };
    }
    if (!s.inPath) {
      return {
        tone: "warn",
        badge: t("cli.state.needsSetup"),
        desc: t("cli.heroDescNotInPath", { dir: s.launcherDir, shell: s.shellName }),
      };
    }
    return { tone: "ok", badge: t("cli.state.ready"), desc: t("cli.heroDescReady") };
  }

  const hero = status ? heroSummary(status) : null;
  const disabled = !status || !status.supported || busy != null;
  const bindings = status?.bindings ?? [];

  return (
    <SettingsPage title={t("cli.title")} subtitle={t("cli.subtitle")}>
      {error && <p className="callout bad mt-0 mb-4" role="alert">{error}</p>}

      {!status && !error && (
        <div className="mb-4 flex items-center gap-2 rounded-[var(--card-radius)] border border-line bg-surface p-5 text-sm text-muted">
          <Loader size="xs" />
          <span>{t("cli.loading")}</span>
        </div>
      )}

      {status && hero && (
        <>
          <section className="mb-4 rounded-[var(--card-radius)] border border-line bg-surface p-5" aria-label={t("cli.heroTitle")}>
            <div className="flex flex-wrap items-center gap-4">
              <span className={`relative grid size-11 shrink-0 place-items-center rounded-2xl ${hero.tone === "ok" ? "bg-ok/13 text-ok" : hero.tone === "warn" ? "bg-warn/14 text-warn" : "bg-hover text-muted"}`}>
                <IconTerminal2 size={21} />
                <span className={`absolute -top-0.5 -right-0.5 size-2.5 rounded-full border-2 border-surface ${hero.tone === "ok" ? "bg-ok" : hero.tone === "warn" ? "bg-warn" : "bg-faint"}${status.activeProjectId ? " live-dot" : ""}`} />
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <strong className="text-[15px] font-semibold text-fg">{t("cli.heroTitle")}</strong>
                  <Badge size="xs" variant="light" color={hero.tone === "ok" ? "green" : hero.tone === "warn" ? "yellow" : "gray"}>
                    {hero.badge}
                  </Badge>
                </div>
                <div className="mt-0.5 text-xs text-muted">{hero.desc}</div>
              </div>
              {status.supported && status.config.enabled && (
                status.launcherState === "notInstalled" || status.launcherState === "outdated" ? (
                  <Button size="xs" loading={busy === "install"} disabled={busy != null} onClick={() => void installLauncher(false)}>
                    {status.launcherState === "outdated" ? t("cli.action.repair") : t("cli.action.install")}
                  </Button>
                ) : status.launcherState === "conflict" ? (
                  <Button size="xs" color="yellow" variant="light" loading={busy === "install"} disabled={busy != null} onClick={() => void installLauncher(true)}>
                    {t("cli.action.replace")}
                  </Button>
                ) : status.channelState === "vmStopped" || status.channelState === "needsRestart" ? (
                  <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>
                    {t("cli.action.openMachine")}
                  </Button>
                ) : status.channelState === "ready" ? (
                  <Button
                    size="xs"
                    variant="default"
                    leftSection={<IconRefresh size={13} />}
                    loading={busy === "repair"}
                    disabled={busy != null}
                    onClick={() => void repairChannels()}
                  >
                    {t("cli.action.restartChannels")}
                  </Button>
                ) : null
              )}
            </div>
          </section>

          <SettingGroup title={t("cli.launcherGroup")}>
            <SettingRow label={t("cli.enabled")} hint={t("cli.enabledHint")}>
              <Switch
                size="sm"
                aria-label={t("cli.enabled")}
                checked={status.config.enabled}
                disabled={disabled}
                onChange={(event) => void saveConfig({ enabled: event.currentTarget.checked })}
              />
            </SettingRow>

            <SettingRow
              label={t("cli.commandPath")}
              hint={status.launcherDetail ?? t("cli.commandPathHint")}
            >
              <div className="flex flex-wrap items-center justify-end gap-1.5">
                <Badge size="xs" variant="light" color={LAUNCHER_COLOR[status.launcherState]}>
                  {launcherLabel(status.launcherState)}
                </Badge>
                <code className="selectable max-w-[260px] truncate rounded-md bg-hover px-2 py-1 font-mono text-xs text-fg" title={status.launcherPath}>
                  {status.launcherPath}
                </code>
                <Tooltip label={t("common.copy")}>
                  <ActionIcon size="sm" variant="subtle" color="gray" aria-label={t("common.copy")} onClick={() => void copyInline(status.launcherPath)}>
                    <IconCopy size={14} />
                  </ActionIcon>
                </Tooltip>
                {status.supported && (status.launcherState === "installed" || status.launcherState === "outdated") && (
                  <Tooltip label={t("cli.action.uninstall")}>
                    <ActionIcon
                      size="sm"
                      variant="subtle"
                      color="red"
                      aria-label={t("cli.action.uninstall")}
                      loading={busy === "uninstall"}
                      disabled={busy != null}
                      onClick={() => void uninstallLauncher()}
                    >
                      <IconTrash size={14} />
                    </ActionIcon>
                  </Tooltip>
                )}
              </div>
            </SettingRow>

            <SettingRow label={t("cli.launcherDir")} hint={t("cli.launcherDirHint")}>
              <TextInput
                size="xs"
                w={220}
                aria-label={t("cli.launcherDir")}
                placeholder="~/.local/bin"
                disabled={disabled}
                value={dirDraft}
                onChange={(event: ChangeEvent<HTMLInputElement>) => setDirDraft(event.currentTarget.value)}
                onBlur={() => void saveConfig({ launcherDir: dirDraft.trim() || null })}
                onKeyDown={(event: ReactKeyboardEvent<HTMLInputElement>) => {
                  if (event.key === "Enter") event.currentTarget.blur();
                }}
              />
            </SettingRow>

            {status.supported && (
              <SettingRow
                label={t("cli.shellPath", { shell: status.shellName })}
                hint={status.inPath
                  ? t("cli.shellPathHintOk", { dir: status.launcherDir, shell: status.shellName })
                  : t("cli.shellPathHintMissing", { dir: status.launcherDir, shell: status.shellName })}
              >
                <Badge size="xs" variant="light" color={status.inPath ? "green" : "yellow"}>
                  {status.inPath ? t("cli.inPath") : t("cli.notInPath")}
                </Badge>
              </SettingRow>
            )}

            {status.supported && !status.inPath && status.pathSnippet && (
              <CopySnippet
                label={t("cli.addToPathSnippet", { shell: status.shellName })}
                hint={t("cli.addToPathHint", { path: status.launcherPath })}
                code={status.pathSnippet}
              />
            )}

            <SettingRow label={t("cli.channels")} hint={t("cli.channelsHint")}>
              <Badge size="xs" variant="light" color={CHANNEL_COLOR[status.channelState]}>
                {channelLabel(status.channelState)}
              </Badge>
            </SettingRow>
          </SettingGroup>

          <SettingGroup title={t("cli.optionsGroup")}>
            <SettingRow label={t("cli.allowShell")} hint={t("cli.allowShellHint")}>
              <Switch
                size="sm"
                aria-label={t("cli.allowShell")}
                checked={status.config.allowShell}
                disabled={disabled || !status.config.enabled}
                onChange={(event) => void saveConfig({ allowShell: event.currentTarget.checked })}
              />
            </SettingRow>
            <SettingRow label={t("cli.autoBindPrompt")} hint={t("cli.autoBindPromptHint")}>
              <Switch
                size="sm"
                aria-label={t("cli.autoBindPrompt")}
                checked={status.config.autoBindPrompt}
                disabled={disabled || !status.config.enabled}
                onChange={(event) => void saveConfig({ autoBindPrompt: event.currentTarget.checked })}
              />
            </SettingRow>
            <SettingRow label={t("cli.forwardEnv")} hint={t("cli.forwardEnvHint")}>
              <Switch
                size="sm"
                aria-label={t("cli.forwardEnv")}
                checked={status.config.forwardTerminalEnv}
                disabled={disabled || !status.config.enabled}
                onChange={(event) => void saveConfig({ forwardTerminalEnv: event.currentTarget.checked })}
              />
            </SettingRow>
          </SettingGroup>

          <SettingGroup title={t("cli.bindingsGroup")}>
            {status.supported && (
              <SettingRow
                label={bindings.length === 0 ? t("cli.bindingsEmpty") : t("cli.bindingsCount", { count: bindings.length })}
                hint={bindings.length === 0 ? t("cli.bindingsEmptyHint") : t("cli.bindingsCountHint")}
              >
                <Button
                  size="xs"
                  variant="default"
                  leftSection={<IconFolderPlus size={14} />}
                  loading={busy === "bind"}
                  disabled={disabled}
                  onClick={() => void bindFolder()}
                >
                  {t("cli.action.bindFolder")}
                </Button>
              </SettingRow>
            )}
            {bindings.map((binding) => (
              <div key={binding.id} className="set-row">
                <div className="set-row-copy min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <strong>{binding.name}</strong>
                    <Badge size="xs" variant="light" color={binding.mode === "twoWay" ? "blue" : "gray"}>
                      {binding.mode === "twoWay"
                        ? t("cli.bindingModeTwoWay")
                        : t("cli.bindingModeOther", { mode: binding.mode })}
                    </Badge>
                    {binding.activeSession && (
                      <Badge size="xs" variant="light" color="green">
                        {t("cli.bindingActive")}
                      </Badge>
                    )}
                    {binding.conflicts > 0 && (
                      <Badge size="xs" variant="light" color="yellow">
                        {t("cli.bindingConflicts", { count: binding.conflicts })}
                      </Badge>
                    )}
                  </div>
                  <span className="font-mono text-xs [overflow-wrap:anywhere]">
                    {binding.hostPath} ↔ VM: {binding.guestPath}
                  </span>
                </div>
                <Button
                  size="compact-xs"
                  variant="subtle"
                  color="gray"
                  leftSection={<IconUnlink size={13} />}
                  loading={busy === `unbind:${binding.id}`}
                  disabled={busy != null || binding.activeSession}
                  onClick={() => void unbindFolder(binding.id, binding.name)}
                >
                  {t("cli.action.unbind")}
                </Button>
              </div>
            ))}
          </SettingGroup>

          <SettingGroup title={t("cli.usageGroup")}>
            <CopySnippet
              label={t("cli.usageAgents")}
              hint={t("cli.usageAgentsHint")}
              code={"cabin claude\ncabin codex\ncabin agy\ncabin opencode\ncabin pi"}
            />
            {status.config.allowShell && (
              <CopySnippet
                label={t("cli.usageShell")}
                hint={t("cli.usageShellHint")}
                code={"cabin shell\ncabin exec git status"}
              />
            )}
            <CopySnippet
              label={t("cli.usageManage")}
              hint={t("cli.usageManageHint")}
              code={"cabin status\ncabin list\ncabin bind"}
            />
          </SettingGroup>
        </>
      )}
    </SettingsPage>
  );
}
