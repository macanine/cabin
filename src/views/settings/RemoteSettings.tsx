import { ActionIcon, Badge, Button, TextInput, Tooltip } from "@mantine/core";
import {
  IconCheck, IconCopy, IconKey, IconNetwork, IconPlugConnected, IconServer, IconShieldLock, IconTerminal2,
} from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { SettingGroup, SettingRow } from "../../components/common";
import { attempt } from "../../lib/operations";
import { machineIpc } from "../../services/ipc/app";
import { clipboard } from "../../services/ipc/dialogs";
import type { MachineDetails } from "../../services/ipc/generated/common";
import { useApp } from "../../state/store";
import { uiActions } from "../../state/ui";
import { SettingsPage } from "../SettingsView";
import { useI18n } from "../../i18n";

const SSH_ALIAS_KEY = "cabin.remote.sshAlias";

function readAlias(): string {
  try {
    return localStorage.getItem(SSH_ALIAS_KEY) || "cabin";
  } catch {
    return "cabin";
  }
}

function joinHostPath(base: string, leaf: string): string {
  const sep = base.includes("\\") ? "\\" : "/";
  return `${base.replace(/[\\/]+$/, "")}${sep}${leaf}`;
}

function parseEndpoint(endpoint: string | null | undefined, fallbackPort: number | null | undefined): { host: string; port: number } {
  if (endpoint) {
    const idx = endpoint.lastIndexOf(":");
    if (idx > 0) {
      const host = endpoint.slice(0, idx);
      const port = Number(endpoint.slice(idx + 1));
      if (host && Number.isInteger(port) && port > 0) return { host, port };
    }
  }
  return { host: "127.0.0.1", port: fallbackPort ?? 2222 };
}

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

