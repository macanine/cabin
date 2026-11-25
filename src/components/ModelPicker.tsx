import { useAutoRefresh } from "../lib/useAutoRefresh";
import { Loader, Popover } from "@mantine/core";
import { CHIP } from "./ComposerChip";
import { IconCheck, IconChevronDown, IconCornerDownLeft, IconCube, IconSearch } from "@tabler/icons-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentKind, AgentModel, ChatConfigOption } from "../services/ipc/generated/core";
import { toIpcError } from "../services/ipc/common";
import { libraryIpc } from "../services/ipc/library";
import { currentLabel } from "../lib/agentControls";
import { customCandidate, flatModels, groupModels, modelLabel } from "../lib/modelList";
import { modelName, rememberModelNames } from "../state/modelMemory";
import { uiActions } from "../state/ui";

const CACHE_MS = 2 * 60 * 1000;
/** A model listing that has not answered by then is reported, not waited on. */
const LOAD_TIMEOUT_MS = 25_000;

const cache = new Map<string, { at: number; models: AgentModel[] }>();
/** One list request per agent/workspace, shared by every picker on screen. */
const inflight = new Map<string, Promise<AgentModel[]>>();

function fetchModels(key: string, workspaceId: string, agent: AgentKind): Promise<AgentModel[]> {
  const running = inflight.get(key);
  if (running) return running;
  const request = libraryIpc.agentModels(workspaceId, agent)
    .then((list) => {
      // Keep the guest's order (default first); drop duplicates.
      const unique = [...new Map(list.map((model) => [model.id, model])).values()];
      cache.set(key, { at: Date.now(), models: unique });
      rememberModelNames(agent, unique);
      return unique;
    })
    .finally(() => inflight.delete(key));
  inflight.set(key, request);
  return request;
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error("The agent took too long to list its models")), ms);
    promise.then(
      (value) => { window.clearTimeout(timer); resolve(value); },
      (error) => { window.clearTimeout(timer); reject(error); },
    );
  });
}

/** Lists longer than this get a search field; short ones are just a menu. */
const SEARCH_FROM = 10;

/** Reasoning effort as its own section of the menu: one row per level. */
function Effort({ option, onChange }: { option: ChatConfigOption; onChange: (value: string) => Promise<void> | void }) {
  const [applying, setApplying] = useState<string | null>(null);
  async function choose(value: string) {
    if (value === option.current || applying) return;
    setApplying(value);
    try { await onChange(value); }
    catch (cause) { uiActions.toast(toIpcError(cause).message, "error"); }
    finally { setApplying(null); }
  }
  return (
    <div className="border-t border-line/70 px-3 pt-2 pb-3" role="radiogroup" aria-label={option.name || "Reasoning effort"}>
      <div className="mb-1.5 text-[11px] font-semibold tracking-wide text-faint uppercase">{option.name || "Reasoning effort"}</div>
      <div className="flex gap-1 rounded-xl bg-hover p-1">
        {option.choices.map((choice) => {
          const current = choice.value === option.current;
          return (
            <button
              key={choice.value}
              type="button"
              role="radio"
              aria-checked={current}
              title={choice.description ?? undefined}
              disabled={applying != null}
              onClick={() => void choose(choice.value)}
              className={`press flex min-w-0 flex-1 cursor-pointer items-center justify-center gap-1 truncate rounded-lg border-0 px-2 py-1 text-xs font-medium disabled:cursor-default ${current ? "bg-sel text-fg" : "bg-transparent text-muted hover:text-fg"}`}
            >
              {applying === choice.value ? <Loader size={11} /> : choice.name}
            </button>
          );
        })}
      </div>
    </div>
  );
}

/**
 * The model for this conversation. The list is the running agent's own
 * selector when it has one (`live`), otherwise the guest's model directory,
 * fetched when the picker first opens and cached briefly. Search narrows it;
 * an id that is not listed can be used as typed.
 */
