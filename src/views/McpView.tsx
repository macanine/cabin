import type { AgentKind } from "../services/ipc/generated/core";
import { ExtensionsView } from "./ExtensionsView";

export function McpView({ agent }: { agent?: AgentKind }) {
  return <ExtensionsView initialTab="mcp" agent={agent} />;
}
