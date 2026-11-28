import { marked, type Token, type Tokens } from "marked";

export interface MarkdownBlock {
  /** Stable key across streaming updates for completed blocks. */
  key: string;
  /** Raw markdown slice for this block, used by React.memo equality. */
  raw: string;
  token: Token;
}

export type ClassifiedLink =
  | { kind: "external"; href: string }
  | { kind: "anchor"; href: string }
  | {
      kind: "file";
      href: string;
      /** Decoded filesystem path (POSIX-style). */
      path: string;
      /** Relative path inside the workspace when resolvable. */
      relativePath: string | null;
      line: number | null;
      endLine: number | null;
      /** Compact label for tooltips. */
      displayPath: string;
    }
  | { kind: "unsafe"; href: string };

const HTML_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: "\u00a0",
};

/** Decode HTML entities emitted by marked's inline tokenizer for safe React text rendering. */
export function decodeHtmlEntities(input: string): string {
  if (!input.includes("&")) return input;
  return input.replace(/&(#x[0-9a-f]+|#[0-9]+|[a-z]+);/gi, (match, body: string) => {
    const lower = body.toLowerCase();
    if (lower in HTML_ENTITIES) return HTML_ENTITIES[lower];
    if (lower.startsWith("#x")) {
      const code = Number.parseInt(lower.slice(2), 16);
      return Number.isFinite(code) && code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : match;
    }
    if (lower.startsWith("#")) {
      const code = Number.parseInt(lower.slice(1), 10);
      return Number.isFinite(code) && code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : match;
    }
    return match;
  });
}

/**
 * Repair incomplete markdown syntax while an agent response is actively streaming
 * so half-written fences, inline code, links, or emphasis render cleanly without
 * flashing raw delimiters.
 */