export function ModelPicker({ workspaceId, agent, value, disabled, onSelect, live = null, reasoning = null, onReasoningChange, compact = false }: {
  compact?: boolean;
  workspaceId: string | null;
  agent: AgentKind;
  value: string | null;
  disabled?: boolean;
  onSelect: (model: string) => void | Promise<void>;
  /** The running agent's own model selector; when present it is the list. */
  live?: ChatConfigOption | null;
  /** The agent's reasoning-effort selector, shown above the models. */
  reasoning?: ChatConfigOption | null;
  onReasoningChange?: (value: string) => Promise<void> | void;
}) {
  const key = `${agent}:${workspaceId ?? ""}`;
  const [opened, setOpened] = useState(false);
  const [models, setModels] = useState<AgentModel[]>(() => cache.get(key)?.models ?? []);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState<string | null>(null);
  const request = useRef(0);
  const list = useRef<HTMLDivElement>(null);

  useEffect(() => {
    request.current++;
    setModels(cache.get(key)?.models ?? []);
    setError(null);
    setLoading(false);
  }, [key]);

  async function load() {
    if (!workspaceId || live || loading) return;
    const hit = cache.get(key);
    if (hit && Date.now() - hit.at < CACHE_MS) { setModels(hit.models); return; }
    const token = ++request.current;
    setLoading(true);
    try {
      const next = await withTimeout(fetchModels(key, workspaceId, agent), LOAD_TIMEOUT_MS);
      if (request.current === token) { setModels(next); setError(null); }
    } catch (cause) {
      if (request.current === token) setError(toIpcError(cause).message);
    } finally {
      if (request.current === token) setLoading(false);
    }
  }

  useAutoRefresh(() => load(), { enabled: opened && !live, interval: error ? 12_000 : 60_000, minGap: 5_000, initial: false });

  const listed: AgentModel[] = useMemo(() => live
    ? live.choices.map((choice) => ({
      id: choice.value,
      name: choice.name || choice.value,
      provider: choice.group,
      description: choice.description,
      isDefault: false,
    }))
    : models, [live, models]);
  useEffect(() => { if (live) rememberModelNames(agent, listed); }, [agent, live, listed]);
  const { groups, shown, total } = useMemo(() => groupModels(listed, query), [listed, query]);
  const rows = useMemo(() => flatModels(groups), [groups]);
  const custom = live ? null : customCandidate(query, listed);
  // Short lists are a plain menu (keys still move through it); long ones
  // (OpenCode lists hundreds) get a search field that also takes custom ids.
  const searchable = listed.length > SEARCH_FROM || (!live && listed.length === 0) || query !== "";
  const options = custom ? rows.length + 1 : rows.length;
  const label = (value && !listed.some((model) => model.id === value) ? modelName(agent, value) : null) ?? modelLabel(value, listed);
  const effort = reasoning && reasoning.choices.length > 1 ? reasoning : null;
  const needsChoice = !value && !disabled && !live;

  useEffect(() => setActive(0), [query, opened]);
  // Start on the current model.
  useEffect(() => {
    if (!opened || query) return;
    const index = rows.findIndex((model) => model.id === value);
    if (index >= 0) setActive(index);
  }, [opened, rows.length]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  async function choose(model: string) {
    if (applying || disabled) return;
    if (model === value) { setOpened(false); return; }
    setApplying(model);
    try {
      await onSelect(model);
      setOpened(false);
      setQuery("");
    } catch (cause) {
      uiActions.toast(toIpcError(cause).message, "error");
    } finally {
      setApplying(null);
    }
  }

  function onKey(event: React.KeyboardEvent) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (options) setActive((index) => (index + (event.key === "ArrowDown" ? 1 : -1) + options) % options);
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (active < rows.length) void choose(rows[active].id);
      else if (custom) void choose(custom);
    } else if (event.key === "Escape") {
      setOpened(false);
    }
  }

  let index = -1;
  return (
    <Popover
      opened={opened}
      onChange={setOpened}
      position="top-start"
      width={320}
      withinPortal
      shadow="md"
      offset={8}
      trapFocus
    >
      <Popover.Target>
        <button
          type="button"
          className={`${CHIP} ${compact ? "!max-w-[min(100%,13rem)]" : ""} ${needsChoice ? "!bg-accent-soft !text-accent" : ""}`}
          disabled={disabled || !workspaceId}
          aria-label={`Model: ${label ?? "not chosen"}${effort ? ` · ${currentLabel(effort)}` : ""}`}
          title={value ?? "Choose the model for this conversation"}
          onClick={() => {
            const next = !opened;
            setOpened(next);
            if (next) void load();
          }}
        >
          {applying ? <Loader size={12} /> : <IconCube size={14} stroke={1.7} className="shrink-0 opacity-70" />}
          <span className="truncate">{label ?? (loading ? "Loading models…" : "Choose a model")}</span>
          {effort && !compact && <span className="shrink-0 text-faint">{currentLabel(effort)}</span>}
          <IconChevronDown size={12} className="shrink-0 opacity-60" />
        </button>
      </Popover.Target>
      <Popover.Dropdown p={0} onKeyDown={onKey} style={{ overflow: "hidden" }}>
        {searchable && (
          <div className="flex items-center gap-2 border-b border-line/70 py-2 pr-2 pl-3 text-faint focus-within:border-[color:var(--field-focus-border)]">
            <IconSearch size={14} className="shrink-0" />
            <input
              autoFocus
              className="min-w-0 flex-1 border-0 bg-transparent font-[inherit] text-[13px] text-fg outline-none placeholder:text-faint"
              placeholder={live ? "Search models" : "Search, or type a model id"}
              aria-label="Search models"
              value={query}
              onChange={(event) => setQuery(event.currentTarget.value)}
            />
          </div>
        )}

        <div className="max-h-[min(46vh,340px)] overflow-y-auto overscroll-contain p-1" ref={list} role="listbox" aria-label="Models">
          <div className="px-2 pt-1.5 pb-1 text-[11px] font-semibold tracking-wide text-faint uppercase">Model</div>
          {loading && !rows.length && (
            <div className="flex items-center gap-2 px-2.5 py-3 text-sm text-muted" role="status"><Loader size={12} aria-hidden /> Loading models…</div>
          )}
          {error && (
            <div className="flex items-center justify-between gap-2 px-2.5 py-3 text-[12.5px] text-bad">
              <span className="min-w-0 [overflow-wrap:anywhere]">{error}</span>
            </div>
          )}
          {groups.map((group) => (
            <div key={group.provider ?? "-"} className="[&+&]:mt-1">
              {groups.length > 1 && group.provider && <div className="sticky -top-1 z-[1] bg-[var(--glass-strong)] px-2 pt-1.5 pb-0.5 text-[11px] font-medium text-faint backdrop-blur">{group.provider}</div>}
              {group.models.map((model) => {
                index++;
                const at = index;
                const current = model.id === value;
                return (
                  <button
                    key={model.id}
                    type="button"
                    role="option"
                    aria-selected={current}
                    data-index={at}
                    className={`flex w-full cursor-pointer items-center gap-2 rounded-lg border-0 px-2 py-1.5 text-left text-fg transition-colors duration-100 ${at === active ? "bg-accent-soft" : "bg-transparent"}`}
                    onMouseMove={() => at !== active && setActive(at)}
                    onClick={() => void choose(model.id)}
                    title={model.id}
                  >
                    <span className="grid min-w-0 flex-1">
                      <span className="flex min-w-0 items-center gap-1.5">
                        <span className={`truncate text-[13px] ${current ? "font-semibold" : ""}`}>{model.name}</span>
                        {model.isDefault && <span className="shrink-0 rounded-full bg-hover px-1.5 py-px text-[10px] font-medium text-faint">Default</span>}
                      </span>
                      {model.description && <span className="truncate text-[11.5px] text-faint">{model.description}</span>}
                    </span>
                    {applying === model.id ? <Loader size={12} /> : current ? <IconCheck size={15} className="shrink-0 text-accent" /> : null}
                  </button>
                );
              })}
            </div>
          ))}
          {custom && (
            <button
              type="button"
              role="option"
              aria-selected={false}
              data-index={rows.length}
              className={`mt-1 flex w-full cursor-pointer items-center gap-2 rounded-lg border-0 border-t border-dashed px-2 py-1.5 text-left text-fg ${active === rows.length ? "bg-accent-soft" : "bg-transparent"}`}
              onMouseMove={() => setActive(rows.length)}
              onClick={() => void choose(custom)}
            >
              <span className="grid min-w-0 flex-1"><span className="truncate text-[13px]">Use “{custom}”</span><span className="text-[11.5px] text-faint">A model id that is not in the list</span></span>
              <IconCornerDownLeft size={13} className="shrink-0 text-faint" />
            </button>
          )}
          {!loading && !error && !rows.length && !custom && (
            <div className="px-2.5 py-3.5 text-[12.5px] text-faint">{query ? "No model matches." : "This agent listed no models."}</div>
          )}
        </div>
        {total > shown && <div className="border-t border-line/70 px-3 py-1.5 text-[11.5px] text-faint">Showing {shown} of {total}. Type to narrow the list.</div>}
        {effort && onReasoningChange && <Effort option={effort} onChange={onReasoningChange} />}
      </Popover.Dropdown>
    </Popover>
  );
}
