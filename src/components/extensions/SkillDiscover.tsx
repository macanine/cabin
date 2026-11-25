import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Alert, Badge, Button, Group, Loader, Menu, SegmentedControl } from "@mantine/core";
import { IconArrowLeft, IconArrowRight } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { formatInteger } from "../../lib/format";
import { toIpcError } from "../../services/ipc/common";
import { skillsIpc } from "../../services/ipc/extensions";
import type { DiscoveredSkill, SkillInstall, SkillLibrary, SkillRegistry, SkillRepository } from "../../services/ipc/generated/core";
import { ExtensionSearch } from "./LibraryParts";

const REPO_PAGE_SIZE = 40;

function sourceId(source: SkillRepository) { return `${source.repository}/${source.branch}/${source.directory}`; }

export function SkillDiscover({ data, busy, onInstall, onManage }: {
  data: SkillLibrary; busy: boolean;
  onInstall: (name: string, source: Extract<SkillInstall, { type: "repository" | "registry" }>) => void;
  onManage: (folder: string) => void;
}) {
  const [tab, setTab] = useState("repositories");
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(0);
  const [results, setResults] = useState<{ id: string; skills: DiscoveredSkill[]; error: string }[]>([]);
  const [activeSource, setActiveSource] = useState<string | null>(null);
  const [registryQuery, setRegistryQuery] = useState("");
  const [registry, setRegistry] = useState<{ query: string; offset: number; data: SkillRegistry } | null>(null);
  const [registryBusy, setRegistryBusy] = useState(false);
  const [registryError, setRegistryError] = useState("");
  const [reload, setReload] = useState(0);
  useAutoRefresh(() => setReload((n) => n + 1), { enabled: tab === "repositories", initial: false, interval: 300_000, minGap: 60_000 });
  const repositories = data.repositories;
  const repositoriesRef = useRef(repositories);
  repositoriesRef.current = repositories;
  const sourcesKey = repositories.map(sourceId).join("\n");
  useEffect(() => {
    let current = true;
    const queue = [...repositoriesRef.current];
    const ids = new Set(queue.map(sourceId));
    setResults((previous) => previous.filter(({ id }) => ids.has(id)));
    async function worker() {
      while (current && queue.length) {
        const source = queue.shift()!;
        const id = sourceId(source);
        let result;
        try { result = { id, skills: await skillsIpc.discover(source), error: "" }; }
        catch (cause) { result = { id, skills: [], error: toIpcError(cause).message }; }
        if (current) setResults((results) => [...results.filter((entry) => entry.id !== id), result]);
      }
    }
    void Promise.all(Array.from({ length: Math.min(queue.length, 3) }, worker));
    return () => { current = false; };
  }, [sourcesKey, reload]);

  async function search(eventQuery: string, offset: number) {
    const query = eventQuery.trim();
    if (!query || registryBusy) return;
    setRegistryBusy(true);
    setRegistryError("");
    try { setRegistry({ query, offset, data: await skillsIpc.registry(query, offset) }); }
    catch (cause) { setRegistryError(toIpcError(cause).message); }
    finally { setRegistryBusy(false); }
  }

  const term = query.trim().toLowerCase();
  const skills = results.filter((result) => !activeSource || result.id === activeSource).flatMap((result) => result.skills)
    .filter((skill) => [skill.name, skill.folder, skill.description, skill.origin.repository.repository, skill.origin.path].some((field) => field.toLowerCase().includes(term)))
    .sort((a, b) => a.folder.localeCompare(b.folder, "en"));
  const lastPage = Math.max(0, Math.ceil(skills.length / REPO_PAGE_SIZE) - 1);
  const visiblePage = Math.min(page, lastPage);
  const visible = skills.slice(visiblePage * REPO_PAGE_SIZE, (visiblePage + 1) * REPO_PAGE_SIZE);
  const installed = new Map(data.installed.map(({ skill }) => [skill.folder, skill.origin]));
  const repoStatus = (skill: DiscoveredSkill) => {
    if (!installed.has(skill.folder)) return null;
    const origin = installed.get(skill.folder);
    return origin && sourceId(origin.repository) === sourceId(skill.origin.repository) && origin.path === skill.origin.path ? "Installed" : "Name in use";
  };
  const repoLoading = results.length < repositories.length;
  return <>
    <div className="extension-toolbar">
      <SegmentedControl aria-label="Discovery source" value={tab} onChange={setTab}
        data={[{ value: "repositories", label: "Repositories" }, { value: "registry", label: "skills.sh" }]} />
    </div>
    {tab === "repositories" ? <>
      <div className="extension-toolbar">
        <ExtensionSearch value={query} onChange={(value) => { setQuery(value); setPage(0); }} placeholder="Filter repository skills" />
        <Menu position="bottom-end"><Menu.Target><Button variant="default">{repositories.find((source) => sourceId(source) === activeSource)?.repository ?? "All repositories"}</Button></Menu.Target>
          <Menu.Dropdown><Menu.Item onClick={() => { setActiveSource(null); setPage(0); }}>All repositories</Menu.Item>
            {repositories.map((source) => <Menu.Item key={sourceId(source)} onClick={() => { setActiveSource(sourceId(source)); setPage(0); }}>{source.repository} · {source.branch}</Menu.Item>)}
          </Menu.Dropdown>
        </Menu>
      </div>
      {results.filter((result) => result.error).map((result) => <Alert key={result.id} color="yellow" title="Repository unavailable">{repositories.find((source) => sourceId(source) === result.id)?.repository} · {result.error}</Alert>)}
      <div className="extension-discovery-list">
        {visible.map((skill) => <DiscoveryRow key={`${sourceId(skill.origin.repository)}/${skill.origin.path}`}
          name={skill.name} detail={[skill.origin.repository.repository, skill.description || skill.origin.path].filter(Boolean).join(" · ")}
          status={repoStatus(skill)} busy={busy}
          onManage={() => onManage(skill.folder)}
          onInstall={() => onInstall(skill.name, { type: "repository", origin: skill.origin })} />)}
        {repoLoading ? <div className="extension-empty"><Loader size="xs" aria-label="Loading repositories" /></div>
          : !skills.length && <div className="extension-empty">{repositories.length ? "No matching repository skills" : "Add a GitHub repository to discover skills."}</div>}
        {lastPage > 0 && <Group justify="flex-end" p="sm">
          <span className="extension-page-count">{formatInteger(visiblePage * REPO_PAGE_SIZE + 1)}–{formatInteger(visiblePage * REPO_PAGE_SIZE + visible.length)} of {formatInteger(skills.length)}</span>
          <Button variant="default" leftSection={<IconArrowLeft size={14} />} disabled={visiblePage === 0} onClick={() => setPage(visiblePage - 1)}>Previous</Button>
          <Button variant="default" rightSection={<IconArrowRight size={14} />} disabled={visiblePage === lastPage} onClick={() => setPage(visiblePage + 1)}>Next</Button>
        </Group>}
      </div>
    </> : <>
      <form className="extension-toolbar" onSubmit={(event) => { event.preventDefault(); void search(registryQuery, 0); }}>
        <ExtensionSearch value={registryQuery} onChange={setRegistryQuery} placeholder="Search skills.sh" />
        <Button loading={registryBusy} disabled={!registryQuery.trim()} type="submit">Search</Button>
      </form>
      {registryError && <Alert color="yellow">{registryError}</Alert>}
      {registry && <div className="extension-discovery-list">
        {registry.data.skills.map((skill) => <DiscoveryRow key={skill.id} name={skill.name}
          detail={`${skill.source} · ${formatInteger(skill.installs)} installs`} status={installed.has(skill.skillId) ? installed.get(skill.skillId)?.repository.repository === skill.source.toLowerCase() ? "Installed" : "Name in use" : null} busy={busy}
          onManage={() => onManage(skill.skillId)}
          onInstall={() => onInstall(skill.name, { type: "registry", repository: skill.source, skillId: skill.skillId })} />)}
        {!registry.data.skills.length && <div className="extension-empty">No skills found for “{registry.query}”.</div>}
        {(registry.offset > 0 || registry.data.skills.length >= 30) && <Group justify="flex-end" p="sm">
          <Button variant="default" leftSection={<IconArrowLeft size={14} />} disabled={registryBusy || registry.offset === 0}
            onClick={() => void search(registry.query, Math.max(0, registry.offset - 30))}>Previous</Button>
          <Button variant="default" rightSection={<IconArrowRight size={14} />} disabled={registryBusy || registry.data.skills.length < 30}
            onClick={() => void search(registry.query, registry.offset + 30)}>Next</Button>
        </Group>}
      </div>}
    </>}
  </>;
}

function DiscoveryRow({ name, detail, status, busy, onInstall, onManage }: {
  name: string; detail: string; status: "Installed" | "Name in use" | null; busy: boolean;
  onInstall: () => void; onManage: () => void;
}) {
  return <div className="extension-discovery-row">
    <div className="extension-row-info"><strong>{name}</strong><span title={detail}>{detail}</span></div>
    {status === "Name in use" ? <Badge variant="light" color="yellow">Name in use</Badge>
      : status === "Installed" ? <Button size="compact-sm" variant="subtle" color="gray" aria-label={`Manage installed skill ${name}`} disabled={busy} onClick={onManage}>Manage</Button>
      : <Button size="compact-sm" variant="light" aria-label={`Install ${name}`} disabled={busy} onClick={onInstall}>Install</Button>}
  </div>;
}