export function RemoteSettings() {
  const { t } = useI18n();
  const machine = useApp((state) => state.machine);
  const [details, setDetails] = useState<MachineDetails | null>(null);
  const [alias, setAlias] = useState<string>(readAlias);

  const installed = machine != null && machine.state !== "notInstalled";
  const running = machine?.state === "running";
  const connected = running && machine?.connection === "connected";

  useEffect(() => {
    if (!installed) {
      setDetails(null);
      return;
    }
    let active = true;
    void machineIpc.details().then(
      (value) => { if (active) setDetails(value); },
      () => { if (active) setDetails(null); },
    );
    return () => { active = false; };
  }, [installed, machine?.state, machine?.connection, machine?.sshPort]);

  const user = machine?.username ?? "dev";
  const { host, port } = parseEndpoint(details?.sshEndpoint, machine?.sshPort);
  const keyPath = details?.instancePath ? joinHostPath(details.instancePath, "id_ed25519") : "~/.cabin/instances/dev/id_ed25519";
  const safeAlias = alias.trim().replace(/\s+/g, "-") || "cabin";

  const sshCommand = `ssh -i "${keyPath}" -p ${port} ${user}@${host}`;
  const sshConfig = [
    `Host ${safeAlias}`,
    `  HostName ${host}`,
    `  Port ${port}`,
    `  User ${user}`,
    `  IdentityFile "${keyPath}"`,
    `  IdentitiesOnly yes`,
    `  StrictHostKeyChecking accept-new`,
  ].join("\n");

  function saveAlias(next: string) {
    setAlias(next);
    try {
      localStorage.setItem(SSH_ALIAS_KEY, next.trim() || "cabin");
    } catch {
      // Ignore storage failures.
    }
  }

  async function copyInline(value: string) {
    await attempt(clipboard.write(value), t("common.copied"));
  }

  return (
    <SettingsPage title={t("remote.title")} subtitle={t("remote.subtitle")}>
      <section className="mb-4 rounded-[var(--card-radius)] border border-line bg-surface p-5" aria-label={t("remote.sshRelay")}>
        <div className="flex flex-wrap items-center gap-4">
          <span className={`relative grid size-11 shrink-0 place-items-center rounded-2xl ${connected ? "bg-ok/13 text-ok" : running ? "bg-warn/14 text-warn" : "bg-hover text-muted"}`}>
            <IconTerminal2 size={21} />
            <span className={`absolute -top-0.5 -right-0.5 size-2.5 rounded-full border-2 border-surface ${connected ? "bg-ok" : running ? "bg-warn" : "bg-faint"}`} />
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <strong className="text-[15px] font-semibold text-fg">{t("remote.sshRelay")}</strong>
              <Badge size="xs" variant="light" color={connected ? "green" : running ? "yellow" : "gray"}>
                {connected ? t("remote.listening") : running ? t("remote.starting") : installed ? t("remote.stopped") : t("remote.notInstalled")}
              </Badge>
            </div>
            <div className="mt-0.5 text-xs text-muted">
              {connected
                ? t("remote.listeningDesc", { host, port })
                : t("remote.stoppedDesc")}
            </div>
          </div>
          {!running && installed && (
            <Button size="xs" variant="default" onClick={() => uiActions.navigate({ kind: "settings", tab: "machine" })}>
              {t("remote.openMachine")}
            </Button>
          )}
        </div>
      </section>

      <SettingGroup title={t("remote.sshEndpointGroup")}>
        <SettingRow label={t("remote.endpoint")} hint={t("remote.endpointHint")}>
          <div className="flex items-center gap-1.5">
            <code className="selectable rounded-md bg-hover px-2 py-1 font-mono text-xs text-fg">{host}:{port}</code>
            <Tooltip label={t("common.copy")}>
              <ActionIcon size="sm" variant="subtle" color="gray" aria-label={t("common.copy")} onClick={() => void copyInline(`${host}:${port}`)}>
                <IconCopy size={14} />
              </ActionIcon>
            </Tooltip>
          </div>
        </SettingRow>
        <SettingRow label={t("remote.username")} hint={t("remote.usernameHint")}>
          <div className="flex items-center gap-1.5">
            <code className="selectable rounded-md bg-hover px-2 py-1 font-mono text-xs text-fg">{user}</code>
            <Tooltip label={t("common.copy")}>
              <ActionIcon size="sm" variant="subtle" color="gray" aria-label={t("common.copy")} onClick={() => void copyInline(user)}>
                <IconCopy size={14} />
              </ActionIcon>
            </Tooltip>
          </div>
        </SettingRow>
        <SettingRow label={t("remote.identityKey")} hint={t("remote.identityKeyHint")}>
          <div className="flex max-w-[360px] items-center gap-1.5">
            <code className="selectable truncate rounded-md bg-hover px-2 py-1 font-mono text-xs text-muted" title={keyPath}>{keyPath}</code>
            <Tooltip label={t("common.copy")}>
              <ActionIcon size="sm" variant="subtle" color="gray" aria-label={t("common.copy")} onClick={() => void copyInline(keyPath)}>
                <IconCopy size={14} />
              </ActionIcon>
            </Tooltip>
          </div>
        </SettingRow>
        <SettingRow label={t("remote.hostAlias")} hint={t("remote.hostAliasHint")}>
          <TextInput
            size="xs"
            w={180}
            value={alias}
            aria-label={t("remote.hostAlias")}
            placeholder="cabin"
            onChange={(event) => saveAlias(event.currentTarget.value)}
          />
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={t("remote.quickConnect")}>
        <CopySnippet
          label={t("remote.terminalCmd")}
          hint={t("remote.terminalCmdHint")}
          code={sshCommand}
        />
        <CopySnippet
          label={t("remote.sshConfigEntry")}
          hint={t("remote.sshConfigHint", { alias: safeAlias })}
          code={sshConfig}
        />
      </SettingGroup>

      <SettingGroup title={t("remote.privateNetworking")}>
        <SettingRow
          label={<span className="inline-flex items-center gap-2"><IconShieldLock size={15} className="text-muted" /> {t("remote.wireguard")}</span>}
          hint={t("remote.wireguardHint")}
        >
          <Badge size="xs" variant="light" color="gray">{t("common.planned")}</Badge>
        </SettingRow>
        <SettingRow
          label={<span className="inline-flex items-center gap-2"><IconNetwork size={15} className="text-muted" /> {t("remote.meshVpn")}</span>}
          hint={t("remote.meshVpnHint")}
        >
          <Badge size="xs" variant="light" color="gray">{t("common.planned")}</Badge>
        </SettingRow>
        <SettingRow
          label={<span className="inline-flex items-center gap-2"><IconPlugConnected size={15} className="text-muted" /> {t("remote.lanForward")}</span>}
          hint={t("remote.lanForwardHint")}
        >
          <Badge size="xs" variant="light" color="gray">{t("common.planned")}</Badge>
        </SettingRow>
        <SettingRow
          label={<span className="inline-flex items-center gap-2"><IconKey size={15} className="text-muted" /> {t("remote.authKeys")}</span>}
          hint={t("remote.authKeysHint")}
        >
          <Badge size="xs" variant="light" color="gray">{t("common.planned")}</Badge>
        </SettingRow>
      </SettingGroup>

      {details?.networkMode && (
        <div className="mt-2 flex items-center gap-2 text-xs text-faint">
          <IconServer size={13} />
          <span>{t("remote.vmNetworkMode")}: <span className="font-mono">{details.networkMode}</span></span>
        </div>
      )}
    </SettingsPage>
  );
}
