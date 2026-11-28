import { IconCheck, IconCopy, IconFileCode } from "@tabler/icons-react";
import { Fragment, memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  classifyLink,
  decodeHtmlEntities,
  parseMarkdownBlocks,
  sanitizeImageSrc,
  type ClassifiedLink,
  type MarkdownBlock,
  type Token,
  type Tokens,
} from "../../lib/markdown";
import { cachedHighlight, highlightCode, normalizeCodeLang } from "../../lib/shiki";
import { getStreamReveal } from "../../lib/streamReveal";
import { clipboard } from "../../services/ipc/dialogs";
import { machineIpc } from "../../services/ipc/app";
import { uiActions } from "../../state/ui";
import { openWorkspaceFile } from "../workbench/mention";
import { useChat } from "./context";

interface LinkHandlerContext {
  sessionId: string;
  workspacePath: string | null;
}

function handleExternalClick(href: string) {
  void machineIpc.fix({ kind: "openUrl", url: href }).catch(() => {
    window.open(href, "_blank", "noopener,noreferrer");
  });
}

function handleFileLinkClick(
  link: Extract<ClassifiedLink, { kind: "file" }>,
  labelText: string,
  ctx: LinkHandlerContext,
) {
  // Prefer the resolved workspace-relative path; fall back to a clean relative label (e.g. `src/worker.ts`).
  const labelCandidate = labelText.trim().replace(/^\.\//, "");
  const looksRelative = Boolean(
    labelCandidate
    && !labelCandidate.includes(" ")
    && !labelCandidate.startsWith("/")
    && !labelCandidate.startsWith("..")
    && labelCandidate.includes("/"),
  );
  const relativePath = link.relativePath ?? (looksRelative ? labelCandidate.replace(/:(\d+)(?:-\d+)?$/, "") : null);

  if (ctx.sessionId && relativePath) {
    uiActions.openPanel("files");
    openWorkspaceFile(ctx.sessionId, relativePath, link.line);
    return;
  }

  void clipboard.write(link.displayPath).then(() => {
    uiActions.toast("Path copied", "info", { detail: link.displayPath });
  }).catch(() => undefined);
}

/** Fenced code block with lazy Shiki dual-theme highlighting and instant plain fallback. */
const CodeBlock = memo(function CodeBlock({
  code,
  lang,
  streaming,
}: {
  code: string;
  lang: string;
  streaming: boolean;
}) {
  const displayLang = (lang || "").trim().split(/\s+/)[0] || "";
  const normalized = normalizeCodeLang(displayLang);
  const [html, setHtml] = useState<string | null>(() => cachedHighlight(code, normalized));
  const [copied, setCopied] = useState(false);
  const copyTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => () => clearTimeout(copyTimer.current), []);

  useEffect(() => {
    const cached = cachedHighlight(code, normalized);
    if (cached !== null) {
      setHtml(cached);
      return;
    }
    if (normalized === "text") {
      setHtml(null);
      return;
    }
    let cancelled = false;
    // Throttle Shiki work slightly while this block is actively streaming.
    const delay = streaming ? 160 : 0;
    const timer = window.setTimeout(() => {
      void highlightCode(code, normalized).then((out) => {
        if (!cancelled && out !== null) setHtml(out);
      }).catch(() => undefined);
    }, delay);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [code, normalized, streaming]);

  function copy() {
    void clipboard.write(code).then(() => {
      setCopied(true);
      clearTimeout(copyTimer.current);
      copyTimer.current = setTimeout(() => setCopied(false), 1500);
    }).catch(() => undefined);
  }

  return (
    <div className="md-code-block" data-md-code-block="">
      <div className="md-code-header">
        <span className="md-code-lang">{displayLang || "text"}</span>
        <button
          type="button"
          className="md-code-copy"
          onClick={copy}
          aria-label={copied ? "Copied" : "Copy code"}
          title={copied ? "Copied" : "Copy code"}
        >
          {copied ? <IconCheck size={14} className="text-ok" /> : <IconCopy size={14} />}
        </button>
      </div>
      <div className="md-code-body">
        {html ? (
          <div className="md-code-shiki" dangerouslySetInnerHTML={{ __html: html }} />
        ) : (
          <pre><code>{code}</code></pre>
        )}
      </div>
    </div>
  );
});

function extractPlainText(tokens?: Token[]): string {
  if (!tokens?.length) return "";
  let out = "";
  for (const token of tokens) {
    if ("tokens" in token && Array.isArray(token.tokens)) {
      out += extractPlainText(token.tokens);
    } else if ("text" in token && typeof token.text === "string") {
      out += decodeHtmlEntities(token.text);
    }
  }
  return out;
}

function renderInlineTokens(tokens: Token[] | undefined, ctx: LinkHandlerContext, keyPrefix: string): ReactNode {
  if (!tokens || tokens.length === 0) return null;
  return tokens.map((token, index) => renderInlineToken(token, ctx, `${keyPrefix}.${index}`));
}

function renderInlineToken(token: Token, ctx: LinkHandlerContext, key: string): ReactNode {
  switch (token.type) {
    case "text": {
      const t = token as Tokens.Text;
      if (t.tokens && t.tokens.length > 0) {
        return <Fragment key={key}>{renderInlineTokens(t.tokens, ctx, key)}</Fragment>;
      }
      return <Fragment key={key}>{decodeHtmlEntities(t.text)}</Fragment>;
    }
    case "escape": {
      const t = token as Tokens.Escape;
      return <Fragment key={key}>{decodeHtmlEntities(t.text)}</Fragment>;
    }
    case "codespan": {
      const t = token as Tokens.Codespan;
      return <code key={key} className="md-inline-code">{decodeHtmlEntities(t.text)}</code>;
    }
    case "strong": {
      const t = token as Tokens.Strong;
      return <strong key={key}>{renderInlineTokens(t.tokens, ctx, key)}</strong>;
    }
    case "em": {
      const t = token as Tokens.Em;
      return <em key={key}>{renderInlineTokens(t.tokens, ctx, key)}</em>;
    }
    case "del": {
      const t = token as Tokens.Del;
      return <del key={key}>{renderInlineTokens(t.tokens, ctx, key)}</del>;
    }
    case "br":
      return <br key={key} />;
    case "link": {
      const t = token as Tokens.Link;
      const classified = classifyLink(t.href, ctx.workspacePath);
      const labelText = extractPlainText(t.tokens) || decodeHtmlEntities(t.text);
      const children = t.tokens?.length ? renderInlineTokens(t.tokens, ctx, key) : decodeHtmlEntities(t.text);

      if (classified.kind === "external") {
        return (
          <a
            key={key}
            href={classified.href}
            title={t.title || classified.href}
            className="md-link"
            rel="noopener noreferrer"
            target="_blank"
            onClick={(event) => {
              event.preventDefault();
              handleExternalClick(classified.href);
            }}
          >
            {children}
          </a>
        );
      }

      if (classified.kind === "anchor") {
        return (
          <a key={key} href={classified.href} title={t.title || undefined} className="md-link">
            {children}
          </a>
        );
      }

      if (classified.kind === "file") {
        const hasCodeChild = t.tokens?.length === 1 && t.tokens[0].type === "codespan";
        return (
          <button
            key={key}
            type="button"
            className={`md-file-link${hasCodeChild ? " is-code" : ""}`}
            title={t.title || classified.displayPath}
            onClick={() => handleFileLinkClick(classified, labelText, ctx)}
          >
            {!hasCodeChild && <IconFileCode size={13} className="md-file-link-icon" aria-hidden />}
            {children}
          </button>
        );
      }

      // Unsafe protocol: render the label cleanly as plain text, never "[blocked]".
      return <span key={key}>{children}</span>;
    }
    case "image": {
      const t = token as Tokens.Image;
      const src = sanitizeImageSrc(t.href);
      if (!src) return <span key={key}>{decodeHtmlEntities(t.text || "")}</span>;
      return (
        <img
          key={key}
          src={src}
          alt={decodeHtmlEntities(t.text || "")}
          title={t.title || undefined}
          loading="lazy"
          className="md-image"
        />
      );
    }
    case "html": {
      const t = token as Tokens.HTML;
      const trimmed = t.raw.trim();
      if (/^<br\s*\/?>$/i.test(trimmed)) return <br key={key} />;
      return <Fragment key={key}>{t.raw}</Fragment>;
    }
    default: {
      const raw = "raw" in token && typeof token.raw === "string" ? token.raw : "";
      return raw ? <Fragment key={key}>{raw}</Fragment> : null;
    }
  }
}

function renderBlockToken(token: Token, ctx: LinkHandlerContext, streaming: boolean, key: string): ReactNode {
  switch (token.type) {
    case "paragraph": {
      const t = token as Tokens.Paragraph;
      return <p key={key}>{renderInlineTokens(t.tokens, ctx, key)}</p>;
    }
    case "heading": {
      const t = token as Tokens.Heading;
      const children = renderInlineTokens(t.tokens, ctx, key);
      switch (t.depth) {
        case 1: return <h1 key={key}>{children}</h1>;
        case 2: return <h2 key={key}>{children}</h2>;
        case 3: return <h3 key={key}>{children}</h3>;
        case 4: return <h4 key={key}>{children}</h4>;
        case 5: return <h5 key={key}>{children}</h5>;
        default: return <h6 key={key}>{children}</h6>;
      }
    }
    case "code": {
      const t = token as Tokens.Code;
      return <CodeBlock key={key} code={t.text} lang={t.lang ?? ""} streaming={streaming} />;
    }
    case "blockquote": {
      const t = token as Tokens.Blockquote;
      return (
        <blockquote key={key}>
          {t.tokens.map((child, index) => renderBlockToken(child, ctx, streaming, `${key}.bq.${index}`))}
        </blockquote>
      );
    }
    case "list": {
      const t = token as Tokens.List;
      const ListTag = t.ordered ? "ol" : "ul";
      const start = t.ordered && typeof t.start === "number" && t.start !== 1 ? t.start : undefined;
      const hasTasks = t.items.some((item) => item.task);
      return (
        <ListTag key={key} start={start} className={hasTasks ? "md-task-list" : undefined}>
          {t.items.map((item, itemIndex) => {
            const itemKey = `${key}.li.${itemIndex}`;
            return (
              <li key={itemKey} className={item.task ? "md-task-item" : undefined}>
                {item.task ? (
                  <>
                    <input
                      type="checkbox"
                      checked={Boolean(item.checked)}
                      readOnly
                      tabIndex={-1}
                      aria-label={item.checked ? "Completed" : "Incomplete"}
                      className="md-task-checkbox"
                    />
                    <span className="md-task-body">
                      {renderListItemTokens(item, ctx, streaming, itemKey)}
                    </span>
                  </>
                ) : (
                  renderListItemTokens(item, ctx, streaming, itemKey)
                )}
              </li>
            );
          })}
        </ListTag>
      );
    }
    case "table": {
      const t = token as Tokens.Table;
      return (
        <div key={key} className="md-table-wrap">
          <table className="md-table">
            <thead>
              <tr>
                {t.header.map((cell, cellIndex) => (
                  <th
                    key={`${key}.th.${cellIndex}`}
                    style={t.align[cellIndex] ? { textAlign: t.align[cellIndex]! } : undefined}
                  >
                    {renderInlineTokens(cell.tokens, ctx, `${key}.th.${cellIndex}`)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {t.rows.map((row, rowIndex) => (
                <tr key={`${key}.tr.${rowIndex}`}>
                  {row.map((cell, cellIndex) => (
                    <td
                      key={`${key}.td.${rowIndex}.${cellIndex}`}
                      style={t.align[cellIndex] ? { textAlign: t.align[cellIndex]! } : undefined}
                    >
                      {renderInlineTokens(cell.tokens, ctx, `${key}.td.${rowIndex}.${cellIndex}`)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }
    case "hr":
      return <hr key={key} />;
    case "text": {
      const t = token as Tokens.Text;
      if (t.tokens && t.tokens.length > 0) {
        return <p key={key}>{renderInlineTokens(t.tokens, ctx, key)}</p>;
      }
      return <p key={key}>{decodeHtmlEntities(t.text)}</p>;
    }
    case "html": {
      const t = token as Tokens.HTML;
      const trimmed = t.raw.trim();
      if (!trimmed) return null;
      return <p key={key}>{trimmed}</p>;
    }
    default:
      return null;
  }
}

function renderListItemTokens(
  item: Tokens.ListItem,
  ctx: LinkHandlerContext,
  streaming: boolean,
  key: string,
): ReactNode {
  if (!item.tokens || item.tokens.length === 0) {
    return decodeHtmlEntities(item.text);
  }
  return item.tokens.map((child, idx) => {
    const childKey = `${key}.${idx}`;
    if (child.type === "text") {
      const textTok = child as Tokens.Text;
      const content = textTok.tokens?.length
        ? renderInlineTokens(textTok.tokens, ctx, childKey)
        : decodeHtmlEntities(textTok.text);
      return item.loose ? <p key={childKey}>{content}</p> : <Fragment key={childKey}>{content}</Fragment>;
    }
    if (child.type === "space") return null;
    return renderBlockToken(child, ctx, streaming, childKey);
  });
}

/** Memoized per-block renderer: settled blocks skip work while the trailing block streams. */
const MarkdownBlockView = memo(
  function MarkdownBlockView({
    block,
    ctx,
    streaming,
  }: {
    block: MarkdownBlock;
    ctx: LinkHandlerContext;
    streaming: boolean;
  }) {
    return <>{renderBlockToken(block.token, ctx, streaming, block.key)}</>;
  },
  (prev, next) =>
    prev.block.raw === next.block.raw
    && prev.streaming === next.streaming
    && prev.ctx.sessionId === next.ctx.sessionId
    && prev.ctx.workspacePath === next.ctx.workspacePath,
);

/**
 * Streaming-safe Markdown renderer built on `marked` + lazy Shiki + CSS Custom
 * Highlight token reveal. Handles `file://` and relative workspace links cleanly
 * without `[blocked]`, and memoizes completed top-level blocks while streaming.
 */
export const Markdown = memo(function Markdown({
  text,
  streaming = false,
  compact = false,
}: {
  text: string;
  streaming?: boolean;
  /** Tighter vertical spacing for thinking/activity panels. */
  compact?: boolean;
}) {
  const { sessionId, workspacePath } = useChat();
  const rootRef = useRef<HTMLDivElement>(null);
  const ctx = useMemo<LinkHandlerContext>(
    () => ({ sessionId, workspacePath: workspacePath ?? null }),
    [sessionId, workspacePath],
  );
  const blocks = useMemo(() => parseMarkdownBlocks(text, streaming), [text, streaming]);

  // Attach the dsh-claude-style CSS Custom Highlight reveal while streaming.
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el || !streaming) return;
    const reveal = getStreamReveal();
    if (!reveal) return;
    reveal.attach(el);
    return () => reveal.detach(el);
  }, [streaming]);

  // Scan newly committed streaming characters synchronously before paint.
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el || !streaming) return;
    getStreamReveal()?.scan(el);
  }, [text, streaming]);

  return (
    <div
      ref={rootRef}
      data-agent-markdown=""
      data-streaming={streaming ? "" : undefined}
      data-compact={compact ? "" : undefined}
    >
      {blocks.map((block, index) => (
        <MarkdownBlockView
          key={block.key}
          block={block}
          ctx={ctx}
          streaming={streaming && index === blocks.length - 1}
        />
      ))}
    </div>
  );
});
