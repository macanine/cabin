import { Menu, Textarea, Tooltip } from "@mantine/core";
import { IconArrowUp, IconCheck, IconChevronDown, IconFolder, IconFolderPlus } from "@tabler/icons-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentKind, ChatPermissionMode, Workspace } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { useApp } from "../state/store";
import { lastStart, rememberStart, setComposerHandoff, uiActions } from "../state/ui";
import { attempt } from "../lib/operations";
import { AGENT_NAMES, CircleButton } from "./common";
import { AgentMark } from "./AgentMarks";
import { AGENT_KINDS } from "./AgentPicker";
import { NewWorkspaceModal } from "./NewWorkspaceModal";
import { AttachmentList, AttachmentPicker, useChatAttachments } from "./ChatAttachments";
import { forgetModel, lastModel, rememberModel } from "../state/modelMemory";
import { ModelPicker } from "./ModelPicker";
import { DEFAULT_CHAT_PERMISSION, PermissionPicker } from "./PermissionPicker";
import { ComposerMenu } from "./chat/ComposerMenu";
import { isComposerComposing, useComposerMenu } from "./chat/useComposerMenu";
import { useI18n } from "../i18n";

/** The project the task runs in: a quiet button with a menu. */
function WorkspacePicker({ workspaces, value, fixed, onSelect, onCreate }: {
  workspaces: Workspace[];
  value: string | null;
  fixed: boolean;
  onSelect: (id: string) => void;
  onCreate: () => void;
}) {
  const { t } = useI18n();
  const current = workspaces.find((w) => w.id === value);
  if (fixed) {
    return (
      <span className="inline-flex h-7 max-w-60 min-w-0 items-center gap-1.5 px-2 text-[13px] font-medium text-fg [&>svg]:shrink-0 [&>svg]:text-muted" title={current?.path}>
        <IconFolder size={14} />
        <span className="truncate">{current?.name ?? t("composer.workspace")}</span>
      </span>
    );
  }
  if (workspaces.length === 0) {
    return (
      <button className="press inline-flex h-7 max-w-60 min-w-0 cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-transparent px-2 text-[13px] font-medium text-fg hover:bg-hover data-expanded:bg-hover [&>svg]:shrink-0 [&>svg]:text-muted" onClick={onCreate}>
        <IconFolderPlus size={14} />
        <span>{t("composer.newWorkspace")}</span>
      </button>
    );
  }
  return (
    <Menu position="bottom-start" withinPortal shadow="md" width={280}>
      <Menu.Target>
        <button className="press inline-flex h-7 max-w-60 min-w-0 cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-transparent px-2 text-[13px] font-medium text-fg hover:bg-hover data-expanded:bg-hover [&>svg]:shrink-0 [&>svg]:text-muted" aria-label={t("composer.workspace")} title={current?.path}>
          <IconFolder size={14} />
          <span className="truncate">{current?.name ?? t("composer.chooseWorkspace")}</span>
          <IconChevronDown size={13} className="!text-faint" />
        </button>
      </Menu.Target>
      <Menu.Dropdown>
        {workspaces.map((w) => (
          <Menu.Item
            key={w.id}
            onClick={() => onSelect(w.id)}
            leftSection={<IconFolder size={14} />}
            rightSection={w.id === value ? <IconCheck size={14} className="text-accent" /> : null}
          >
            <div className="min-w-0">
              <div className="truncate">{w.name}</div>
              <div className="truncate font-mono text-xs text-faint" title={w.path}>{w.path}</div>
            </div>
          </Menu.Item>
        ))}
        <Menu.Divider />
        <Menu.Item leftSection={<IconFolderPlus size={14} />} onClick={onCreate}>{t("composer.newWorkspace")}</Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}

/** One mark per agent; the chosen one is lit, missing ones are dimmed. */
function AgentChoice({ value, installed, onChange }: {
  value: AgentKind;
  installed: (kind: AgentKind) => boolean;
  onChange: (kind: AgentKind) => void;
}) {
  return (
    <div className="inline-flex items-center gap-0.5 rounded-[9px] bg-hover p-0.5" role="radiogroup" aria-label="Agent">
      {AGENT_KINDS.map((kind) => {
        const available = installed(kind);
        const label = available ? AGENT_NAMES[kind] : `${AGENT_NAMES[kind]} is not installed`;
        return (
          <Tooltip key={kind} label={label} openDelay={250}>
            <button
              className={`press inline-flex h-6 cursor-pointer items-center justify-center gap-1.5 rounded-[7px] border-0 p-0 text-muted transition-[width,background-color,color,box-shadow] duration-300 ease-out disabled:cursor-default disabled:opacity-35 ${
                value === kind ? "bg-surface pr-2.5 pl-2 text-fg shadow-[0_1px_2px_rgb(0_0_0/0.07),inset_0_0_0_1px_var(--glass-edge)]" : "w-[30px] bg-transparent hover:not-disabled:text-fg"
              }`}
              role="radio"
              aria-checked={value === kind}
              aria-label={label}
              disabled={!available}
              onClick={() => onChange(kind)}
            >
              <AgentMark agent={kind} size={15} />
              {value === kind && <span className="text-xs font-medium whitespace-nowrap">{AGENT_NAMES[kind]}</span>}
            </button>
          </Tooltip>
        );
      })}
    </div>
  );
}

/** The start box. The bar on top picks where (workspace) and who (agent);
 * the box holds the first task, its attachments and how it runs. Enter
 * starts the session; Shift+Enter adds a line. */
export function Composer({ workspaceId: fixedWorkspace, autoFocus = true, onSubmittingChange }: {
  workspaceId?: string;
  autoFocus?: boolean;
  onSubmittingChange?: (submitting: boolean) => void;
}) {
  const { t } = useI18n();
  const workspaces = useApp((state) => state.workspaces.filter((w) => !w.archived));
  const agents = useApp((state) => state.agents);
  const defaultAgent = useApp((state) => state.settings?.defaultAgent ?? "claude");
  const remembered = useMemo(lastStart, []);
  const [prompt, setPrompt] = useState("");
  const [caret, setCaret] = useState(0);
  const [workspaceId, setWorkspaceId] = useState<string | null>(fixedWorkspace ?? remembered.workspaceId ?? null);
  const [agent, setAgent] = useState<AgentKind>(remembered.agent ?? defaultAgent);
  const [model, setModel] = useState<string | null>(() => lastModel(remembered.agent ?? defaultAgent));
  const [permission, setPermission] = useState<ChatPermissionMode>(DEFAULT_CHAT_PERMISSION);
  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState(false);
  const attachments = useChatAttachments();
  const input = useRef<HTMLTextAreaElement>(null);
  const box = useRef<HTMLDivElement>(null);
  const composing = useRef(false);

  const installed = (kind: AgentKind) => agents.find((a) => a.kind === kind)?.installed ?? false;
  const complete = !!workspaceId && installed(agent) && !busy;
  const menu = useComposerMenu({ text: prompt, caret, workspaceId, agent, enabled: complete,
    onApply: (next, at) => {
      setPrompt(next);
      setCaret(at);
      requestAnimationFrame(() => {
        input.current?.focus();
        input.current?.setSelectionRange(at, at);
      });
    },
  });

  // Fall back to a valid workspace and an installed agent.
  useEffect(() => {
    if (fixedWorkspace) setWorkspaceId(fixedWorkspace);
    else if (!workspaces.some((w) => w.id === workspaceId)) setWorkspaceId(workspaces[0]?.id ?? null);
  }, [fixedWorkspace, workspaces, workspaceId]);
  useEffect(() => {
    if (agents.length && !installed(agent)) {
      const first = AGENT_KINDS.find(installed);
      if (first) chooseAgent(first);
    }
  }, [agents, agent]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (autoFocus) input.current?.focus();
  }, [autoFocus]);

  function chooseAgent(kind: AgentKind) {
    setAgent(kind);
    setModel(lastModel(kind));
    setPermission(DEFAULT_CHAT_PERMISSION);
  }

  const anyInstalled = AGENT_KINDS.some(installed);
  // A chat always runs on a model the user picked.
  const needsModel = model == null;
  const ready = workspaceId != null && installed(agent) && !busy && !attachments.loading && !needsModel;

  async function start() {
    if (workspaceId == null && !fixedWorkspace) {
      // No workspace yet: create one first, then press Enter again.
      setCreating(true);
      return;
    }
    if (needsModel && workspaceId && installed(agent)) { uiActions.toast("Choose a model first", "info"); return; }
    if (!ready || !workspaceId) return;
    setBusy(true);
    onSubmittingChange?.(true);
    const session = await attempt(
      libraryIpc.createChat(workspaceId, agent, prompt.trim() || null, model, attachments.files, permission, null),
    );
    if (!session) {
      setBusy(false);
      onSubmittingChange?.(false);
      // The remembered model may be what failed; the next visit asks again.
      forgetModel(agent);
      return;
    }
    if (box.current) setComposerHandoff(session.id, box.current.getBoundingClientRect());
    rememberStart(workspaceId, agent);
    if (model) rememberModel(agent, model);
    setPrompt("");
    attachments.clear();
    uiActions.navigate({ kind: "session", id: session.id });
  }

  return (
    <div className="relative">
      <div className="mb-2 flex min-w-0 flex-wrap items-center justify-between gap-2 px-1">
        <WorkspacePicker
          workspaces={workspaces}
          value={workspaceId}
          fixed={fixedWorkspace != null}
          onSelect={setWorkspaceId}
          onCreate={() => setCreating(true)}
        />
        <AgentChoice value={agent} installed={installed} onChange={chooseAgent} />
      </div>
      <div ref={box} className={`composer relative${busy ? " submitting" : ""}`}>
        {menu.open && <ComposerMenu menu={menu} placement="below" />}
        <Textarea
          {...menu.inputProps}
          ref={input}
          variant="unstyled"
          autosize
          minRows={2}
          maxRows={9}
          classNames={{ input: "composer-textarea" }}
          placeholder={complete ? t("composer.placeholder") : t("composer.placeholderFor", { agent: AGENT_NAMES[agent] })}
          value={prompt}
          onChange={(event) => { setPrompt(event.currentTarget.value); setCaret(event.currentTarget.selectionStart); }}
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          onCompositionStart={() => { composing.current = true; }}
          onCompositionEnd={() => { composing.current = false; }}
          onKeyDown={(e) => {
            if (isComposerComposing(e, composing.current) || menu.onKey(e)) return;
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              if (!e.repeat) void start();
            }
          }}
          aria-label="First task"
        />
        <AttachmentList state={attachments} />
        <div className="composer-bar">
          <AttachmentPicker state={attachments} disabled={busy} />
          <ModelPicker workspaceId={workspaceId} agent={agent} value={model} disabled={!installed(agent) || busy} onSelect={setModel} />
          <PermissionPicker agent={agent} value={permission} disabled={busy} onChange={setPermission} />
          <span className="min-w-0 flex-1" />
          <CircleButton
            label={
              !anyInstalled
                ? "Install an agent in Agents first"
                : workspaceId == null
                  ? "Choose or create a workspace"
                  : needsModel
                    ? "Choose a model first"
                    : "Start (Enter)"
            }
            primary
            loading={busy}
            disabled={!ready && !(workspaceId == null && anyInstalled)}
            onClick={start}
          >
            <IconArrowUp size={16} stroke={2.2} />
          </CircleButton>
        </div>
      </div>
      <NewWorkspaceModal
        opened={creating}
        onClose={() => setCreating(false)}
        onCreated={(workspace) => {
          setCreating(false);
          setWorkspaceId(workspace.id);
          input.current?.focus();
        }}
      />
    </div>
  );
}
