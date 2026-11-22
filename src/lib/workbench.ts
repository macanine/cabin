// Pure logic for the session panel (components/workbench): file kinds and
// languages, git decorations for the file tree, preview targets, and the
// fuzzy file filter. Tested in tests/workbench.test.mjs.

import type { GitFileStatus, GitStatus } from "../services/ipc/generated/core";

/** "src/lib/a.ts" → ["src/lib", "a.ts"]. */
export function splitPath(path: string): [string, string] {
  const cut = path.lastIndexOf("/");
  return cut < 0 ? ["", path] : [path.slice(0, cut), path.slice(cut + 1)];
}

const EXT_LANG: Record<string, string> = {
  ts: "typescript", mts: "typescript", cts: "typescript", tsx: "tsx",
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "jsx",
  json: "json", jsonc: "jsonc", json5: "json5",
  md: "markdown", mdx: "mdx", markdown: "markdown",
  rs: "rust", py: "python", go: "go", rb: "ruby", java: "java", kt: "kotlin", kts: "kotlin",
  swift: "swift", c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp", cs: "csharp",
  php: "php", lua: "lua", dart: "dart", scala: "scala", zig: "zig", ex: "elixir", exs: "elixir",
  sh: "shellscript", bash: "shellscript", zsh: "shellscript", fish: "fish", ps1: "powershell",
  html: "html", htm: "html", vue: "vue", svelte: "svelte", astro: "astro",
  css: "css", scss: "scss", sass: "sass", less: "less",
  yml: "yaml", yaml: "yaml", toml: "toml", ini: "ini", xml: "xml", svg: "xml", plist: "xml",
  sql: "sql", graphql: "graphql", gql: "graphql", proto: "proto", prisma: "prisma",
  dockerfile: "docker", tf: "hcl", hcl: "hcl", nix: "nix", diff: "diff", patch: "diff",
};

const NAME_LANG: Record<string, string> = {
  dockerfile: "docker", makefile: "makefile", "cmakelists.txt": "cmake",
  ".gitignore": "ignore", ".dockerignore": "ignore", ".env": "dotenv", "justfile": "just",
};

/** The Shiki language for a file, or "text". */
export function languageFor(path: string): string {
  const name = splitPath(path)[1].toLowerCase();
  if (NAME_LANG[name]) return NAME_LANG[name];
  if (name.startsWith(".env")) return "dotenv";
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return "text";
  return EXT_LANG[name.slice(dot + 1)] ?? "text";
}

export type FileGlyph = "code" | "markdown" | "json" | "image" | "config" | "lock" | "text" | "git";

/** Which icon a file gets in the tree. */
export function fileGlyph(path: string): FileGlyph {
  const name = splitPath(path)[1].toLowerCase();
  if (name.startsWith(".git")) return "git";
  if (name.endsWith(".lock") || name === "package-lock.json" || name === "pnpm-lock.yaml") return "lock";
  if (/\.(png|jpe?g|gif|webp|svg|ico|bmp|avif)$/.test(name)) return "image";
  if (/\.(md|mdx|markdown)$/.test(name)) return "markdown";
  if (/\.(json|jsonc|json5)$/.test(name)) return "json";
  if (/\.(ya?ml|toml|ini|conf|cfg)$/.test(name) || name.startsWith(".env") || name === "dockerfile" || name === "makefile") return "config";
  return languageFor(path) === "text" ? "text" : "code";
}

/** The panel's three groups of changes, in order. Untracked files sit with the
 * unstaged changes, as in an editor's "Changes" group. */
export interface ChangeGroups {
  conflicted: GitFileStatus[];
  staged: GitFileStatus[];
  changes: GitFileStatus[];
}

export function changeGroups(status: GitStatus | null): ChangeGroups {
  if (!status?.repository) return { conflicted: [], staged: [], changes: [] };
  return {
    conflicted: status.conflicted,
    staged: status.staged,
    changes: [...status.unstaged, ...status.untracked].sort((a, b) => a.path.localeCompare(b.path)),
  };
}

/** Distinct paths with any change (the badge on the Changes tab). */
export function changeCount(status: GitStatus | null): number {
  if (!status?.repository) return 0;
  const paths = new Set<string>();
  for (const list of [status.conflicted, status.staged, status.unstaged, status.untracked]) {
    for (const file of list) paths.add(file.path);
  }
  return paths.size;
}

export type Decoration = "modified" | "added" | "deleted" | "untracked" | "conflict" | "renamed";

const RANK: Record<Decoration, number> = { conflict: 5, deleted: 4, modified: 3, renamed: 2, added: 1, untracked: 0 };

function decorationOf(file: GitFileStatus): Decoration {
  switch (file.status) {
    case "U": return "conflict";
    case "?": return "untracked";
    case "A": return "added";
    case "D": return "deleted";
    case "R": case "C": return "renamed";
    default: return "modified";
  }
}

