import { Menu } from "@mantine/core";
import {
  IconArrowBackUp, IconCheck, IconClipboardText, IconCopy, IconMarkdown,
} from "@tabler/icons-react";
import { marked } from "marked";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { clipboard } from "../../services/ipc/dialogs";
import { uiActions } from "../../state/ui";
import { useChat } from "./context";

export function IconButton({
  label,
  onClick,
  children,
  disabled,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
  disabled?: boolean;
}) {
  return (
    <button
      className="press grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-muted hover:bg-hover hover:text-fg disabled:cursor-default disabled:opacity-40 disabled:hover:bg-transparent"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
    >
      {children}
    </button>
  );
}

export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <IconButton
      label={copied ? "Copied" : label}
      onClick={() => {
        void clipboard.write(text).then(() => {
          setCopied(true);
          clearTimeout(timer.current);
          timer.current = setTimeout(() => setCopied(false), 1500);
        }).catch(() => {});
      }}
    >
      {copied ? <IconCheck size={14} className="animate-fade text-ok" /> : <IconCopy size={14} />}
    </IconButton>
  );
}

/** Restore a turn's prompt in the composer. */
export function EditButton({ turn }: { turn: string }) {
  const { edit, readOnly } = useChat();
  if (readOnly || turn === "start") return null;
  return (
    <IconButton label="Restore prompt" onClick={() => edit(turn)}>
      <IconArrowBackUp size={14} />
    </IconButton>
  );
}

/** Copy an assistant answer either as rendered rich text or as its Markdown
 * source. Rich text carries the Markdown as its plain-text alternative. */
export function CopyMenu({ markdown, label = "Copy response" }: { markdown: string; label?: string }) {
  const [copied, setCopied] = useState<"rich" | "md" | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);

  function flash(kind: "rich" | "md") {
    setCopied(kind);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied(null), 1500);
  }

  async function write(run: () => Promise<void>, kind: "rich" | "md") {
    try {
      await run();
      flash(kind);
    } catch (error) {
      uiActions.toast(error instanceof Error ? error.message : String(error), "error");
    }
  }

  return (
    <Menu position="top-end" withinPortal width={200}>
      <Menu.Target>
        <button
          type="button"
          className="press grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-muted hover:bg-hover hover:text-fg disabled:cursor-default disabled:opacity-40 disabled:hover:bg-transparent"
          aria-label={label}
          title={label}
        >
          {copied ? <IconCheck size={14} className="animate-fade text-ok" /> : <IconCopy size={14} />}
        </button>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Item
          leftSection={<IconClipboardText size={14} />}
          onClick={() => void write(async () => {
            const html = await marked.parse(markdown, { async: false, gfm: true });
            await clipboard.writeHtml(html, markdown);
          }, "rich")}
        >
          Copy as rich text
        </Menu.Item>
        <Menu.Item
          leftSection={<IconMarkdown size={14} />}
          onClick={() => void write(() => clipboard.write(markdown), "md")}
        >
          Copy as Markdown
        </Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}
