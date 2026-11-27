import { Button, useComputedColorScheme } from "@mantine/core";
import {
  IconArrowDown, IconChevronDown, IconChevronUp, IconEraser, IconLetterCase,
  IconMinus, IconPlus, IconSearch, IconX,
} from "@tabler/icons-react";
import {
  useCallback, useEffect, useRef, useState, useSyncExternalStore,
  type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode,
} from "react";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-700.css";
import "@fontsource/jetbrains-mono/latin-400-italic.css";
import "@fontsource/jetbrains-mono/latin-700-italic.css";
import { useAutoRefresh } from "../lib/useAutoRefresh";
import { toIpcError } from "../services/ipc/common";
import { clipboard } from "../services/ipc/dialogs";
import { IS_MAC } from "../lib/platform";
import { useI18n } from "../i18n";
import type { Grid } from "../lib/terminal/protocol";
import type { ContextMenuRequest, ScrollState, SearchState } from "../lib/terminal/engine";
import {
  acquireTerminalSession, releaseUnusedTerminals,
  type TerminalSessionController, type TerminalViewState,
} from "../lib/terminal/session";
import {
  BASE, getTerminalFontSize, stepTerminalFontSize, subscribeTerminalFontSize,
  TERMINAL_FONT_MAX, TERMINAL_FONT_MIN,
} from "../lib/terminal/theme";

export type TerminalSize = Grid;

const INITIAL_SCROLL: ScrollState = {
  viewportY: 0,
  scrollbackLength: 0,
  rows: 24,
  isAltScreen: false,
  hasUnreadBelow: false,
};

const INITIAL_SEARCH: SearchState = {
  query: "",
  caseSensitive: false,
  total: 0,
  activeIndex: -1,
};

/** Focus the terminal unless the user is typing in another field or dialog. */
function focusUnlessEditing(controller: TerminalSessionController | null) {
  if (!controller) return;
  const active = document.activeElement as HTMLElement | null;
  const editing =
    active != null &&
    active !== document.body &&
    !active.closest(".terminal-host") &&
    (active.matches("input, textarea, select, [contenteditable='true']") ||
      Boolean(active.closest("[role='dialog'], [role='menu'], .terminal-search")));
  if (!editing) controller.engine.focus();
}

/** Custom interactive scrollbar synced to the terminal's scrollback buffer. */
function TerminalScrollbar({
  scroll,
  onScrollTo,
}: {
  scroll: ScrollState;
  onScrollTo: (viewportY: number) => void;
}) {
  const trackRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ startY: number; startOffset: number; travel: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  if (scroll.isAltScreen || scroll.scrollbackLength <= 0) return null;

  const total = scroll.scrollbackLength + scroll.rows;
  const thumbRatio = Math.max(0.06, Math.min(1, scroll.rows / total));
  const topRatio = scroll.scrollbackLength > 0 ? 1 - scroll.viewportY / scroll.scrollbackLength : 1;

  const onTrackPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const track = trackRef.current;
    if (!track) return;
    event.preventDefault();
    event.stopPropagation();

    const rect = track.getBoundingClientRect();
    const thumbHeight = Math.max(24, rect.height * thumbRatio);
    const travel = Math.max(1, rect.height - thumbHeight);
    const currentTop = topRatio * travel;
    const clickY = event.clientY - rect.top;

    if (clickY >= currentTop && clickY <= currentTop + thumbHeight) {
      dragRef.current = { startY: event.clientY, startOffset: scroll.viewportY, travel };
    } else {
      const targetTop = Math.max(0, Math.min(travel, clickY - thumbHeight / 2));
      const nextOffset = Math.round((1 - targetTop / travel) * scroll.scrollbackLength);
      onScrollTo(nextOffset);
      dragRef.current = { startY: event.clientY, startOffset: nextOffset, travel };
    }
    setDragging(true);
    track.setPointerCapture(event.pointerId);
  };

  const onTrackPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const deltaY = event.clientY - drag.startY;
    const deltaLines = (deltaY / drag.travel) * scroll.scrollbackLength;
    const nextOffset = Math.max(0, Math.min(scroll.scrollbackLength, Math.round(drag.startOffset - deltaLines)));
    onScrollTo(nextOffset);
  };

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    setDragging(false);
    if (trackRef.current?.hasPointerCapture(event.pointerId)) {
      trackRef.current.releasePointerCapture(event.pointerId);
    }
  };

  return (
    <div
      ref={trackRef}
      className={`terminal-scrollbar${dragging ? " dragging" : ""}`}
      role="scrollbar"
      aria-orientation="vertical"
      aria-valuemin={0}
      aria-valuemax={scroll.scrollbackLength}
      aria-valuenow={scroll.scrollbackLength - scroll.viewportY}
      onPointerDown={onTrackPointerDown}
      onPointerMove={onTrackPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
    >
      <div
        className="terminal-scrollbar-thumb"
        style={{
          height: `max(24px, ${(thumbRatio * 100).toFixed(2)}%)`,
          top: `${(topRatio * 100).toFixed(2)}%`,
          transform: `translateY(${(-topRatio * 100).toFixed(2)}%)`,
        }}
      />
    </div>
  );
}

