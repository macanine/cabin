/**
 * Streaming token reveal inspired by dsh-claude-style (D107):
 * newly streamed characters fade in over 240ms across 8 CSS Custom Highlight
 * alpha steps with zero DOM wrapper nodes, so text selection, layout, and
 * React reconciliation stay untouched.
 *
 * Scoped exclusively to `[data-agent-markdown][data-streaming] [data-md-reveal-run]`
 * and disabled under Reduce Motion or Lite mode.
 */

export const REVEAL_STEPS = 8;
const REVEAL_DURATION_MS = 240;
const REVEAL_PREFIX = "cabin-reveal-";
const REVEAL_RUN_ATTR = "data-md-reveal-run";
const REVEAL_COLOR_VAR = "--md-reveal-color";
const MAX_STAGGER_MS = 28;
const MIN_STAGGER_MS = 4;
const BURST_LIMIT = 480;

interface HighlightLike {
  size: number;
  clear(): void;
  add(range: AbstractRange): void;
}

interface HighlightRegistryLike {
  set(name: string, highlight: HighlightLike): void;
  delete(name: string): boolean;
}

interface RevealSegment {
  node: Text;
  start: number;
  end: number;
  bornAt: number;
}

function getHighlightApi(): {
  registry: HighlightRegistryLike;
  HighlightCtor: new () => HighlightLike;
} | null {
  if (typeof window === "undefined" || typeof StaticRange === "undefined") return null;
  if (document.documentElement.dataset.perf === "lite") return null;
  if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return null;
  const g = globalThis as unknown as {
    CSS?: { highlights?: HighlightRegistryLike };
    Highlight?: new () => HighlightLike;
  };
  if (!g.CSS?.highlights || typeof g.Highlight !== "function") return null;
  return { registry: g.CSS.highlights, HighlightCtor: g.Highlight };
}

class StreamRevealController {
  private readonly api: { registry: HighlightRegistryLike; HighlightCtor: new () => HighlightLike };
  private readonly highlights: (HighlightLike | null)[] = new Array(REVEAL_STEPS).fill(null);
  private readonly containers = new Set<HTMLElement>();
  private readonly knownLengths = new WeakMap<Text, number>();
  private readonly colorPublished = new WeakMap<HTMLElement, string>();
  private segments: RevealSegment[] = [];
  private rafId: number | null = null;

  constructor(api: { registry: HighlightRegistryLike; HighlightCtor: new () => HighlightLike }) {
    this.api = api;
  }

  /** Register a streaming container and snapshot its already-rendered text as settled. */
  attach(container: HTMLElement) {
    this.containers.add(container);
    this.baseline(container);
  }

  detach(container: HTMLElement) {
    this.containers.delete(container);
    this.segments = this.segments.filter((seg) => seg.node.isConnected && !container.contains(seg.node));
    if (this.containers.size === 0) {
      this.stopFrame();
      this.clearHighlights();
    }
  }

