import { ActionIcon, Alert, Badge, Button, Drawer, Loader, Menu, Modal, SegmentedControl, Tooltip } from "@mantine/core";
import {
  IconCheck, IconCopy, IconDots, IconDownload, IconFileText, IconPlug, IconPlus,
  IconRefresh, IconSparkles, IconTerminal2, IconTrash, IconWorld, IconZip,
} from "@tabler/icons-react";
import { useState } from "react";
import { AGENT_KINDS } from "../components/AgentPicker";
import { AgentLogo, AGENT_NAMES } from "../components/common";
import { Markdown } from "../components/chat/Markdown";
import { ExtensionSearch, ExtensionStatus, useExtensionLibrary } from "../components/extensions/LibraryParts";
import { McpLibraryEditor } from "../components/extensions/McpLibraryEditor";
import { SkillDiscover } from "../components/extensions/SkillDiscover";
import { InstallSkillDialog, SkillBackupsDialog, SkillRepositoryDialog, type SkillInstallDialog } from "../components/extensions/SkillDialogs";
import { ViewHeader } from "../components/TitleBar";
import { attempt } from "../lib/operations";
import { agentsIpc } from "../services/ipc/agents";
import { clipboard, confirmAction } from "../services/ipc/dialogs";
import { mcpIpc, skillsIpc } from "../services/ipc/extensions";
import type {
  AgentKind, LocalSkill, McpChange, McpRow, McpServer, SkillChange, SkillFile, SkillInstall, SkillMode,
} from "../services/ipc/generated/core";
import { useApp } from "../state/store";
import { useI18n } from "../i18n";

export type ExtensionTab = "mcp" | "skills" | "discover";

function mcpAvailable(row: McpRow, kind: AgentKind, blocked: AgentKind[]) {
  const binding = row.bindings.find((entry) => entry.kind === kind);
  return Boolean(
    row.managed && !row.conflict && !blocked.includes(kind)
    && (binding?.state === "enabled" || binding?.state === "disabled"),
  );
}

function AgentScopeChip({
  kind, active, drifted, unavailable, disabled, tooltip, onToggle,
}: {
  kind: AgentKind;
  active: boolean;
  drifted?: boolean;
  unavailable?: string;
  disabled: boolean;
  tooltip: string;
  onToggle: (next: boolean) => void;
}) {
  return (
    <Tooltip label={tooltip} openDelay={220}>
      <button
        type="button"
        className={`ext-scope-chip press${active ? " active" : ""}${unavailable ? " unavailable" : ""}`}
        aria-pressed={active}
        disabled={disabled || Boolean(unavailable)}
        onClick={() => onToggle(!active)}
      >
        <AgentLogo agent={kind} size={13} />
        <span>{AGENT_NAMES[kind]}</span>
        {active && <IconCheck size={11} stroke={2.8} className="ext-scope-check" aria-hidden />}
        {drifted && !disabled && <span className="ext-scope-drift" aria-hidden />}
      </button>
    </Tooltip>
  );
}

function FilterPills({
  agent, onAgent, total, count, extra,
}: {
  agent: AgentKind | null;
  onAgent: (kind: AgentKind | null) => void;
  total: number;
  count: (kind: AgentKind) => number;
  extra?: React.ReactNode;
}) {
  const { t } = useI18n();
  return (
    <div className="ext-filter-pills" role="group" aria-label="Filter by agent">
      <button type="button" className={`ext-filter-pill press${agent == null ? " active" : ""}`} onClick={() => onAgent(null)}>
        <span>{t("common.all")}</span>
        <span className="ext-filter-count">{total}</span>
      </button>
      {AGENT_KINDS.map((kind) => {
        const n = count(kind);
        return (
          <button
            key={kind}
            type="button"
            className={`ext-filter-pill press${agent === kind ? " active" : ""}`}
            onClick={() => onAgent(agent === kind ? null : kind)}
          >
            <AgentLogo agent={kind} size={13} />
            <span>{AGENT_NAMES[kind]}</span>
            <span className="ext-filter-count">{n}</span>
          </button>
        );
      })}
      {extra}
    </div>
  );
}