/**
 * Desktop terminal view bound to one backend PTY session.
 * Powered directly by libghostty WASM (`TerminalEngine`) + disconnect-resilient
 * `TerminalSessionController` with pooled scrollback, search, adaptive fitting,
 * reactive font sizing, and native-feeling scrolling.
 */
export function SessionTerminal({
  sessionId,
  onSize,
  focusKey,
  plain = false,
  archived = false,
  notice,
}: {
  sessionId: string;
  onSize?: (size: TerminalSize) => void;
  focusKey?: unknown;
  plain?: boolean;
  archived?: boolean;
  notice?: ReactNode;
}) {
  const { t } = useI18n();
  const slotRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const controllerRef = useRef<TerminalSessionController | null>(null);
  const onSizeRef = useRef(onSize);
  onSizeRef.current = onSize;

  const scheme = useComputedColorScheme("light", { getInitialValueInEffect: false });
  const fontSize = useSyncExternalStore(subscribeTerminalFontSize, getTerminalFontSize);

  const [loadAttempt, setLoadAttempt] = useState(0);
  const [state, setState] = useState<TerminalViewState>({
    phase: "connecting",
    busy: true,
    grid: { cols: 80, rows: 24 },
    scroll: INITIAL_SCROLL,
  });
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchState, setSearchState] = useState<SearchState>(INITIAL_SEARCH);
  const [menu, setMenu] = useState<ContextMenuRequest | null>(null);

  const openSearch = useCallback(() => {
    setSearchOpen(true);
    requestAnimationFrame(() => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    });
  }, []);

  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    controllerRef.current?.engine.clearSearch();
    controllerRef.current?.engine.focus();
  }, []);

  useEffect(() => {
    focusUnlessEditing(controllerRef.current);
  }, [focusKey]);

  // Keep scheme synced in place without tearing down the session controller.
  useEffect(() => {
    controllerRef.current?.setScheme(scheme);
  }, [scheme]);

  // Attach/detach pooled terminal session.
  useEffect(() => {
    const slot = slotRef.current;
    if (!slot) return;
    const focusAtNavigation = document.activeElement;
    let cancelled = false;
    let detach = () => {};

    void acquireTerminalSession(sessionId, scheme)
      .then((controller) => {
        if (cancelled) return;
        controllerRef.current = controller;
        const active = document.activeElement;
        const focusOnAttach =
          (active === focusAtNavigation || active === document.body || active == null) &&
          !active?.closest("[role='dialog'], [role='menu']");

        detach = controller.attach(
          slot,
          { autoStart: plain && !archived, scheme, focus: focusOnAttach },
          {
            onState: setState,
            onSize: (grid) => onSizeRef.current?.(grid),
            onSearchOpen: openSearch,
            onSearchUpdate: setSearchState,
            onContextMenu: (request) => setMenu(request),
          },
        );
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setState((prev) => ({ ...prev, phase: "offline", busy: false, error: toIpcError(cause).message }));
        }
      });

    const onWindowFocus = () => requestAnimationFrame(() => focusUnlessEditing(controllerRef.current));
    window.addEventListener("focus", onWindowFocus);

    return () => {
      cancelled = true;
      window.removeEventListener("focus", onWindowFocus);
      controllerRef.current = null;
      detach();
      releaseUnusedTerminals();
    };
    // `scheme` is intentionally excluded: scheme changes apply in-place via the effect above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, plain, archived, loadAttempt, openSearch]);

  // Close context menu on outside interaction or Escape.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenu(null);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  const retry = () => {
    const controller = controllerRef.current;
    if (controller) {
      controller.engine.focus();
      void controller.sync("retry");
    } else {
      setState((prev) => ({ ...prev, phase: "connecting", busy: true, error: undefined }));
      setLoadAttempt((attempt) => attempt + 1);
    }
  };

  // Automatic recovery uses `"recover"` so a plain shell restarts cleanly once the link returns.
  useAutoRefresh(
    () => {
      const controller = controllerRef.current;
      if (controller) void controller.sync("recover");
      else setLoadAttempt((attempt) => attempt + 1);
    },
    { enabled: Boolean(state.error) || state.phase === "reconnecting", initial: false, interval: 5_000 },
  );

  const zoom = (delta: number) => {
    stepTerminalFontSize(delta);
    controllerRef.current?.engine.focus();
  };

  const handlePasteAction = async () => {
    setMenu(null);
    const controller = controllerRef.current;
    if (!controller) return;
    controller.engine.focus();
    try {
      const text = (await clipboard.read()) || (await navigator.clipboard?.readText());
      if (text) controller.engine.pasteText(text);
    } catch {
      // Clipboard unavailable.
    }
  };

  const status = state.status?.state;
  const failed = state.error ?? (plain && state.status?.state === "failed" ? state.status.message : null);
  const exited = plain && state.status?.state === "exited" ? state.status : null;
  const offline = state.phase === "offline";
  const reconnecting = state.phase === "reconnecting";

  let label: string;
  let tone: "live" | "warn" | "error" | "connecting" | undefined;
  if (archived) {
    label = t("term.status.archived");
  } else if (offline) {
    label = t("term.status.disconnected");
    tone = "warn";
  } else if (reconnecting) {
    label = t("term.status.reconnecting");
    tone = "connecting";
  } else if (failed) {
    label = t("term.status.unavailable");
    tone = "error";
  } else if (status === "running") {
    label = t("term.status.running");
    tone = "live";
  } else if (state.busy || status === "starting" || state.phase === "connecting") {
    label = t("term.status.connecting");
    tone = "connecting";
  } else if (exited) {
    label = t("term.status.exited");
  } else {
    label = t("term.status.stopped");
  }

  const palette = BASE[scheme];
  const surfaceStyle = {
    "--console-bg": palette.background,
    "--console-ink": palette.foreground,
    "--console-caret": palette.accent,
  } as CSSProperties;
  const modifier = IS_MAC ? "⌘" : "Ctrl+";
  const showJump = !state.scroll.isAltScreen && state.scroll.viewportY > 0;
  const showBanner = Boolean(notice || failed || offline || reconnecting || (exited && !archived));

  const menuStyle: CSSProperties | undefined = menu
    ? {
        left: `${Math.min(menu.x, Math.max(8, window.innerWidth - 188))}px`,
        top: `${Math.min(menu.y, Math.max(8, window.innerHeight - 230))}px`,
      }
    : undefined;

  return (
    <div className="terminal-surface" data-terminal-theme={scheme} style={surfaceStyle}>
      <div
        className="terminal-frame"
        onMouseDown={(event) => {
          if (event.button === 0 && (event.target === event.currentTarget || event.target === slotRef.current)) {
            event.preventDefault();
            controllerRef.current?.engine.focus();
          }
        }}
      >
        <div className="terminal-slot" ref={slotRef} />
        <TerminalScrollbar
          scroll={state.scroll}
          onScrollTo={(offset) => controllerRef.current?.engine.scrollTo(offset)}
        />

        {searchOpen && (
          <div className="terminal-search" role="search" onMouseDown={(e) => e.stopPropagation()}>
            <IconSearch size={13} className="terminal-search-icon" aria-hidden />
            <input
              ref={searchInputRef}
              type="text"
              value={searchState.query}
              placeholder={t("term.findPlaceholder")}
              aria-label={t("term.find")}
              onChange={(event) =>
                controllerRef.current?.engine.setSearch(event.target.value, searchState.caseSensitive)
              }
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  closeSearch();
                } else if (event.key === "Enter") {
                  event.preventDefault();
                  if (event.shiftKey) controllerRef.current?.engine.prevMatch();
                  else controllerRef.current?.engine.nextMatch();
                }
              }}
            />
            {searchState.query && (
              <span className="terminal-search-count">
                {searchState.total > 0
                  ? `${searchState.activeIndex + 1} / ${searchState.total}`
                  : t("term.noMatches")}
              </span>
            )}
            <button
              type="button"
              className={searchState.caseSensitive ? "active" : undefined}
              title={t("term.matchCase")}
              aria-label={t("term.matchCase")}
              aria-pressed={searchState.caseSensitive}
              onClick={() =>
                controllerRef.current?.engine.setSearch(searchState.query, !searchState.caseSensitive)
              }
            >
              <IconLetterCase size={14} />
            </button>
            <button
              type="button"
              disabled={searchState.total === 0}
              title={t("term.prevMatch")}
              aria-label={t("term.prevMatch")}
              onClick={() => controllerRef.current?.engine.prevMatch()}
            >
              <IconChevronUp size={14} />
            </button>
            <button
              type="button"
              disabled={searchState.total === 0}
              title={t("term.nextMatch")}
              aria-label={t("term.nextMatch")}
              onClick={() => controllerRef.current?.engine.nextMatch()}
            >
              <IconChevronDown size={14} />
            </button>
            <button
              type="button"
              title={t("common.close")}
              aria-label={t("common.close")}
              onClick={closeSearch}
            >
              <IconX size={14} />
            </button>
          </div>
        )}

        {showJump && (
          <button
            type="button"
            className={`terminal-jump${state.scroll.hasUnreadBelow ? " unread" : ""}`}
            onClick={() => {
              controllerRef.current?.engine.scrollToBottom();
              controllerRef.current?.engine.focus();
            }}
          >
            <IconArrowDown size={13} />
            <span>{state.scroll.hasUnreadBelow ? t("term.newOutput") : t("term.jumpToBottom")}</span>
          </button>
        )}
      </div>

      {showBanner && (
        <div
          className="terminal-notice"
          data-tone={failed ? "error" : offline || reconnecting ? "warn" : undefined}
          role="status"
        >
          {notice ?? (
            <>
              <span className="terminal-notice-text">
                {reconnecting
                  ? failed ?? t("term.reconnectingNotice")
                  : offline
                    ? failed ?? t("term.disconnectedNotice")
                    : failed ??
                      (exited?.code != null
                        ? t("term.exitedCode", { code: exited.code })
                        : t("term.exited"))}
              </span>
              {!archived && (plain || state.error || offline || reconnecting) && (
                <Button size="compact-xs" variant="default" onClick={retry}>
                  {plain && (exited || status === "failed")
                    ? t("term.restartShell")
                    : offline || reconnecting
                      ? t("term.reconnect")
                      : t("common.retry")}
                </Button>
              )}
            </>
          )}
        </div>
      )}

      <div className="terminal-statusbar">
        <div className="terminal-statusbar-left">
          <span className="terminal-status" data-tone={tone} role="status">
            {label}
          </span>
          <span className="terminal-dims" aria-hidden>
            {state.grid.cols} × {state.grid.rows}
          </span>
          {showJump && (
            <button
              type="button"
              className="terminal-scroll-badge"
              onClick={() => {
                controllerRef.current?.engine.scrollToBottom();
                controllerRef.current?.engine.focus();
              }}
            >
              {t("term.scrollLines", { count: state.scroll.viewportY })}
            </button>
          )}
        </div>

        <div className="terminal-statusbar-right">
          <button
            type="button"
            className={`terminal-tool-btn${searchOpen ? " active" : ""}`}
            onClick={() => (searchOpen ? closeSearch() : openSearch())}
            title={`${t("term.find")} (${IS_MAC ? "⌘F" : "Ctrl+Shift+F"})`}
            aria-label={t("term.find")}
          >
            <IconSearch size={13} />
          </button>
          <button
            type="button"
            className="terminal-tool-btn"
            onClick={() => {
              controllerRef.current?.engine.clearLocal();
              controllerRef.current?.engine.focus();
            }}
            title={t("term.clear")}
            aria-label={t("term.clear")}
          >
            <IconEraser size={13} />
          </button>
          <span className="terminal-bar-sep" aria-hidden />
          <div className="terminal-zoom" role="group" aria-label={t("term.textSize")}>
            <button
              type="button"
              disabled={fontSize <= TERMINAL_FONT_MIN}
              onClick={() => zoom(-1)}
              title={t("term.decreaseFont", { shortcut: `${modifier}−` })}
              aria-label={t("term.decreaseFont", { shortcut: `${modifier}−` })}
            >
              <IconMinus size={13} />
            </button>
            <button
              type="button"
              className="terminal-zoom-value"
              onClick={() => zoom(0)}
              title={t("term.resetFont", { shortcut: `${modifier}0` })}
              aria-label={t("term.resetFont", { shortcut: `${modifier}0` })}
            >
              {fontSize} px
            </button>
            <button
              type="button"
              disabled={fontSize >= TERMINAL_FONT_MAX}
              onClick={() => zoom(1)}
              title={t("term.increaseFont", { shortcut: `${modifier}+` })}
              aria-label={t("term.increaseFont", { shortcut: `${modifier}+` })}
            >
              <IconPlus size={13} />
            </button>
          </div>
        </div>
      </div>

      {menu && (
        <div
          className="terminal-menu"
          role="menu"
          style={menuStyle}
          onMouseDown={(event) => event.stopPropagation()}
        >
          <button
            type="button"
            role="menuitem"
            disabled={!menu.hasSelection}
            onClick={() => {
              controllerRef.current?.engine.copySelection();
              setMenu(null);
              controllerRef.current?.engine.focus();
            }}
          >
            <span>{t("term.menu.copy")}</span>
            <kbd>{IS_MAC ? "⌘C" : "Ctrl+Shift+C"}</kbd>
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => void handlePasteAction()}
          >
            <span>{t("term.menu.paste")}</span>
            <kbd>{IS_MAC ? "⌘V" : "Ctrl+Shift+V"}</kbd>
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              controllerRef.current?.engine.selectAll();
              setMenu(null);
              controllerRef.current?.engine.focus();
            }}
          >
            <span>{t("term.menu.selectAll")}</span>
            <kbd>{IS_MAC ? "⌘A" : "Ctrl+Shift+A"}</kbd>
          </button>
          <div className="terminal-menu-sep" />
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setMenu(null);
              openSearch();
            }}
          >
            <span>{t("term.menu.find")}</span>
            <kbd>{IS_MAC ? "⌘F" : "Ctrl+Shift+F"}</kbd>
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setMenu(null);
              controllerRef.current?.engine.clearLocal();
              controllerRef.current?.engine.focus();
            }}
          >
            <span>{t("term.menu.clear")}</span>
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setMenu(null);
              zoom(0);
            }}
          >
            <span>{t("term.menu.resetZoom")}</span>
            <kbd>{modifier}0</kbd>
          </button>
        </div>
      )}
    </div>
  );
}
