import type { Terminal } from "ghostty-web";

/** Route WebKit IME input through Ghostty’s textarea and align it to the canvas caret. */
export function installTerminalIme(term: Terminal, root: HTMLElement) {
  let positionInput = (_force = false) => {};
  root.contentEditable = "false";
  root.removeAttribute("tabindex");
  root.removeAttribute("role");
  root.removeAttribute("aria-label");
  root.removeAttribute("aria-multiline");
  const textarea = term.textarea;
  if (textarea) {
    // Keep a real caret rectangle for the IME even while the input is hidden.
    textarea.style.clipPath = "none";
    textarea.style.pointerEvents = "none";
    let composing = false;
    let preedit = "";
    let lastCommit = "";
    let commitTimer: ReturnType<typeof setTimeout> | undefined;
    const measure = document.createElement("canvas").getContext("2d");
    textarea.wrap = "off";
    positionInput = (force = false) => {
      const cursor = term.wasmTerm?.getCursor();
      const metrics = term.renderer?.getMetrics();
      // Ghostty temporarily moves this textarea for its context menu.
      if (!cursor || !metrics || (!force && textarea.style.position === "fixed")) return;
      const col = Math.max(0, Math.min(term.cols - 1, cursor.x));
      const row = Math.max(0, Math.min(term.rows - 1, cursor.y + Math.floor(term.viewportY)));
      const font = `${term.options.fontSize}px ${term.options.fontFamily}`;
      if (measure) measure.font = font;
      const textWidth = composing ? (measure?.measureText(preedit).width ?? preedit.length * metrics.width) : 0;
      const width = Math.min((term.cols - col) * metrics.width, Math.max(metrics.width, Math.ceil(textWidth + metrics.width)));
      textarea.style.position = "absolute";
      textarea.style.left = `${col * metrics.width}px`;
      textarea.style.top = `${row * metrics.height}px`;
      textarea.style.width = `${width}px`;
      textarea.style.height = `${metrics.height}px`;
      textarea.style.font = font;
      textarea.style.lineHeight = `${metrics.height}px`;
    };
    // Ghostty prevents beforeinput on the host; it must not cancel the IME's
    // composition events on the textarea. compositionend still bubbles to
    // Ghostty, which sends the committed text to the PTY once.
    textarea.addEventListener("beforeinput", (event) => event.stopPropagation());
    textarea.addEventListener("compositionstart", () => {
      composing = true;
      preedit = "";
      lastCommit = "";
      clearTimeout(commitTimer);
      textarea.classList.add("terminal-composing");
      positionInput(true);
    });
    textarea.addEventListener("compositionupdate", (event) => {
      preedit = event.data;
      positionInput(true);
    });
    textarea.addEventListener("compositionend", (event) => {
      composing = false;
      preedit = "";
      lastCommit = event.data;
      textarea.classList.remove("terminal-composing");
      positionInput();
      clearTimeout(commitTimer);
      commitTimer = setTimeout(() => { lastCommit = ""; }, 0);
      queueMicrotask(() => { if (!composing) textarea.value = ""; });
    });
    // Some input methods commit text without a composition event or keydown.
    textarea.addEventListener("input", (event) => {
      const input = event as InputEvent;
      if (composing || input.isComposing || !["insertText", "insertReplacementText"].includes(input.inputType)) return;
      const text = input.data ?? textarea.value;
      if (text && text !== lastCommit) term.input(text, true);
      lastCommit = "";
      textarea.value = "";
    });
    positionInput();
  }
  return positionInput;
}
