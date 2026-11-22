import { memo } from "react";
import { Streamdown, type ControlsConfig, type LinkSafetyConfig, type PluginConfig } from "streamdown";
import { code } from "@streamdown/code";
import "streamdown/styles.css";

// Streamdown: streaming-safe markdown with Shiki code blocks.
const PLUGINS: PluginConfig = { code };
const CONTROLS: ControlsConfig = {
  code: { copy: true, download: false },
  table: false,
  mermaid: false,
};
const LINKS: LinkSafetyConfig = { enabled: false };

/** Agent text as markdown. `streaming` repairs half-written syntax and
 * re-renders only the block that is still growing. */
export const Markdown = memo(function Markdown({
  text,
  streaming = false,
}: {
  text: string;
  streaming?: boolean;
}) {
  return (
    <Streamdown
      data-agent-markdown=""
      prefix="sd"
      mode={streaming ? "streaming" : "static"}
      isAnimating={streaming}
      plugins={PLUGINS}
      controls={CONTROLS}
      linkSafety={LINKS}
      lineNumbers={false}
      codeBlockMaxHeight={520}
    >
      {text}
    </Streamdown>
  );
});