  /** Record current text lengths without animating (prevents history from flashing). */
  private baseline(container: HTMLElement) {
    const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT);
    let current = walker.nextNode();
    while (current) {
      const textNode = current as Text;
      this.knownLengths.set(textNode, textNode.data.length);
      current = walker.nextNode();
    }
  }

  /** Scan a streaming container after a commit and queue fade-in for newly arrived text. */
  scan(container: HTMLElement) {
    if (!this.containers.has(container)) return;
    if (document.documentElement.dataset.perf === "lite") return;

    const now = performance.now();
    const added: Array<{ node: Text; start: number; end: number }> = [];
    let totalAdded = 0;

    const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT);
    let current = walker.nextNode();
    while (current) {
      const node = current as Text;
      const len = node.data.length;
      const prev = this.knownLengths.get(node) ?? 0;
      if (len > prev) {
        this.knownLengths.set(node, len);
        // Skip whitespace-only deltas so empty formatting nodes don't allocate ranges.
        if (node.data.slice(prev, len).trim().length > 0) {
          added.push({ node, start: prev, end: len });
          totalAdded += len - prev;
        }
      } else if (len < prev) {
        this.knownLengths.set(node, len);
      }
      current = walker.nextNode();
    }

    if (added.length === 0) return;
    // Huge pastes or full re-renders settle immediately instead of strobing.
    if (totalAdded > BURST_LIMIT) return;

    // Split large deltas into small visual chunks for a silky wave feel.
    const chunks: Array<{ node: Text; start: number; end: number }> = [];
    for (const entry of added) {
      const span = entry.end - entry.start;
      const step = span > 24 ? Math.ceil(span / 6) : Math.max(1, Math.min(4, span));
      for (let pos = entry.start; pos < entry.end; pos += step) {
        chunks.push({ node: entry.node, start: pos, end: Math.min(entry.end, pos + step) });
      }
    }

    const stagger = chunks.length > 1
      ? Math.max(MIN_STAGGER_MS, Math.min(MAX_STAGGER_MS, Math.round(70 / chunks.length)))
      : 0;

    for (let i = 0; i < chunks.length; i++) {
      const chunk = chunks[i];
      this.publishRunColor(chunk.node.parentElement);
      this.segments.push({
        node: chunk.node,
        start: chunk.start,
        end: chunk.end,
        bornAt: now + i * stagger,
      });
    }

    // Paint synchronously on this tick so arriving glyphs never flash at 100% alpha for 1 frame.
    this.paint(now);
    this.scheduleFrame();
  }

  private publishRunColor(element: HTMLElement | null) {
    if (!element || !element.isConnected) return;
    const theme = document.documentElement.dataset.theme ?? "auto";
    const key = `${theme}:${element.className}`;
    if (this.colorPublished.get(element) === key) return;
    this.colorPublished.set(element, key);
    if (!element.hasAttribute(REVEAL_RUN_ATTR)) {
      element.setAttribute(REVEAL_RUN_ATTR, "");
    }
    const color = window.getComputedStyle(element).color;
    if (color) {
      element.style.setProperty(REVEAL_COLOR_VAR, color);
    }
  }

  private scheduleFrame() {
    if (this.rafId !== null) return;
    this.rafId = requestAnimationFrame((now) => {
      this.rafId = null;
      this.paint(now);
      if (this.segments.length > 0) this.scheduleFrame();
    });
  }

  private stopFrame() {
    if (this.rafId !== null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
  }

  private paint(now: number) {
    const buckets: StaticRange[][] = Array.from({ length: REVEAL_STEPS }, () => []);
    const remaining: RevealSegment[] = [];

    for (const seg of this.segments) {
      if (!seg.node.isConnected) continue;
      const len = seg.node.data.length;
      if (seg.start >= len) continue;
      const end = Math.min(seg.end, len);
      const elapsed = now - seg.bornAt;
      if (elapsed >= REVEAL_DURATION_MS) continue;

      const progress = Math.max(0, elapsed / REVEAL_DURATION_MS);
      // Ease-out curve so letters quickly become legible and gently finish settling.
      const eased = 1 - Math.pow(1 - progress, 2);
      const step = Math.min(REVEAL_STEPS - 1, Math.floor(eased * REVEAL_STEPS));
      try {
        buckets[step].push(
          new StaticRange({
            startContainer: seg.node,
            startOffset: seg.start,
            endContainer: seg.node,
            endOffset: end,
          }),
        );
        remaining.push(seg);
      } catch {
        // Node mutated out of bounds; skip cleanly.
      }
    }

    this.segments = remaining;
    for (let step = 0; step < REVEAL_STEPS; step++) {
      this.showStep(step, buckets[step]);
    }
  }

  private showStep(step: number, ranges: StaticRange[]) {
    let highlight = this.highlights[step];
    if (ranges.length === 0) {
      if (highlight && highlight.size > 0) highlight.clear();
      return;
    }
    if (!highlight) {
      highlight = new this.api.HighlightCtor();
      this.highlights[step] = highlight;
      this.api.registry.set(`${REVEAL_PREFIX}${step}`, highlight);
    } else {
      highlight.clear();
    }
    for (const range of ranges) highlight.add(range);
  }

  private clearHighlights() {
    for (let step = 0; step < REVEAL_STEPS; step++) {
      if (!this.highlights[step]) continue;
      this.api.registry.delete(`${REVEAL_PREFIX}${step}`);
      this.highlights[step] = null;
    }
  }
}

let sharedController: StreamRevealController | null | undefined;

export function getStreamReveal(): StreamRevealController | null {
  if (sharedController !== undefined) return sharedController;
  const api = getHighlightApi();
  sharedController = api ? new StreamRevealController(api) : null;
  return sharedController;
}
