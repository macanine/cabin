import { useEffect, useState } from "react";
import type { AgentChatCapabilities, AgentKind } from "../services/ipc/generated/core";
import { agentsIpc } from "../services/ipc/agents";

/**
 * The single lookup for what a chat with an agent can do. The guest runtime
 * declares it (permission modes, runtime); components ask here instead of
 * comparing agent names.
 */
const NONE = (agent: AgentKind): AgentChatCapabilities => ({ agent, permissionModes: [] });

let cache: Promise<AgentChatCapabilities[]> | null = null;
const load = () => (cache ??= agentsIpc.chatCapabilities().catch((error) => { cache = null; throw error; }));

export function useChatCapabilities(agent: AgentKind): AgentChatCapabilities {
  const [value, setValue] = useState<AgentChatCapabilities>(NONE(agent));
  useEffect(() => {
    let live = true;
    setValue(NONE(agent));
    load().then((all) => { if (live) setValue(all.find((entry) => entry.agent === agent) ?? NONE(agent)); }).catch(() => {});
    return () => { live = false; };
  }, [agent]);
  return value;
}
