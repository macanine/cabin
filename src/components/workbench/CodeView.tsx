import { memo, useEffect, useState } from "react";
import { languageFor } from "../../lib/workbench";

// Highlighting is skipped above these sizes: the plain view stays instant.
const MAX_HIGHLIGHT_BYTES = 300_000;
const MAX_HIGHLIGHT_LINES = 8000;

type Highlighter = typeof import("shiki")["codeToHtml"];
let shiki: Promise<Highlighter> | null = null;
const loadShiki = () => (shiki ??= import("shiki").then((module) => module.codeToHtml));

/**
 * Read-only source with line numbers and Shiki highlighting in both themes
 * (light and dark colours ride on CSS variables, switched in workbench.css).
 * Plain text renders first; the highlighted version replaces it when ready.
 */
export const CodeView = memo(function CodeView({ path, text }: { path: string; text: string }) {
  const [html, setHtml] = useState<string | null>(null);
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();

  useEffect(() => {
    setHtml(null);
    const lang = languageFor(path);
    if (text.length > MAX_HIGHLIGHT_BYTES || lines.length > MAX_HIGHLIGHT_LINES) return;
    let cancelled = false;
    loadShiki()
      .then((codeToHtml) => codeToHtml(text, {
        lang,
        themes: { light: "github-light", dark: "github-dark" },
        defaultColor: false,
      }).catch(() => codeToHtml(text, { lang: "text", themes: { light: "github-light", dark: "github-dark" }, defaultColor: false })))
      .then((out) => { if (!cancelled) setHtml(out); })
      .catch(() => { /* The plain view stays. */ });
    return () => { cancelled = true; };
  }, [path, text]); // eslint-disable-line react-hooks/exhaustive-deps

  const gutter = String(lines.length).length;
  return (
    <div className="wb-code" style={{ ["--wb-gutter" as string]: `${gutter + 1}ch` }}>
      {html ? (
        <div className="wb-code-shiki" dangerouslySetInnerHTML={{ __html: html }} />
      ) : (
        <pre className="wb-code-plain"><code>{lines.map((line, index) => <span className="line" key={index}>{line}{"\n"}</span>)}</code></pre>
      )}
    </div>
  );
});
