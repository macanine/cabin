/**
 * Shared lazy Shiki syntax highlighter with a bounded LRU cache so code blocks
 * in chat and workbench stay instant across re-renders and theme switches.
 */
type CodeToHtml = typeof import("shiki")["codeToHtml"];

const MAX_CACHE = 240;
const MAX_HIGHLIGHT_BYTES = 180_000;
const MAX_HIGHLIGHT_LINES = 4000;

const LANG_ALIASES: Record<string, string> = {
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  py: "python",
  rs: "rust",
  sh: "bash",
  zsh: "bash",
  shell: "bash",
  console: "bash",
  yml: "yaml",
  md: "markdown",
  rb: "ruby",
  kt: "kotlin",
  cs: "csharp",
  "c++": "cpp",
  "c#": "csharp",
  dockerfile: "docker",
  plaintext: "text",
  plain: "text",
  txt: "text",
};

let shikiPromise: Promise<CodeToHtml> | null = null;
const cache = new Map<string, string>();

function loadShiki(): Promise<CodeToHtml> {
  return (shikiPromise ??= import("shiki").then((module) => module.codeToHtml));
}

export function normalizeCodeLang(raw?: string | null): string {
  const cleaned = (raw ?? "").trim().toLowerCase().split(/\s+/)[0] ?? "";
  if (!cleaned) return "text";
  return LANG_ALIASES[cleaned] ?? cleaned;
}

function cacheKey(lang: string, code: string): string {
  return `${lang}\0${code}`;
}

export function cachedHighlight(code: string, rawLang?: string | null): string | null {
  const lang = normalizeCodeLang(rawLang);
  const key = cacheKey(lang, code);
  const hit = cache.get(key);
  if (hit !== undefined) {
    // Refresh LRU recency.
    cache.delete(key);
    cache.set(key, hit);
    return hit;
  }
  return null;
}

export async function highlightCode(code: string, rawLang?: string | null): Promise<string | null> {
  const lang = normalizeCodeLang(rawLang);
  if (!code || lang === "text" || code.length > MAX_HIGHLIGHT_BYTES) return null;
  if (code.split("\n").length > MAX_HIGHLIGHT_LINES) return null;

  const existing = cachedHighlight(code, lang);
  if (existing !== null) return existing;

  const codeToHtml = await loadShiki();
  const options = {
    themes: { light: "github-light", dark: "github-dark" },
    defaultColor: false as const,
  };
  let html: string;
  try {
    html = await codeToHtml(code, { ...options, lang });
  } catch {
    html = await codeToHtml(code, { ...options, lang: "text" });
  }

  const key = cacheKey(lang, code);
  cache.set(key, html);
  if (cache.size > MAX_CACHE) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return html;
}
