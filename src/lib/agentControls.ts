// What the live agent reports about its session (`ChatControls`), shaped for
// the composer. Everything here comes from the agent over ACP; nothing is
// chosen from an agent's name.
import type {
  AgentCommand, ChatConfigOption, ChatContextUse, ChatControls, ChatPlanEntry,
  ChatRuntimeCapabilities,
} from "../services/ipc/generated/core";

export const EMPTY_CAPABILITIES: ChatRuntimeCapabilities = {
  prompt: { images: false, audio: false, embeddedContext: false },
  steer: false,
  resume: false,
  load: false,
  fork: false,
  list: false,
  plans: false,
  subagents: false,
  asyncTasks: false,
  notices: false,
  compaction: false,
  fileRead: false,
  fileWrite: false,
  terminals: false,
  goal: false,
};

export const EMPTY_CONTROLS: ChatControls = {
  agentName: null,
  agentVersion: null,
  options: [],
  commands: [],
  plan: [],
  context: null,
  title: null,
  suggestions: [],
  canSteer: false,
  images: false,
  capabilities: EMPTY_CAPABILITIES,
  goal: null,
};

/** The agent's live slash commands as composer menu entries. Codex advertises
 * a skill as a command literally named `$skill`; that `$` is its trigger, so
 * it is inserted as-is rather than prefixed with `/`. */
export function liveCommands(controls: ChatControls): AgentCommand[] {
  return controls.commands.map((command) => ({
    name: command.name,
    description: command.description || null,
    hint: command.hint,
    kind: "command",
    source: "agent",
    insert: command.name.startsWith("$") ? `${command.name} ` : `/${command.name} `,
  }));
}

/** The catalog read from disk plus the agent's live commands; the live entry
 * wins when both name the same command. A disk `$` skill stays even when the
 * agent advertises the same token as a command: the `/` menu lists the command
 * and the `$` menu still lists the skill, as codeg does. */
export function mergeCommands(catalog: AgentCommand[], live: AgentCommand[]): AgentCommand[] {
  const seen = new Set(live.map((command) => command.insert.trim()));
  return [
    ...live,
    ...catalog.filter((command) =>
      !seen.has(command.insert.trim())
      || (command.kind === "skill" && command.insert.startsWith("$"))
    ),
  ];
}

/** Selectors shown next to the model picker: everything but the model, which
 * has its own picker, and selectors with nothing to choose. */
export function sessionOptions(controls: ChatControls): ChatConfigOption[] {
  return controls.options.filter(
    (option) => !option.hidden && option.category !== "model" && (option.kind === "boolean" || option.choices.length > 1),
  );
}

/** The agent's live model selector, when it reports one. */
export function liveModel(controls: ChatControls): ChatConfigOption | null {
  return controls.options.find((option) => option.category === "model" && option.choices.length > 0) ?? null;
}

/** The reasoning/thinking-depth option, when the agent exposes one. Its
 * choices keep the agent's order and become the slider's stops. */
export function reasoningOption(options: ChatConfigOption[]): ChatConfigOption | null {
  const usable = (option: ChatConfigOption) => option.kind !== "boolean" && option.choices.length > 1;
  return options.find((option) => option.category === "thought_level" && usable(option))
    ?? options.find((option) =>
      usable(option) && /thought|reason|thinking|effort/i.test(`${option.category ?? ""} ${option.id} ${option.name}`),
    )
    ?? null;
}

/** The display name of an option's current value. */
export function currentLabel(option: ChatConfigOption): string {
  if (option.kind === "boolean") return option.current === "true" ? "On" : "Off";
  return option.choices.find((choice) => choice.value === option.current)?.name || option.current || option.name;
}

/** Choices grouped for a menu, in the agent's order. */
export function groupedChoices(option: ChatConfigOption): { group: string | null; choices: ChatConfigOption["choices"] }[] {
  const groups: { group: string | null; choices: ChatConfigOption["choices"] }[] = [];
  for (const choice of option.choices) {
    const last = groups[groups.length - 1];
    if (last && last.group === choice.group) last.choices.push(choice);
    else groups.push({ group: choice.group, choices: [choice] });
  }
  return groups;
}

export interface PlanProgress {
  done: number;
  total: number;
  /** The entry in progress, else the first pending one. */
  current: ChatPlanEntry | null;
}

export function planProgress(plan: ChatPlanEntry[]): PlanProgress {
  const done = plan.filter((entry) => entry.status === "completed").length;
  const current = plan.find((entry) => entry.status === "in_progress")
    ?? plan.find((entry) => entry.status === "pending")
    ?? null;
  return { done, total: plan.length, current };
}

/** Share of the context window in use, 0–1, or null when unknown. */
export function contextFill(context: ChatContextUse | null): number | null {
  if (!context || context.size <= 0) return null;
  return Math.min(1, Math.max(0, context.used / context.size));
}

export interface QueuedMessage {
  id: string;
  text: string;
}

let queued = 0;

export function queueMessage(queue: QueuedMessage[], text: string): QueuedMessage[] {
  const trimmed = text.trim();
  if (!trimmed) return queue;
  queued += 1;
  return [...queue, { id: `queued-${Date.now()}-${queued}`, text: trimmed }];
}

/** The first queued message and the rest, once the agent is idle again. */
export function takeQueued(queue: QueuedMessage[]): [QueuedMessage | null, QueuedMessage[]] {
  return queue.length ? [queue[0], queue.slice(1)] : [null, queue];
}