/** Git colour for every changed path, and for each folder above one (the
 * strongest change inside wins), like an editor's explorer. */
export function gitDecorations(status: GitStatus | null): Map<string, Decoration> {
  const map = new Map<string, Decoration>();
  if (!status?.repository) return map;
  const put = (path: string, decoration: Decoration) => {
    const old = map.get(path);
    if (!old || RANK[decoration] > RANK[old]) map.set(path, decoration);
  };
  for (const list of [status.untracked, status.staged, status.unstaged, status.conflicted]) {
    for (const file of list) {
      const decoration = decorationOf(file);
      put(file.path, decoration);
      let dir = splitPath(file.path)[0];
      while (dir) {
        put(dir, decoration);
        dir = splitPath(dir)[0];
      }
    }
  }
  return map;
}

/** The single letter shown for a change ("M", "A", "D", "R", "U" untracked, "!" conflict). */
export function statusLetter(file: GitFileStatus): string {
  if (file.status === "?") return "U";
  if (file.status === "U") return "!";
  if (file.status === "T") return "M";
  return file.status;
}

export const STATUS_LABEL: Record<string, string> = {
  M: "Modified", A: "Added", D: "Deleted", R: "Renamed", C: "Copied", T: "Type changed", U: "Conflict", "?": "Untracked",
};

/** What the preview address bar was given: a port and a path on the VM's localhost. */
export interface PreviewTarget { port: number; path: string }

/**
 * Read "3000", ":3000/docs", "localhost:5173", "http://127.0.0.1:8080/a?b"
 * or "/next/path" (with the current port) as a target on the VM's loopback.
 * Other hosts are refused: the preview reaches the VM's own servers only.
 */
export function parsePreviewTarget(input: string, currentPort: number | null): PreviewTarget | null {
  const text = input.trim();
  if (!text) return null;
  if (/^\d{1,5}$/.test(text)) return validPort(Number(text), "/");
  if (text.startsWith("/")) return currentPort == null ? null : { port: currentPort, path: text };
  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(text) ? text : `http://${text.startsWith(":") ? `localhost${text}` : text}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  const host = url.hostname.replace(/^\[|\]$/g, "");
  if (!["localhost", "127.0.0.1", "0.0.0.0", "::1", "::"].includes(host)) return null;
  const port = url.port ? Number(url.port) : url.protocol === "https:" ? 443 : 80;
  return validPort(port, `${url.pathname || "/"}${url.search}${url.hash}`);
}

function validPort(port: number, path: string): PreviewTarget | null {
  return Number.isInteger(port) && port > 0 && port < 65536 ? { port, path } : null;
}

/** How the address bar shows a target. */
export function previewAddress(target: PreviewTarget): string {
  return `localhost:${target.port}${target.path === "/" ? "" : target.path}`;
}

/**
 * Subsequence match of `query` in `path`, scored so that consecutive runs,
 * matches at word starts and in the file name rank first. null: no match.
 */
export function fuzzyScore(query: string, path: string): number | null {
  const q = query.toLowerCase().replace(/\s+/g, "");
  if (!q) return 0;
  const p = path.toLowerCase();
  const nameStart = p.lastIndexOf("/") + 1;
  // Greedy from the start of the path, and again from the file name: the
  // leftmost match alone would spend "c" on "src/" before reaching "Chat".
  const scores = [greedy(q, p, 0, nameStart), greedy(q, p, nameStart, nameStart)].filter((s): s is number => s != null);
  if (!scores.length) return null;
  let score = Math.max(...scores);
  const name = p.slice(nameStart);
  if (name.startsWith(q)) score += 12;
  else if (name.includes(q)) score += 6;
  // Shorter paths win ties.
  return score - path.length * 0.01;
}

function greedy(q: string, p: string, from: number, nameStart: number): number | null {
  let score = 0;
  let at = from;
  let previous = -2;
  for (const char of q) {
    const found = p.indexOf(char, at);
    if (found < 0) return null;
    const boundary = found === 0 || "/-_. ".includes(p[found - 1]);
    score += 1;
    if (found === previous + 1) score += 4;
    if (boundary) score += 3;
    if (found >= nameStart) score += 2;
    previous = found;
    at = found + 1;
  }
  return score;
}

export function fuzzyFilter(query: string, paths: readonly string[], limit = 200): string[] {
  return paths
    .map((path) => ({ path, score: fuzzyScore(query, path) }))
    .filter((entry): entry is { path: string; score: number } => entry.score != null)
    .sort((a, b) => b.score - a.score)
    .slice(0, limit)
    .map((entry) => entry.path);
}

/** Bytes as "512 B", "4.2 KB", "1.3 MB" (English, like lib/format.ts). */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
