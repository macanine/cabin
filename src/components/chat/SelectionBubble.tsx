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

function isBackwardSelection(selection: Selection): boolean {
  const { anchorNode, anchorOffset, focusNode, focusOffset } = selection;
  if (!anchorNode || !focusNode) return false;
  if (anchorNode === focusNode) return focusOffset < anchorOffset;
  const pos = anchorNode.compareDocumentPosition(focusNode);
  return Boolean(pos & Node.DOCUMENT_POSITION_PRECEDING);
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
  const lastPointer = useRef<{ x: number; y: number } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    function read(pointer?: { x: number; y: number } | null): Bubble | null {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) return null;
      const range = selection.getRangeAt(0);
      if (!container!.contains(range.commonAncestorContainer)) return null;
      const text = selection.toString().trim();
      if (!text) return null;

      const bounds = container!.getBoundingClientRect();
      const rawRects = Array.from(range.getClientRects()).filter(
        (r) => r.width > 0.5 && r.height > 0.5 && r.bottom > bounds.top + 4 && r.top < bounds.bottom - 4,
      );
      const fallbackRect = range.getBoundingClientRect();
      const rects = rawRects.length > 0 ? rawRects : [fallbackRect];
      const first = rects[0];
      const last = rects[rects.length - 1];
      if (!first || (first.width === 0 && first.height === 0 && last.width === 0 && last.height === 0)) {
        return null;
      }

      const backward = isBackwardSelection(selection);
      const multiLine = last.top - first.top > first.height * 0.6;
      // Place below the end when dragging forward across lines (or near the top edge),
      // and above the start for single-line or upward selections so text stays uncovered.
      let below = multiLine ? !backward : false;
      if (!below && first.top - bounds.top < 52) below = true;
      if (below && bounds.bottom - last.bottom < 52 && first.top - bounds.top >= 52) below = false;

      const anchor = below ? last : first;
      const anchorLeftPad = anchor.width >= 160 ? anchor.left + 72 : 0;
      const minX = Math.max(bounds.left + 88, anchorLeftPad, 88);
      const maxX = Math.min(bounds.right - 88, window.innerWidth - 88);
      const rawX = pointer
        ? Math.min(Math.max(pointer.x, anchor.left), anchor.right)
        : anchor.left + anchor.width / 2;
      const x = minX <= maxX ? Math.min(Math.max(rawX, minX), maxX) : bounds.left + bounds.width / 2;
      const y = below
        ? Math.min(last.bottom + 8, bounds.bottom - 46)
        : Math.max(first.top - 8, bounds.top + 46);

      return { x, y, text, below };
    }

    const show = (event?: MouseEvent | KeyboardEvent) => {
      const pointer = event && "clientX" in event ? { x: event.clientX, y: event.clientY } : lastPointer.current;
      if (event && "clientX" in event) lastPointer.current = pointer;
      // Let the browser settle the selection on the next frame.
      requestAnimationFrame(() => {
        const next = read(pointer);
        setCopied(false);
        setBubble(next);
      });
    };
    const hide = () => setBubble(null);

    const onContextMenu = (event: MouseEvent) => {
      const pointer = { x: event.clientX, y: event.clientY };
      lastPointer.current = pointer;
      const next = read(pointer);
      if (!next) return;
      event.preventDefault();
      setCopied(false);
      setBubble(next);
    };

    const onSelect = () => {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed) hide();
    };

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") hide();
    };

    document.addEventListener("mouseup", show);
    document.addEventListener("keyup", show);
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("selectionchange", onSelect);
    document.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    container.addEventListener("contextmenu", onContextMenu);
    return () => {
      document.removeEventListener("mouseup", show);
      document.removeEventListener("keyup", show);
      document.removeEventListener("keydown", onKeyDown);
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
        <span>{copied ? "Copied" : "Copy"}</span>
      </button>
      <span className="selection-bubble-sep" aria-hidden />
      <button
        type="button"
        aria-label="Quote selection"
        onClick={() => {
          onQuote(bubble.text);
          close();
        }}
      >
        <IconQuote size={13} />
        <span>Quote</span>
      </button>
    </div>
  );
}
