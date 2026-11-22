import { invokeGuest } from "./common";
import type {
  AgentKind, DiscoveredSkill, McpChange, McpLibrary, McpServer, OperationRecord,
  SkillChange, SkillFile, SkillInstall, SkillLibrary, SkillMode, SkillRegistry, SkillRepository,
} from "./generated/core";

export const mcpIpc = {
  list: () => invokeGuest<McpLibrary>("mcp_library"),
  edit: (name: string) => invokeGuest<McpServer>("mcp_library_edit", { name }),
  export: (name: string | null = null) => invokeGuest<string>("mcp_library_export", { name }),
  parse: (text: string) => invokeGuest<McpServer[]>("mcp_library_parse", { text }),
  change: (change: McpChange) => invokeGuest<OperationRecord>("mcp_library_change", change),
};

export const skillsIpc = {
  list: () => invokeGuest<SkillLibrary>("skill_library"),
  read: (folder: string) => invokeGuest<SkillFile>("skill_library_read", { folder }),
  repositories: (repositories: SkillRepository[]) => invokeGuest<SkillRepository[]>("skill_library_repositories", { repositories }),
  discover: (repository: SkillRepository) => invokeGuest<DiscoveredSkill[]>("skill_library_discover", repository),
  registry: (query: string, offset: number) => invokeGuest<SkillRegistry>("skill_library_registry", { query, offset }),
  install: (source: SkillInstall, targets: AgentKind[], mode: SkillMode = "link") => invokeGuest<OperationRecord>("skill_library_install", { source, targets, mode }),
  change: (change: SkillChange) => invokeGuest<OperationRecord>("skill_library_change", change),
  updates: () => invokeGuest<string[]>("skill_library_updates"),
};
