import { Modal } from "@mantine/core";
import {
  IconActivity, IconBrandChrome, IconChartBar, IconMessage, IconPlug, IconPlus, IconRobot, IconSearch, IconSettings,
  IconSparkles, IconTerminal2, IconWorld,
} from "@tabler/icons-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { rankPalette, type PaletteItem } from "../lib/commandPalette";
import { lastActivity, shortAge } from "../lib/taskList";
import { openTerminal } from "../lib/terminal";
import { useActivity } from "../state/unread";
import { useApp } from "../state/store";
import { SETTINGS_TABS, uiActions, useUi, type View } from "../state/ui";
import { IS_MAC } from "../lib/platform";
import { sessionKindName } from "./common";
import { type StaticMessageKey, useI18n } from "../i18n";

interface Entry extends PaletteItem {
  icon: ReactNode;
  shortcut?: string;
  run: () => void;
}

const I = { size: 16, stroke: 1.7 } as const;
const VIEWS: { view: View; labelKey: StaticMessageKey; icon: ReactNode; keywords?: string }[] = [
  { view: { kind: "chrome" }, labelKey: "nav.chrome", icon: <IconBrandChrome {...I} />, keywords: "browser desktop environment chrome 浏览器" },
  { view: { kind: "agents" }, labelKey: "palette.agentsAndProviders", icon: <IconRobot {...I} />, keywords: "claude codex opencode pi antigravity account limits sign in 智能体" },
  { view: { kind: "usage" }, labelKey: "nav.usage", icon: <IconChartBar {...I} />, keywords: "cost tokens spend usage 用量" },
  { view: { kind: "mcp" }, labelKey: "palette.extensionsMcp", icon: <IconPlug {...I} />, keywords: "mcp servers extensions plugins 扩展" },
  { view: { kind: "skills" }, labelKey: "palette.extensionsSkills", icon: <IconSparkles {...I} />, keywords: "skills discover extensions 技能" },
];
const TAB_LABEL_KEY: Record<(typeof SETTINGS_TABS)[number], StaticMessageKey> = {
  general: "settings.tab.general",
  network: "settings.tab.network",
  remote: "settings.tab.remote",
  tools: "settings.tab.tools",
  machine: "settings.tab.machine",
  storage: "settings.tab.storage",
  permissions: "settings.tab.permissions",
};

function useEntries(): Entry[] {
  const { t, locale } = useI18n();
  const sessions = useApp((state) => state.sessions);
  const workspaces = useApp((state) => state.workspaces);
  const activity = useActivity();
  return useMemo(() => {
    const now = Date.now();
    const name = new Map(workspaces.map((w) => [w.id, w.name]));
    const waiting = (id: string) => (activity.get(id) === "waiting" ? 1 : 0);
    const tasks: Entry[] = [...sessions]
      .sort((a, b) => waiting(b.id) - waiting(a.id) || lastActivity(b) - lastActivity(a))
      .map((session, index) => ({
        id: `task:${session.id}`,
        label: session.name,
        hint: [name.get(session.workspaceId), waiting(session.id) ? t("nav.needsYou") : shortAge(lastActivity(session), now)].filter(Boolean).join(" · "),
        group: "Tasks",
        keywords: `${sessionKindName(session.agent)}${session.archived ? " archived" : ""}`,
        onSearchOnly: session.archived || index >= 6,
        icon: session.agent ? <IconMessage {...I} /> : <IconTerminal2 {...I} />,
        run: () => uiActions.navigate({ kind: "session", id: session.id }),
      }));
    const active = workspaces.filter((w) => !w.archived);
    return [
      ...tasks,
      { id: "new", label: t("nav.newTask"), shortcut: IS_MAC ? "⌘N" : "Ctrl+Shift+N", group: "New", icon: <IconPlus {...I} />, run: () => uiActions.navigate({ kind: "new" }) },
      ...active.flatMap((w): Entry[] => [
        { id: `new:${w.id}`, label: `${t("nav.newTask")} · ${w.name}`, group: "New", onSearchOnly: true, icon: <IconPlus {...I} />, run: () => uiActions.navigate({ kind: "new", workspaceId: w.id }) },
        { id: `term:${w.id}`, label: `${t("nav.newTerminal")} · ${w.name}`, group: "New", keywords: "shell terminal", onSearchOnly: true, icon: <IconTerminal2 {...I} />, run: () => void openTerminal(w.id) },
      ]),
      ...VIEWS.map(({ view, labelKey, icon, keywords }): Entry => ({ id: `view:${view.kind}`, label: t(labelKey), group: "Go to", keywords, icon, run: () => uiActions.navigate(view) })),
      { id: "settings", label: t("nav.settings"), shortcut: IS_MAC ? "⌘," : "Ctrl+,", group: "Go to", icon: <IconSettings {...I} />, run: () => uiActions.navigate({ kind: "settings" }) },
      ...SETTINGS_TABS.map((tab): Entry => ({ id: `settings:${tab}`, label: t("palette.settingsSuffix", { tab: t(TAB_LABEL_KEY[tab]) }), group: "Go to", onSearchOnly: true, icon: <IconSettings {...I} />, run: () => uiActions.navigate({ kind: "settings", tab }) })),
      { id: "check", label: t("palette.networkCheck"), group: "Go to", keywords: "internet route exit network 网络", onSearchOnly: true, icon: <IconWorld {...I} />, run: uiActions.openNetworkCheck },
      { id: "operations", label: t("nav.operations"), group: "Go to", keywords: "progress log operations", onSearchOnly: true, icon: <IconActivity {...I} />, run: () => uiActions.showOperations(true) },
    ];
  }, [sessions, workspaces, activity, locale]); // eslint-disable-line react-hooks/exhaustive-deps
}

