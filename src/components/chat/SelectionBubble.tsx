import { IconCheck, IconCopy, IconQuote } from "@tabler/icons-react";
import { useEffect, useRef, useState, type RefObject } from "react";
import { clipboard } from "../../services/ipc/dialogs";
import { uiActions } from "../../state/ui";

interface Bubble {
  x: number;
  y: number;
  text: string;
  below: boolean;
}

/** Floating actions for a text selection inside the transcript. It replaces
 * the native menu for selected text and offers Copy / Quote. */
export function SelectionBubble({
  containerRef,
  onQuote,
}: {
  containerRef: RefObject<HTMLElement | null>;
  onQuote: (text: string) => void;
}) {
  const [bubble, setBubble] = useState<Bubble | null>(null);
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    function read(): Bubble | null {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) return null;
      const range = selection.getRangeAt(0);
      if (!container!.contains(range.commonAncestorContainer)) return null;
      const text = selection.toString().trim();
      if (!text) return null;
      const rect = range.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) return null;
      const below = rect.top < 64;
      return {
        x: Math.min(Math.max(rect.left + rect.width / 2, 90), window.innerWidth - 90),
        y: below ? rect.bottom + 8 : rect.top - 8,
        text,
        below,
      };
    }

    const show = () => {
      // Let the browser settle the selection on the next frame.
      requestAnimationFrame(() => {
        const next = read();
        setCopied(false);
        setBubble(next);
      });
    };
    const hide = () => setBubble(null);

    const onContextMenu = (event: MouseEvent) => {
      const next = read();
      if (!next) return;
      event.preventDefault();
      setCopied(false);
      setBubble(next);
    };

    const onSelect = () => {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed) hide();
    };

    document.addEventListener("mouseup", show);
    document.addEventListener("keyup", show);
    document.addEventListener("selectionchange", onSelect);
    document.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    container.addEventListener("contextmenu", onContextMenu);
    return () => {
      document.removeEventListener("mouseup", show);
      document.removeEventListener("keyup", show);
      document.removeEventListener("selectionchange", onSelect);
      document.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
      container.removeEventListener("contextmenu", onContextMenu);
    };
  }, [containerRef]);

  if (!bubble) return null;

  function close() {
    setBubble(null);
    window.getSelection()?.removeAllRanges();
  }

  async function copy() {
    if (!bubble) return;
    try {
      await clipboard.write(bubble.text);
      setCopied(true);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setBubble(null), 700);
    } catch (error) {
      uiActions.toast(error instanceof Error ? error.message : String(error), "error");
    }
  }

  return (
    <div
      className={`selection-bubble${bubble.below ? " below" : ""}`}
      style={{ left: bubble.x, top: bubble.y }}
      role="toolbar"
      aria-label="Selected text actions"
      onMouseDown={(event) => event.preventDefault()}
      onMouseUp={(event) => event.stopPropagation()}
    >
      <button type="button" aria-label="Copy selection" onClick={() => void copy()}>
        {copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
        {copied ? "Copied" : "Copy"}
      </button>
      <button
        type="button"
        aria-label="Quote selection"
        onClick={() => {
          onQuote(bubble.text);
          close();
        }}
      >
        <IconQuote size={13} />
        Quote
      </button>
    </div>
  );
}
