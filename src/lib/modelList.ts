import type { AgentModel } from "../services/ipc/generated/core";

export interface ModelGroup {
  /** The provider; null for models without one. */
  provider: string | null;
  models: AgentModel[];
}

/** Rows rendered at once; more are reached by searching. */
export const MODEL_ROW_LIMIT = 150;

/** Every word of the query must appear in the model's name, id or provider. */
export function matchesModel(model: AgentModel, query: string): boolean {
  const words = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const haystack = `${model.name} ${model.id} ${model.provider ?? ""}`.toLocaleLowerCase();
  return words.every((word) => haystack.includes(word));
}

/**
 * Matching models grouped by provider, providers in first-seen order (the
 * guest lists the default first), at most `limit` rows in all.
 */
export function groupModels(models: AgentModel[], query: string, limit = MODEL_ROW_LIMIT): { groups: ModelGroup[]; shown: number; total: number } {
  const matching = models.filter((model) => matchesModel(model, query));
  const all: ModelGroup[] = [];
  const byProvider = new Map<string | null, ModelGroup>();
  for (const model of matching) {
    const provider = model.provider ?? null;
    let group = byProvider.get(provider);
    if (!group) {
      group = { provider, models: [] };
      byProvider.set(provider, group);
      all.push(group);
    }
    group.models.push(model);
  }
  // Trim in display order, so the cut never splits the list unexpectedly.
  const groups: ModelGroup[] = [];
  let shown = 0;
  for (const group of all) {
    if (shown >= limit) break;
    const take = group.models.slice(0, limit - shown);
    groups.push({ provider: group.provider, models: take });
    shown += take.length;
  }
  return { groups, shown, total: matching.length };
}

/** The flat order the keyboard walks through. */
export const flatModels = (groups: ModelGroup[]) => groups.flatMap((group) => group.models);

/** The button's label: the model's name, else its id after any `provider/`. */
export function modelLabel(value: string | null, models: AgentModel[]): string | null {
  if (!value) return null;
  const known = models.find((model) => model.id === value);
  if (known?.name) return known.name;
  const slash = value.indexOf("/");
  return slash > 0 && slash < value.length - 1 ? value.slice(slash + 1) : value;
}

/** A typed query that is not a listed model can be used as a custom id. */
export function customCandidate(query: string, models: AgentModel[]): string | null {
  const id = query.trim();
  if (!id || /\s/.test(id)) return null;
  return models.some((model) => model.id === id) ? null : id;
}
