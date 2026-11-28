import { memo, useEffect, useState } from "react";
import { languageFor } from "../../lib/workbench";

// Highlighting is skipped above these sizes: the plain view stays instant.
const MAX_HIGHLIGHT_BYTES = 300_000;
const MAX_HIGHLIGHT_LINES = 8000;

type Highlighter = typeof import("shiki")["codeToHtml"];
let shiki: Promise<Highlighter> | null = null;
const loadShiki = () => (shiki ??= import("shiki").then((module) => module.codeToHtml));

/**
 * Read-only source with a separate non-selectable line-number gutter and Shiki
 * highlighting in both themes (switched in workbench.css). Plain text renders
 * first; the highlighted version replaces it when ready.
 */
export const CodeView = memo(function CodeView({ path, text }: { path: string; text: string }) {
  const [html, setHtml] = useState<string | null>(null);
  const source = text.endsWith("\n") ? text.slice(0, -1) : text;
  const lines = source.split("\n");

  useEffect(() => {
    setHtml(null);
    const lang = languageFor(path);
    if (source.length > MAX_HIGHLIGHT_BYTES || lines.length > MAX_HIGHLIGHT_LINES) return;
    let cancelled = false;
    loadShiki()
      .then((codeToHtml) => codeToHtml(source, {
        lang,
        themes: { light: "github-light", dark: "github-dark" },
        defaultColor: false,
      }).catch(() => codeToHtml(source, { lang: "text", themes: { light: "github-light", dark: "github-dark" }, defaultColor: false })))
      .then((out) => { if (!cancelled) setHtml(out); })
      .catch(() => { /* The plain view stays. */ });
    return () => { cancelled = true; };
  }, [path, source]); // eslint-disable-line react-hooks/exhaustive-deps

  const gutterDigits = Math.max(2, String(lines.length).length);
  return (
    <div className="wb-code" style={{ ["--wb-gutter" as string]: `${gutterDigits}ch` }}>
      <div className="wb-code-gutter" aria-hidden="true">
        {lines.map((_, index) => <span className="wb-code-num" key={index}>{index + 1}</span>)}
      </div>
      <div className="wb-code-body">
        {html ? (
          <div className="wb-code-shiki" dangerouslySetInnerHTML={{ __html: html }} />
        ) : (
          <pre className="wb-code-plain"><code>{lines.map((line, index) => <span className="line" key={index}>{line}{"\n"}</span>)}</code></pre>
        )}
      </div>
    </div>
  );
});