function Body({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const entries = useEntries();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const groups = useMemo(() => rankPalette(entries, query), [entries, query]);
  const flat = useMemo(() => groups.flatMap((group) => group.items), [groups]);
  const at = Math.min(active, Math.max(0, flat.length - 1));
  const groupLabel = (group: string) => group === "Tasks" ? t("palette.group.tasks") : group === "New" ? t("palette.group.new") : t("palette.group.goto");
  useEffect(() => {
    list.current?.querySelector(`[data-index="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at, groups]);

  // Close first: what runs next may open its own dialog.
  const run = (entry: Entry | undefined) => {
    if (!entry) return;
    onClose();
    entry.run();
  };
  let index = -1;
  return (
    <div onKeyDown={(event) => {
      if (event.nativeEvent.isComposing) return;
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        if (flat.length) setActive((at + (event.key === "ArrowDown" ? 1 : -1) + flat.length) % flat.length);
      } else if (event.key === "Enter") {
        event.preventDefault();
        if (!event.repeat) run(flat[at]);
      }
    }}>
      <label className="flex h-12 items-center gap-3 border-b border-line px-4 text-faint">
        <IconSearch size={17} className="shrink-0" />
        <input
          data-autofocus
          className="min-w-0 flex-1 border-0 bg-transparent font-[inherit] text-[15px] text-fg outline-none placeholder:text-faint"
          placeholder={t("palette.placeholder")}
          value={query}
          onChange={(event) => { setQuery(event.currentTarget.value); setActive(0); }}
          role="combobox" aria-expanded aria-controls="palette-list" aria-label={t("palette.placeholder")}
          aria-activedescendant={flat[at] ? `palette-${at}` : undefined}
        />
      </label>
      <div ref={list} id="palette-list" role="listbox" aria-label="Results" className="max-h-[min(52vh,420px)] overflow-y-auto overscroll-contain p-1.5">
        {groups.map((group) => (
          <div key={group.group} role="group" aria-label={groupLabel(group.group)} className="[&+&]:mt-1">
            <div className="px-2.5 pt-2 pb-1 text-[11px] font-semibold tracking-wide text-faint uppercase">{groupLabel(group.group)}</div>
            {group.items.map((entry) => {
              const i = ++index;
              const selected = i === at;
              return (
                <div
                  key={entry.id} id={`palette-${i}`} data-index={i} role="option" aria-selected={selected}
                  className={`press flex h-9 cursor-pointer items-center gap-3 rounded-lg px-2.5 text-[13.5px] ${selected ? "bg-sel font-medium text-fg" : "text-fg"}`}
                  onMouseMove={() => !selected && setActive(i)}
                  onClick={() => run(entry)}
                >
                  <span className={`grid size-5 shrink-0 place-items-center transition-colors duration-100 ${selected ? "text-accent" : "text-muted"}`}>{entry.icon}</span>
                  <span className="min-w-0 flex-1 truncate">{entry.label}</span>
                  {entry.hint && <span className="max-w-[45%] shrink-0 truncate text-xs font-normal text-faint">{entry.hint}</span>}
                  {entry.shortcut && <kbd className="shrink-0 rounded border border-b-2 border-line bg-surface px-1.5 py-px font-mono text-[10.5px] font-normal text-muted">{entry.shortcut}</kbd>}
                </div>
              );
            })}
          </div>
        ))}
        {flat.length === 0 && <p className="m-0 px-2.5 py-6 text-center text-sm text-faint">{t("palette.noMatches", { query: query.trim() })}</p>}
      </div>
      <div className="flex items-center justify-between gap-4 border-t border-line/70 px-4 py-2 text-[11px] text-faint">
        <span className="flex items-center gap-3">
          <span><kbd className="mr-1 rounded border border-line bg-surface px-1 py-px font-mono text-[10px] text-muted">↑↓</kbd>{t("palette.navigate")}</span>
          <span><kbd className="mr-1 rounded border border-line bg-surface px-1 py-px font-mono text-[10px] text-muted">↵</kbd>{t("palette.open")}</span>
        </span>
        <span><kbd className="mr-1 rounded border border-line bg-surface px-1 py-px font-mono text-[10px] text-muted">Esc</kbd>{t("palette.close")}</span>
      </div>
    </div>
  );
}

/** Jump to a task, start one, or open a page from the keyboard (⌘K, Ctrl+Shift+K). */
export function CommandPalette() {
  const opened = useUi((ui) => ui.paletteOpen);
  return (
    <Modal opened={opened} onClose={uiActions.closePalette} withCloseButton={false} centered={false} yOffset="14vh" size={580} padding={0} aria-label="Command palette">
      <Body onClose={uiActions.closePalette} />
    </Modal>
  );
}
