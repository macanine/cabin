import type { AgentKind } from "../services/ipc/generated/core";
import { ExtensionsView } from "./ExtensionsView";

export function SkillsView({ agent }: { agent?: AgentKind }) {
  return <ExtensionsView initialTab="skills" agent={agent} />;
}