export function repairStreamingMarkdown(input: string): string {
  if (!input) return "";
  let text = input.replace(/\r\n?/g, "\n");

  // 1. Track fenced code blocks line-by-line (``` or ~~~).
  let openFenceChar: "`" | "~" | null = null;
  let openFenceLen = 0;
  const lines = text.split("\n");
  for (const line of lines) {
    const trimmed = line.replace(/^ {0,3}/, "");
    if (!openFenceChar) {
      const open = trimmed.match(/^(`{3,}|~{3,})/);
      if (open) {
        const marker = open[1];
        const rest = trimmed.slice(marker.length);
        // Backtick info strings cannot contain backticks per CommonMark.
        if (marker[0] !== "`" || !rest.includes("`")) {
          openFenceChar = marker[0] as "`" | "~";
          openFenceLen = marker.length;
        }
      }
    } else {
      const close = new RegExp(`^${openFenceChar}{${openFenceLen},}[ \\t]*$`);
      if (close.test(trimmed)) {
        openFenceChar = null;
        openFenceLen = 0;
      }
    }
  }
  if (openFenceChar) {
    const suffix = text.endsWith("\n") ? "" : "\n";
    return `${text}${suffix}${openFenceChar.repeat(openFenceLen)}`;
  }

  // 2. Inspect only the trailing paragraph (outside fences) for unclosed inline tokens.
  const paraBreak = text.lastIndexOf("\n\n");
  const head = paraBreak === -1 ? "" : text.slice(0, paraBreak + 2);
  let tail = paraBreak === -1 ? text : text.slice(paraBreak + 2);

  // Close an unclosed inline code span (matching backtick run length).
  let openTickLen = 0;
  for (let i = 0; i < tail.length; ) {
    if (tail[i] === "\\" && openTickLen === 0) {
      i += 2;
      continue;
    }
    if (tail[i] === "`") {
      let j = i;
      while (j < tail.length && tail[j] === "`") j++;
      const run = j - i;
      if (openTickLen === 0) openTickLen = run;
      else if (run === openTickLen) openTickLen = 0;
      i = j;
      continue;
    }
    i++;
  }
  if (openTickLen > 0) {
    return `${head}${tail}${"`".repeat(openTickLen)}`;
  }

  // Strip/complete a half-typed markdown link or image at the very end: `[label](partial` or `![alt](partial`.
  const openLink = tail.match(/(!?)\[([^\]\n]*)\]\(([^)\n]*)$/);
  if (openLink) {
    const [full, bang, label] = openLink;
    const prefix = tail.slice(0, tail.length - full.length);
    // Keep the readable label while URL is still arriving (no raw `](` flash).
    tail = bang ? prefix : `${prefix}${label}`;
  } else {
    // Close dangling strong `**` or strikethrough `~~` on the last line if paired count is odd.
    const lastNl = tail.lastIndexOf("\n");
    const beforeLine = lastNl === -1 ? "" : tail.slice(0, lastNl + 1);
    let lastLine = lastNl === -1 ? tail : tail.slice(lastNl + 1);

    const stripped = lastLine.replace(/`[^`]*`/g, "");
    const strongCount = (stripped.match(/(?<!\\)\*\*/g) ?? []).length;
    if (strongCount % 2 === 1 && !lastLine.endsWith("**")) {
      lastLine += "**";
    }
    const strikeCount = (stripped.match(/(?<!\\)~~/g) ?? []).length;
    if (strikeCount % 2 === 1 && !lastLine.endsWith("~~")) {
      lastLine += "~~";
    }
    tail = beforeLine + lastLine;
  }

  text = head + tail;
  return text;
}

/** Tokenize markdown into top-level blocks with stable keys for memoized rendering. */
export function parseMarkdownBlocks(text: string, streaming: boolean): MarkdownBlock[] {
  if (!text) return [];
  const prepared = streaming ? repairStreamingMarkdown(text) : text;
  const tokens = marked.lexer(prepared, { gfm: true, breaks: false });
  const blocks: MarkdownBlock[] = [];
  let index = 0;
  for (const token of tokens) {
    if (token.type === "space" || token.type === "def") continue;
    blocks.push({
      key: `${index}:${token.type}`,
      raw: token.raw,
      token,
    });
    index++;
  }
  return blocks;
}

function parseLineHash(hash: string): { line: number | null; endLine: number | null } {
  if (!hash) return { line: null, endLine: null };
  // Matches #L10, #L10-L20, #L10-20, #10, #10-20
  const m = hash.match(/^#?L?(\d+)(?:-L?(\d+))?$/i);
  if (!m) return { line: null, endLine: null };
  const line = Number.parseInt(m[1], 10);
  const endLine = m[2] ? Number.parseInt(m[2], 10) : null;
  return {
    line: Number.isFinite(line) ? line : null,
    endLine: endLine != null && Number.isFinite(endLine) ? endLine : null,
  };
}

function stripLeadingSlashForWindows(path: string): string {
  // file:///C:/foo -> C:/foo
  return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path;
}

function normalizePosix(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+/g, "/").replace(/\/$/, "");
}

function resolveRelativePath(filePath: string, workspacePath?: string | null): string | null {
  const cleanFile = normalizePosix(filePath);
  if (!cleanFile.startsWith("/") && !/^[A-Za-z]:\//.test(cleanFile)) {
    const stripped = cleanFile.replace(/^\.\//, "");
    return stripped && !stripped.startsWith("..") ? stripped : null;
  }
  if (workspacePath) {
    const cleanRoot = normalizePosix(workspacePath);
    if (cleanRoot && cleanFile.startsWith(`${cleanRoot}/`)) {
      return cleanFile.slice(cleanRoot.length + 1);
    }
  }
  return null;
}

function formatDisplayPath(path: string, relativePath: string | null, line: number | null, endLine: number | null): string {
  const base = relativePath ?? path;
  if (line == null) return base;
  return endLine != null && endLine !== line ? `${base}:${line}-${endLine}` : `${base}:${line}`;
}

/**
 * Classify a markdown link destination safely without blocking workspace `file://`
 * or relative file references.
 */
export function classifyLink(rawHref: string, workspacePath?: string | null): ClassifiedLink {
  const href = rawHref.trim();
  if (!href) return { kind: "unsafe", href };

  if (href.startsWith("#")) {
    return { kind: "anchor", href };
  }

  const schemeMatch = href.match(/^([a-zA-Z][a-zA-Z0-9+.-]*):/);
  if (schemeMatch) {
    const scheme = schemeMatch[1].toLowerCase();
    if (scheme === "http" || scheme === "https" || scheme === "mailto") {
      return { kind: "external", href };
    }
    if (scheme === "file") {
      let decodedPath: string;
      let hash: string;
      try {
        const parsed = new URL(href);
        decodedPath = decodeURIComponent(parsed.pathname);
        hash = parsed.hash;
      } catch {
        const withoutScheme = href.replace(/^file:\/\//i, "");
        const hashIndex = withoutScheme.indexOf("#");
        const rawPath = hashIndex === -1 ? withoutScheme : withoutScheme.slice(0, hashIndex);
        hash = hashIndex === -1 ? "" : withoutScheme.slice(hashIndex);
        try {
          decodedPath = decodeURIComponent(rawPath);
        } catch {
          decodedPath = rawPath;
        }
      }
      const path = stripLeadingSlashForWindows(decodedPath);
      const { line, endLine } = parseLineHash(hash);
      const relativePath = resolveRelativePath(path, workspacePath);
      return {
        kind: "file",
        href,
        path,
        relativePath,
        line,
        endLine,
        displayPath: formatDisplayPath(path, relativePath, line, endLine),
      };
    }
    // Block javascript:, vbscript:, data:, etc. quietly (rendered as plain text, never "[blocked]").
    return { kind: "unsafe", href };
  }

  // Relative or bare local file path (e.g. `src/worker.ts`, `./src/crypto.ts#L12`, `/abs/path.ts`).
  const hashIndex = href.indexOf("#");
  const rawPath = hashIndex === -1 ? href : href.slice(0, hashIndex);
  const hash = hashIndex === -1 ? "" : href.slice(hashIndex);
  let path: string;
  try {
    path = decodeURIComponent(rawPath);
  } catch {
    path = rawPath;
  }
  const { line, endLine } = parseLineHash(hash);
  const relativePath = resolveRelativePath(path, workspacePath);
  return {
    kind: "file",
    href,
    path,
    relativePath,
    line,
    endLine,
    displayPath: formatDisplayPath(path, relativePath, line, endLine),
  };
}

/** Allow http(s) and safe raster/SVG data image URLs matching Cabin's CSP. */
export function sanitizeImageSrc(rawSrc: string): string | null {
  const src = rawSrc.trim();
  if (!src) return null;
  if (/^https?:\/\//i.test(src)) return src;
  if (/^data:image\/(png|jpeg|jpg|gif|webp|avif);base64,[a-z0-9+/=\s]+$/i.test(src)) return src;
  return null;
}

export type { Token, Tokens };