export function ExtensionsView({ initialTab = "mcp", agent: initialAgent }: { initialTab?: ExtensionTab; agent?: AgentKind }) {
  const { t } = useI18n();
  const [tab, setTab] = useState<ExtensionTab>(initialTab);
  const [agent, setAgent] = useState<AgentKind | null>(initialAgent ?? null);
  const [query, setQuery] = useState("");

  // MCP state
  const mcpLib = useExtensionLibrary(mcpIpc.list);
  const [writeError, setWriteError] = useState<{ title: string; message: string } | null>(null);
  const [editor, setEditor] = useState<{ server: McpServer | null; opened: boolean; revision: number } | null>(null);
  const pi = useApp((state) => state.agents.find((entry) => entry.kind === "pi"));
  const legacyPi = pi?.installed && (pi.version?.match(/\b\d+\./)?.[0] === "0." || (!pi.version && pi.package === "@mariozechner/pi-coding-agent"));

  // Skills state
  const skillsLib = useExtensionLibrary(skillsIpc.list);
  const [updatesOnly, setUpdatesOnly] = useState(false);
  const [checking, setChecking] = useState(false);
  const [updates, setUpdates] = useState<string[] | null>(null);
  const [dialog, setDialog] = useState<SkillInstallDialog | null>(null);
  const [manageRepos, setManageRepos] = useState(false);
  const [backups, setBackups] = useState(false);
  const [detail, setDetail] = useState<SkillFile | null>(null);

  const mcpData = mcpLib.data;
  const skillsData = skillsLib.data;
  const term = query.trim().toLowerCase();

  // MCP derived
  const blocked = mcpData?.agents
    .filter((entry) => !entry.path || entry.error || (entry.kind === "pi" && legacyPi))
    .map((entry) => entry.kind) ?? [];
  const mcpRows = mcpData?.rows.filter((row) =>
    (!agent || row.bindings.some((binding) => binding.kind === agent && (binding.state === "enabled" || binding.wanted)))
    && [row.name, row.endpoint, row.description].some((value) => value.toLowerCase().includes(term)),
  ) ?? [];
  const importableMcp = mcpData?.rows.some((row) => !row.managed && !row.conflict && row.transport);

  // Skills derived
  const matches = (...fields: string[]) => fields.some((value) => value.toLowerCase().includes(term));
  const installedSkills = skillsData?.installed.filter(({ skill, bindings }) => {
    const source = skill.origin?.repository.repository ?? "Local / ZIP";
    return (!agent || bindings.some((binding) => binding.kind === agent && binding.state !== "disabled"))
      && (!updatesOnly || updates?.includes(skill.folder))
      && matches(skill.name, skill.folder, skill.description, source);
  }) ?? [];
  const importedPaths = new Set(skillsData?.installed.map(({ skill }) => skill.importedFrom).filter(Boolean));
  const localSkills = updatesOnly ? [] : skillsData?.local.filter(({ entry, kind }) =>
    !importedPaths.has(entry.path) && (!agent || agent === kind) && matches(entry.name, entry.description, entry.path),
  ) ?? [];

  function changeMcp(action: McpChange, onError?: (message: string) => void) {
    setWriteError(null);
    return mcpLib.change(() => mcpIpc.change(action), onError ?? ((message) => {
      const subject = action.type === "bind" ? action.targets.map((kind) => AGENT_NAMES[kind]).join(", ") : "MCP configuration";
      const names = action.type === "bind" && action.names.length === 1 ? ` for ${action.names[0]}` : "";
      setWriteError({ title: `Could not update ${subject}${names}`, message });
    }));
  }

  function openEditor(server: McpServer | null) {
    setEditor((current) => ({ server, opened: true, revision: (current?.revision ?? 0) + 1 }));
  }
  function closeEditor() {
    setEditor((current) => current?.opened ? { ...current, opened: false } : current);
  }
  async function editMcp(row: McpRow) {
    const server = await attempt(mcpIpc.edit(row.name));
    if (server) openEditor(server);
  }
  async function copyMcp(name?: string) {
    const ok = await confirmAction("Copy server definitions to the clipboard? This can include secrets, but does not include which agents have the servers enabled.", "Copy JSON");
    if (ok) await attempt(mcpIpc.export(name).then(clipboard.write), "MCP configuration copied");
  }
  async function removeMcp(row: McpRow) {
    if (await confirmAction(`Delete ${row.name} from the MCP library and its agent configurations?`, "Delete server")) {
      await changeMcp({ type: "remove", names: [row.name] });
    }
  }

  function changeSkill(action: SkillChange) {
    return skillsLib.change(() => skillsIpc.change(action));
  }
  function installSkill(source: SkillInstall, targets: AgentKind[], mode?: SkillMode) {
    return skillsLib.change(() => skillsIpc.install(source, targets, mode));
  }
  async function checkUpdates() {
    setChecking(true);
    try {
      const found = await attempt(skillsIpc.updates());
      setUpdates(found ?? null);
      if (!found) setUpdatesOnly(false);
    } finally {
      setChecking(false);
    }
  }
  async function updateSkills(folders: string[]) {
    for (const folder of folders) {
      if (!await installSkill({ type: "update", folder }, [])) break;
      setUpdates((current) => current?.filter((name) => name !== folder) ?? null);
    }
  }
  async function removeSkill(folder: string, name: string) {
    if (await confirmAction(`Uninstall ${name}? You can restore managed copies from Skill backups.`, "Uninstall")) {
      await changeSkill({ type: "remove", folders: [folder] });
    }
  }
  async function openSkill(folder: string | LocalSkill) {
    const result = await attempt(typeof folder === "string" ? skillsIpc.read(folder) : agentsIpc.readSkill(folder.kind, folder.entry.path));
    if (result) setDetail(result);
  }

  const mcpCount = mcpData?.rows.length ?? 0;
  const skillCount = skillsData?.installed.length ?? 0;

  return (
    <>
      <ViewHeader title={t("ext.title")} subtitle={t("ext.subtitle")}>
        <SegmentedControl
          size="xs"
          value={tab}
          onChange={(value) => setTab(value as ExtensionTab)}
          aria-label={t("ext.title")}
          data={[
            { value: "mcp", label: mcpData ? `${t("ext.tab.mcp")} (${mcpCount})` : t("ext.tab.mcp") },
            { value: "skills", label: skillsData ? `${t("ext.tab.skills")} (${skillCount})` : t("ext.tab.skills") },
            { value: "discover", label: t("ext.tab.discover") },
          ]}
        />
        {tab === "mcp" ? (
          <>
            <Button size="compact-sm" leftSection={<IconPlus size={14} />} onClick={() => openEditor(null)} disabled={mcpLib.busy || !mcpData}>
              {t("ext.addServer")}
            </Button>
            <Menu position="bottom-end" withinPortal>
              <Menu.Target>
                <ActionIcon variant="subtle" color="gray" aria-label="MCP actions"><IconDots size={17} /></ActionIcon>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item leftSection={<IconDownload size={15} />} disabled={mcpLib.busy || !importableMcp} onClick={() => void changeMcp({ type: "import" })}>
                  {t("ext.importFromAgents")}
                </Menu.Item>
                <Menu.Item leftSection={<IconRefresh size={15} />} disabled={mcpLib.busy || !mcpData?.rows.some((row) => row.managed)} onClick={() => void changeMcp({ type: "sync" })}>
                  {t("ext.syncManaged")}
                </Menu.Item>
                <Menu.Item leftSection={<IconCopy size={15} />} disabled={mcpLib.busy || !mcpData?.rows.some((row) => row.managed)} onClick={() => void copyMcp()}>
                  {t("ext.copyAllJson")}
                </Menu.Item>
              </Menu.Dropdown>
            </Menu>
          </>
        ) : (
          <>
            <Menu position="bottom-end" withinPortal>
              <Menu.Target>
                <Button size="compact-sm" leftSection={<IconPlus size={14} />} disabled={!skillsData || skillsLib.busy}>{t("ext.addSkill")}</Button>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item leftSection={<IconZip size={15} />} onClick={() => setDialog({ type: "zip" })}>{t("ext.installZip")}</Menu.Item>
                <Menu.Item leftSection={<IconDownload size={15} />} onClick={() => setDialog({ type: "local" })}>{t("ext.importFromAgents")}</Menu.Item>
              </Menu.Dropdown>
            </Menu>
            <Menu position="bottom-end" withinPortal>
              <Menu.Target>
                <ActionIcon variant="subtle" color="gray" aria-label="Skills actions"><IconDots size={17} /></ActionIcon>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item leftSection={<IconRefresh size={15} />} disabled={skillsLib.busy || checking || !skillsData?.installed.length} onClick={() => void checkUpdates()}>
                  {checking ? t("ext.checkingUpdates") : t("ext.checkUpdates")}
                </Menu.Item>
                <Menu.Item disabled={skillsLib.busy || !skillsData?.installed.length} onClick={() => void changeSkill({ type: "sync" })}>
                  {t("ext.syncSkills")}
                </Menu.Item>
                <Menu.Divider />
                <Menu.Item disabled={!skillsData} onClick={() => setManageRepos(true)}>{t("ext.manageRepos")}</Menu.Item>
                <Menu.Item disabled={!skillsData} onClick={() => setBackups(true)}>{t("ext.skillBackups")}</Menu.Item>
              </Menu.Dropdown>
            </Menu>
          </>
        )}
      </ViewHeader>

      <div className="ext-scroll">
        <div className="ext-canvas">
          {tab === "mcp" && (
            !mcpData ? <ExtensionStatus error={mcpLib.error} /> : (
              <>
                <div className="ext-controls">
                  <div className="ext-search-wrap">
                    <ExtensionSearch value={query} onChange={setQuery} placeholder={t("ext.searchMcp")} />
                  </div>
                  <FilterPills
                    agent={agent}
                    onAgent={setAgent}
                    total={mcpData.rows.length}
                    count={(kind) => mcpData.rows.filter((row) => row.bindings.some((b) => b.kind === kind && (b.state === "enabled" || b.wanted))).length}
                  />
                </div>

                {mcpData.agents.filter((entry) => entry.error).map((entry) => (
                  <Alert color="yellow" key={entry.kind} title={`${AGENT_NAMES[entry.kind]} MCP config could not be read`}>
                    {entry.path && <span className="extension-path">{entry.path}</span>} {entry.error}
                  </Alert>
                ))}
                {legacyPi && <Alert color="gray">Native MCP requires Pi 1.0 or newer.</Alert>}
                {writeError && (
                  <Alert color="red" role="alert" title={writeError.title} withCloseButton closeButtonLabel="Dismiss error" onClose={() => setWriteError(null)}>
                    {writeError.message}
                  </Alert>
                )}

                {importableMcp && (
                  <div className="ext-banner">
                    <div className="min-w-0 flex-1">
                      <strong>{t("ext.existingDetected")}</strong>
                      <span>{t("ext.existingDetectedHint")}</span>
                    </div>
                    <Button size="xs" variant="light" leftSection={<IconDownload size={13} />} disabled={mcpLib.busy} onClick={() => void changeMcp({ type: "import" })}>
                      {t("ext.importAll")}
                    </Button>
                  </div>
                )}

                {mcpRows.length > 0 ? (
                  <div className="ext-grid stagger">
                    {mcpRows.map((row) => {
                      const activeCount = row.bindings.filter((b) => b.state === "enabled").length;
                      const isStdio = row.transport === "stdio";
                      const supported = AGENT_KINDS.filter((kind) => mcpAvailable(row, kind, blocked));
                      return (
                        <article key={row.name} className="ext-card">
                          <div className="ext-card-top">
                            <span className={`ext-card-icon${activeCount > 0 ? " active" : ""}`} aria-hidden>
                              {isStdio ? <IconTerminal2 size={16} stroke={1.8} /> : <IconWorld size={16} stroke={1.8} />}
                            </span>
                            <div className="ext-card-title">
                              <div className="flex flex-wrap items-center gap-1.5">
                                <strong>{row.name}</strong>
                                {row.transport && <span className="ext-tag">{row.transport}</span>}
                                {row.conflict ? <Badge size="xs" color="yellow" variant="light">Conflict</Badge>
                                  : !row.transport ? <Badge size="xs" color="yellow" variant="light">Unreadable</Badge>
                                  : !row.managed ? <Badge size="xs" color="gray" variant="light">Unmanaged</Badge>
                                  : null}
                              </div>
                              {row.endpoint && <code className="ext-endpoint" title={row.endpoint}>{row.endpoint}</code>}
                              {row.description && <p className="ext-desc">{row.description}</p>}
                            </div>
                            <div className="ext-card-actions">
                              {row.managed && !row.conflict && (
                                <Button size="compact-xs" variant="subtle" color="gray" disabled={mcpLib.busy} onClick={() => void editMcp(row)}>
                                  {t("common.edit")}
                                </Button>
                              )}
                              {row.managed ? (
                                <Menu position="bottom-end" withinPortal>
                                  <Menu.Target>
                                    <ActionIcon size="sm" variant="subtle" color="gray" aria-label={`Actions for ${row.name}`}>
                                      <IconDots size={15} />
                                    </ActionIcon>
                                  </Menu.Target>
                                  <Menu.Dropdown>
                                    <Menu.Item disabled={mcpLib.busy || row.conflict} onClick={() => void editMcp(row)}>{t("ext.editServer")}</Menu.Item>
                                    <Menu.Item leftSection={<IconCopy size={14} />} disabled={mcpLib.busy} onClick={() => void copyMcp(row.name)}>{t("ext.copyAsJson")}</Menu.Item>
                                    <Menu.Item disabled={mcpLib.busy || !supported.length} onClick={() => void changeMcp({ type: "bind", names: [row.name], targets: supported, enabled: true })}>
                                      {t("ext.enableAllAgents")}
                                    </Menu.Item>
                                    <Menu.Item disabled={mcpLib.busy || !supported.length} onClick={() => void changeMcp({ type: "bind", names: [row.name], targets: supported, enabled: false })}>
                                      {t("ext.disableAllAgents")}
                                    </Menu.Item>
                                    <Menu.Divider />
                                    <Menu.Item color="red" leftSection={<IconTrash size={14} />} disabled={mcpLib.busy || row.conflict} onClick={() => void removeMcp(row)}>
                                      {t("ext.deleteServer")}
                                    </Menu.Item>
                                  </Menu.Dropdown>
                                </Menu>
                              ) : row.conflict ? (
                                <Tooltip label="Agents have different native configurations for this server ID">
                                  <Badge size="xs" color="yellow" variant="outline">Check configs</Badge>
                                </Tooltip>
                              ) : null}
                            </div>
                          </div>

                          <div className="ext-scope-row" aria-label={`Enabled agents for ${row.name}`}>
                            <span className="ext-scope-label">{t("ext.agentsLabel")}</span>
                            <div className="ext-scope-chips">
                              {row.bindings.map((binding) => {
                                const note = binding.kind === "pi" && legacyPi ? "Requires Pi 1.0 or newer" : undefined;
                                const statuses: Record<string, string> = {
                                  unreadable: "Config unreadable",
                                  conflict: "Conflicting native config",
                                  unsupported: "Transport unsupported",
                                  local: "Local native config",
                                };
                                const unavailable = note ?? statuses[binding.state];
                                const active = binding.state === "enabled";
                                const canToggle = mcpAvailable(row, binding.kind, blocked);
                                const tooltip = unavailable
                                  ? `${AGENT_NAMES[binding.kind]}: ${unavailable}`
                                  : !canToggle
                                    ? `${AGENT_NAMES[binding.kind]}: Import server to manage`
                                    : active !== binding.wanted
                                      ? `${AGENT_NAMES[binding.kind]} (saved: ${binding.wanted ? "on" : "off"})`
                                      : `${active ? "Disable" : "Enable"} ${row.name} for ${AGENT_NAMES[binding.kind]}`;
                                return (
                                  <AgentScopeChip
                                    key={binding.kind}
                                    kind={binding.kind}
                                    active={active}
                                    drifted={canToggle && active !== binding.wanted}
                                    unavailable={unavailable}
                                    disabled={mcpLib.busy || !canToggle}
                                    tooltip={tooltip}
                                    onToggle={(enabled) => void changeMcp({ type: "bind", names: [row.name], targets: [binding.kind], enabled })}
                                  />
                                );
                              })}
                            </div>
                          </div>
                        </article>
                      );
                    })}
                  </div>
                ) : (
                  <div className="ext-empty-card">
                    <span className="ext-empty-icon"><IconPlug size={22} stroke={1.6} /></span>
                    <strong>{term || agent ? t("ext.noMcpMatch") : t("ext.noMcpTitle")}</strong>
                    <p>
                      {term || agent ? t("ext.clearFiltersHint") : t("ext.noMcpHint")}
                    </p>
                    {!term && !agent && (
                      <div className="mt-2 flex flex-wrap justify-center gap-2">
                        <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => openEditor(null)}>{t("ext.addServer")}</Button>
                        {importableMcp && (
                          <Button size="xs" variant="default" leftSection={<IconDownload size={14} />} onClick={() => void changeMcp({ type: "import" })}>
                            {t("ext.importFromAgents")}
                          </Button>
                        )}
                      </div>
                    )}
                  </div>
                )}
              </>
            )
          )}

          {tab === "skills" && (
            !skillsData ? <ExtensionStatus error={skillsLib.error} /> : (
              <>
                <div className="ext-controls">
                  <div className="ext-search-wrap">
                    <ExtensionSearch value={query} onChange={setQuery} placeholder={t("ext.searchSkills")} />
                  </div>
                  <FilterPills
                    agent={agent}
                    onAgent={setAgent}
                    total={skillsData.installed.length}
                    count={(kind) => skillsData.installed.filter(({ bindings }) => bindings.some((b) => b.kind === kind && b.state !== "disabled")).length}
                    extra={updates != null && updates.length > 0 ? (
                      <button
                        type="button"
                        className={`ext-filter-pill press${updatesOnly ? " active" : ""}`}
                        onClick={() => setUpdatesOnly(!updatesOnly)}
                      >
                        <span>{t("ext.updates")}</span>
                        <span className="ext-filter-count">{updates.length}</span>
                      </button>
                    ) : undefined}
                  />
                </div>

                {updates?.length === 0 && <Alert color="teal">{t("ext.allUpToDate")}</Alert>}

                {installedSkills.length > 0 ? (
                  <div className="ext-grid stagger">
                    {installedSkills.map(({ skill, bindings }) => {
                      const hasUpdate = Boolean(updates?.includes(skill.folder));
                      const activeCount = bindings.filter((b) => b.state === "enabled").length;
                      const source = skill.origin?.repository.repository ?? "Local / ZIP";
                      return (
                        <article key={skill.folder} className="ext-card">
                          <div className="ext-card-top">
                            <span className={`ext-card-icon${activeCount > 0 ? " active" : ""}`} aria-hidden>
                              <IconSparkles size={16} stroke={1.8} />
                            </span>
                            <div className="ext-card-title">
                              <div className="flex flex-wrap items-center gap-1.5">
                                <button type="button" className="ext-title-link" onClick={() => void openSkill(skill.folder)}>
                                  {skill.name}
                                </button>
                                <span className="ext-tag">{source}</span>
                                {hasUpdate && <Badge size="xs" color="blue" variant="light">Update available</Badge>}
                              </div>
                              {skill.description && <p className="ext-desc">{skill.description}</p>}
                            </div>
                            <div className="ext-card-actions">
                              {hasUpdate && (
                                <Button size="compact-xs" variant="light" disabled={skillsLib.busy} onClick={() => void updateSkills([skill.folder])}>
                                  {t("common.update")}
                                </Button>
                              )}
                              <Button size="compact-xs" variant="subtle" color="gray" leftSection={<IconFileText size={13} />} onClick={() => void openSkill(skill.folder)}>
                                SKILL.md
                              </Button>
                              <Menu position="bottom-end" withinPortal>
                                <Menu.Target>
                                  <ActionIcon size="sm" variant="subtle" color="gray" aria-label={`Actions for ${skill.name}`}>
                                    <IconDots size={15} />
                                  </ActionIcon>
                                </Menu.Target>
                                <Menu.Dropdown>
                                  <Menu.Item leftSection={<IconFileText size={14} />} onClick={() => void openSkill(skill.folder)}>{t("ext.readSkillMd")}</Menu.Item>
                                  {skill.origin && (
                                    <Menu.Item leftSection={<IconRefresh size={14} />} disabled={skillsLib.busy} onClick={() => void updateSkills([skill.folder])}>
                                      {t("ext.updateFromRepo")}
                                    </Menu.Item>
                                  )}
                                  <Menu.Divider />
                                  <Menu.Item color="red" leftSection={<IconTrash size={14} />} disabled={skillsLib.busy} onClick={() => void removeSkill(skill.folder, skill.name)}>
                                    {t("common.uninstall")}
                                  </Menu.Item>
                                </Menu.Dropdown>
                              </Menu>
                            </div>
                          </div>

                          <div className="ext-scope-row" aria-label={`Enabled agents for ${skill.name}`}>
                            <span className="ext-scope-label">{t("ext.agentsLabel")}</span>
                            <div className="ext-scope-chips">
                              {bindings.map((binding) => {
                                const unavailable = binding.state === "local" ? "Local native install"
                                  : binding.state === "unsupported" ? "Unsupported"
                                  : binding.state === "conflict" ? "Conflicting native file"
                                  : binding.state === "unreadable" ? "Unreadable"
                                  : undefined;
                                const active = binding.state === "enabled";
                                const tooltip = unavailable
                                  ? `${AGENT_NAMES[binding.kind]}: ${unavailable}`
                                  : `${active ? "Disable" : "Enable"} ${skill.name} for ${AGENT_NAMES[binding.kind]}`;
                                return (
                                  <AgentScopeChip
                                    key={binding.kind}
                                    kind={binding.kind}
                                    active={active}
                                    drifted={!unavailable && active !== binding.wanted}
                                    unavailable={unavailable}
                                    disabled={skillsLib.busy}
                                    tooltip={tooltip}
                                    onToggle={(enabled) => void changeSkill({ type: "bind", folders: [skill.folder], targets: [binding.kind], enabled })}
                                  />
                                );
                              })}
                            </div>
                          </div>
                        </article>
                      );
                    })}
                  </div>
                ) : !localSkills.length ? (
                  <div className="ext-empty-card">
                    <span className="ext-empty-icon"><IconSparkles size={22} stroke={1.6} /></span>
                    <strong>{term || agent || updatesOnly ? t("ext.noSkillsMatch") : t("ext.noSkillsTitle")}</strong>
                    <p>
                      {term || agent || updatesOnly ? t("ext.clearFiltersHint") : t("ext.noSkillsHint")}
                    </p>
                    {!term && !agent && !updatesOnly && (
                      <div className="mt-2 flex flex-wrap justify-center gap-2">
                        <Button size="xs" onClick={() => setTab("discover")}>{t("ext.browseDiscover")}</Button>
                        <Button size="xs" variant="default" onClick={() => setDialog({ type: "zip" })}>{t("ext.installZip")}</Button>
                      </div>
                    )}
                  </div>
                ) : null}

                {localSkills.length > 0 && (
                  <section className="ext-local-section">
                    <div className="ext-local-head">
                      <span>{t("ext.detectedInAgents", { count: localSkills.length })}</span>
                      <Button size="compact-xs" variant="subtle" onClick={() => setDialog({ type: "local" })}>{t("ext.importIntoLibrary")}</Button>
                    </div>
                    <div className="ext-local-list">
                      {localSkills.map((local) => (
                        <button key={`${local.kind}:${local.entry.path}`} type="button" className="ext-local-row press" onClick={() => void openSkill(local)}>
                          <AgentLogo agent={local.kind} size={15} />
                          <div className="min-w-0 flex-1">
                            <strong>{local.entry.name}</strong>
                            <span>{local.entry.description || local.entry.path}</span>
                          </div>
                          <span className="ext-tag">{AGENT_NAMES[local.kind]}</span>
                        </button>
                      ))}
                    </div>
                  </section>
                )}
              </>
            )
          )}

          {tab === "discover" && (
            !skillsData ? <ExtensionStatus error={skillsLib.error} /> : (
              <SkillDiscover
                data={skillsData}
                busy={skillsLib.busy}
                onManage={(folder) => {
                  setQuery(folder);
                  setAgent(null);
                  setUpdatesOnly(false);
                  setTab("skills");
                }}
                onInstall={(name, source) => setDialog({ type: "remote", name, source })}
              />
            )
          )}
        </div>
      </div>

      <Drawer
        opened={editor?.opened ?? false}
        onClose={closeEditor}
        onExitTransitionEnd={() => setEditor((current) => current?.opened ? current : null)}
        title={editor?.server ? `Edit ${editor.server.name}` : "Add MCP server"}
        position="right"
        size="lg"
        classNames={{ header: "extension-editor-header" }}
        closeOnClickOutside={!mcpLib.busy}
        closeOnEscape={!mcpLib.busy}
        withCloseButton={!mcpLib.busy}
      >
        {editor && mcpData && (
          <McpLibraryEditor
            key={editor.revision}
            initial={editor.server}
            rows={mcpData.rows}
            agents={mcpData.agents}
            disabled={blocked}
            busy={mcpLib.busy}
            onSave={changeMcp}
            onClose={closeEditor}
          />
        )}
      </Drawer>

      {dialog && skillsData && (
        <InstallSkillDialog
          dialog={dialog}
          local={skillsData.local}
          initialAgent={agent ?? undefined}
          busy={skillsLib.busy}
          onClose={() => setDialog(null)}
          onInstall={async (source, targets, mode) => {
            const saved = await installSkill(source, targets, mode);
            if (saved) setTab("skills");
            return saved;
          }}
        />
      )}
      {manageRepos && skillsData && (
        <SkillRepositoryDialog
          repositories={skillsData.repositories}
          onClose={() => setManageRepos(false)}
          onSaved={() => void skillsLib.reload()}
        />
      )}
      {backups && skillsData && (
        <SkillBackupsDialog
          backups={skillsData.backups}
          busy={skillsLib.busy}
          onClose={() => setBackups(false)}
          onChange={changeSkill}
        />
      )}
      {detail && (
        <Modal opened title={detail.name} onClose={() => setDetail(null)} size="lg" centered>
          {detail.description && <p className="mt-0 text-sm text-muted">{detail.description}</p>}
          <span className="extension-path">{detail.path}</span>
          <div className="extension-skill-markdown"><Markdown text={detail.body} /></div>
        </Modal>
      )}
      {checking && <span className="sr-only" role="status"><Loader size={12} /> Checking skill updates</span>}
    </>
  );
}
