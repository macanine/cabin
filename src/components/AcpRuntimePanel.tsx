import { Badge, Button, Checkbox, Loader, Select, Tooltip } from "@mantine/core";
import { IconArrowUp, IconCheck, IconDownload, IconTrash } from "@tabler/icons-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { AcpRuntime, AgentStatus } from "../services/ipc/generated/core";
import { agentsIpc } from "../services/ipc/agents";
import { toIpcError } from "../services/ipc/common";
import { attempt, runOperation } from "../lib/operations";
import { isPrerelease, offeredVersions, updateFor } from "../lib/runtimeVersions";
import { uiActions } from "../state/ui";

/** One line under the agent's name: which runtime its chats use. */
export function runtimeSummary(runtime: AcpRuntime | undefined): string | null {
  if (!runtime) return null;
  if (runtime.overridePath) return `ACP runtime overridden: ${runtime.overridePath}`;
  const name = runtime.package.split("/").pop() ?? runtime.package;
  const onDisk = runtime.installed.includes(runtime.selected);
  return `Chats run on ${name} ${runtime.selected}${onDisk ? "" : " (installs on first chat)"}`;
}

/**
 * The ACP runtime of one agent: the adapter package (or, for OpenCode, the
 * CLI itself), the version new chats use, the versions on disk, and any
 * published version to install. Running chats keep their version until they
 * restart.
 */
export function AcpRuntimePanel({ runtime, agent, onChange }: {
  runtime: AcpRuntime;
  agent: AgentStatus | undefined;
  onChange: () => Promise<void> | void;
}) {
  const [published, setPublished] = useState<string[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [prereleases, setPrereleases] = useState(isPrerelease(runtime.selected));
  const [choice, setChoice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setPublished(await agentsIpc.runtimeVersions(runtime.agent));
    } catch (cause) {
      setError(toIpcError(cause).message);
    } finally {
      setLoading(false);
    }
  }, [runtime.agent]);

  useEffect(() => { void load(); }, [load]);

  const update = published ? updateFor(runtime.selected, published) : null;
  const options = useMemo(
    () => offeredVersions(published ?? [], [...runtime.installed, runtime.defaultVersion, runtime.selected], prereleases),
    [published, runtime.installed, runtime.defaultVersion, runtime.selected, prereleases],
  );
  const target = choice ?? options[0] ?? null;

  async function install(version: string) {
    setBusy(`install:${version}`);
    await runOperation(agentsIpc.installRuntime(runtime.agent, version, true), `${runtime.package} ${version} is now used for new chats`);
    setBusy(null);
    setChoice(null);
    await onChange();
  }

  async function use(version: string) {
    setBusy(`use:${version}`);
    try {
      await agentsIpc.selectRuntime(runtime.agent, version);
      uiActions.toast(`New chats use ${version}`, "success");
    } catch (cause) {
      uiActions.toast(toIpcError(cause).message, "error");
    }
    setBusy(null);
    await onChange();
  }

  async function remove(version: string) {
    setBusy(`remove:${version}`);
    await attempt(agentsIpc.removeRuntime(runtime.agent, version));
    setBusy(null);
    await onChange();
  }

  const label = (version: string) => {
    const tags = [
      version === runtime.selected ? "in use" : null,
      version === runtime.defaultVersion ? "tested" : null,
      runtime.installed.includes(version) ? "on disk" : null,
      isPrerelease(version) ? "preview" : null,
    ].filter(Boolean);
    return tags.length ? `${version} — ${tags.join(", ")}` : version;
  };

  return (
    <div className="runtime-panel">
      <div className="runtime-head">
        <div className="grow">
          <div className="row" style={{ gap: 8 }}>
            <strong className="mono fs-13">{runtime.package}</strong>
            <Badge size="xs" variant="light" color="gray">{runtime.license}</Badge>
            {update && <Badge size="xs" variant="light" color="blue">Update {update}</Badge>}
          </div>
          <div className="muted fs-12">
            {runtime.usesAgentCli
              ? `Speaks ACP for the ${agent?.version ? `${agent.name} CLI ${agent.version}` : "agent CLI"} installed above; its bundled CLI is not installed.`
              : "The agent itself, run as an ACP server."}
            {" "}Running chats keep their version until they restart.
          </div>
        </div>
        {update && !runtime.overridePath && (
          <Button size="xs" leftSection={<IconArrowUp size={14} />} loading={busy === `install:${update}`} disabled={busy != null} onClick={() => void install(update)}>
            Update to {update}
          </Button>
        )}
      </div>

      {runtime.overridePath && (
        <div className="callout row fs-12">Overridden by an environment variable: <span className="mono truncate">{runtime.overridePath}</span></div>
      )}

      <div className="runtime-installed" role="list" aria-label="Versions on disk">
        {runtime.installed.length === 0 && (
          <div className="faint fs-12">Nothing installed yet. {runtime.selected} is installed when the first chat starts, or install it now.</div>
        )}
        {runtime.installed.map((version) => {
          const selected = version === runtime.selected;
          return (
            <div key={version} role="listitem" className={`runtime-version${selected ? " selected" : ""}`}>
              {selected ? <IconCheck size={14} className="runtime-check" /> : <span className="runtime-check" />}
              <span className="mono grow">{version}</span>
              {version === runtime.defaultVersion && <Badge size="xs" variant="outline" color="gray">tested</Badge>}
              {isPrerelease(version) && <Badge size="xs" variant="outline" color="yellow">preview</Badge>}
              {!selected && (
                <>
                  <Button size="compact-xs" variant="default" loading={busy === `use:${version}`} disabled={busy != null} onClick={() => void use(version)}>Use</Button>
                  <Tooltip label="Remove this version" openDelay={300}>
                    <Button size="compact-xs" variant="subtle" color="gray" aria-label={`Remove ${version}`} loading={busy === `remove:${version}`} disabled={busy != null} onClick={() => void remove(version)}>
                      <IconTrash size={13} />
                    </Button>
                  </Tooltip>
                </>
              )}
            </div>
          );
        })}
      </div>

      <div className="runtime-install row">
        <Select
          size="xs"
          className="grow"
          aria-label="Version to install"
          placeholder={loading ? "Loading versions…" : "Version"}
          data={options.map((version) => ({ value: version, label: label(version) }))}
          value={target}
          onChange={setChoice}
          searchable
          nothingFoundMessage="No such version"
          rightSection={loading ? <Loader size={12} /> : undefined}
          disabled={runtime.overridePath != null}
          comboboxProps={{ withinPortal: true }}
        />
        <Checkbox size="xs" label="Previews" checked={prereleases} onChange={(event) => setPrereleases(event.currentTarget.checked)} />
        <Button
          size="xs"
          variant="default"
          leftSection={<IconDownload size={14} />}
          disabled={!target || busy != null || runtime.overridePath != null || target === runtime.selected}
          loading={busy === `install:${target}`}
          onClick={() => target && (runtime.installed.includes(target) ? void use(target) : void install(target))}
        >
          {target && runtime.installed.includes(target) ? "Use" : "Install & use"}
        </Button>
      </div>
      {error && <div className="faint fs-12" role="status">Could not list published versions: {error}</div>}
    </div>
  );
}
