import { ActionIcon, Tooltip } from "@mantine/core";
import { IconArrowLeft } from "@tabler/icons-react";
import { createContext, type ReactNode, useContext, useLayoutEffect } from "react";
import { createPortal } from "react-dom";
import { type ChromeLayout, chromeLayout, createWindowChromeRefresh, MAC_DEFAULT } from "../lib/windowChrome";
import { onWindowGeometryChange, windowChrome } from "../services/ipc/window";
import { uiActions, useUi } from "../state/ui";
import { IS_MAC } from "../lib/platform";
import { CabinMark } from "./CabinMark";
import { SidebarToggle } from "./Sidebar";

let applied: ChromeLayout | null = null;

/** Writes the chrome variables, only when they change (no relayout per resize). */
function applyLayout(layout: ChromeLayout) {
  if (applied && applied.strip === layout.strip && applied.inset === layout.inset && applied.center === layout.center) return;
  applied = layout;
  const { strip, inset, center } = layout;
  const root = document.documentElement.style;
  root.setProperty("--strip-h", `${strip}px`);
  root.setProperty("--controls-inset", `${inset}px`);
  root.setProperty("--controls-center", `${center}px`);
}

/** Reserves first-paint space, then lays out the strip from AppKit's measurements. */
export function useWindowChrome() {
  useLayoutEffect(() => {
    if (!IS_MAC) {
      applyLayout(chromeLayout(null));
      return;
    }
    document.documentElement.dataset.platform = "mac";
    applyLayout(MAC_DEFAULT);
    const refresh = createWindowChromeRefresh(windowChrome, applyLayout);
    let disposed = false;
    let unlisten: (() => void) | undefined;
    // Subscribe before measuring so a transition invalidates any older result.
    onWindowGeometryChange(refresh.schedule)
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {})
      .finally(() => refresh.measure());
    return () => {
      disposed = true;
      refresh.dispose();
      unlisten?.();
    };
  }, []);
}

/**
 * The window strip: a draggable band across the top, with the sidebar
 * toggle pinned right after the traffic lights on their centre line. It
 * never moves with the sidebar's state.
 */
/** A full-window view can offer Back in place of the sidebar toggle. */
export function WindowStrip({ focus = false, back = true }: { focus?: boolean; back?: boolean }) {
  const canGoBack = useUi((ui) => back && ui.historyIndex > 0);
  return (
    <>
      <div className="window-strip" data-tauri-drag-region />
      <div className="window-toolbar">
        {!focus ? (
          <>
            <SidebarToggle />
            {IS_MAC && (
              <span
                className="cabin-brand-enter ml-1.5 inline-flex items-center gap-1.5 text-fg select-none"
                data-tauri-drag-region
              >
                <CabinMark variant="color" size={21} className="shrink-0" />
                <span className="text-[13px] font-semibold tracking-[-0.02em]" data-tauri-drag-region>Cabin</span>
              </span>
            )}
          </>
        ) : canGoBack ? (
          <Tooltip label="Back" position="bottom">
            <ActionIcon variant="subtle" color="gray" size="md" onClick={uiActions.back} aria-label="Back">
              <IconArrowLeft size={18} stroke={1.6} />
            </ActionIcon>
          </Tooltip>
        ) : null}
      </div>
    </>
  );
}

const TitleBarSlot = createContext<HTMLElement | null>(null);
export const TitleBarSlotProvider = TitleBarSlot.Provider;

/**
 * Header of the content card. Views fill it with `TitleBarContent`; it takes
 * no space when a view puts nothing there.
 */
export function TitleBar({ slotRef }: { slotRef: (node: HTMLElement | null) => void }) {
  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="titlebar-slot" ref={slotRef} data-tauri-drag-region />
    </header>
  );
}

/**
 * The standard view header: a title, an optional quiet subtitle, then the
 * actions on the right. Every page uses it so titles sit on one baseline.
 */
export function ViewHeader({ title, subtitle, lead, children }: {
  title: ReactNode;
  subtitle?: ReactNode;
  /** Before the title: a Back button, never a logo. */
  lead?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <TitleBarContent>
      {lead}
      <span className="flex min-w-0 flex-1 items-baseline gap-2.5">
        <strong className="shrink-0 text-md font-semibold">{title}</strong>
        {subtitle != null && <span className="min-w-0 truncate text-sm text-faint">{subtitle}</span>}
      </span>
      {children}
    </TitleBarContent>
  );
}

/** Renders a view's header controls inside the content card's header. */
export function TitleBarContent({ children, className = "" }: { children: ReactNode; className?: string }) {
  const slot = useContext(TitleBarSlot);
  if (!slot) return null;
  return createPortal(
    <div className={`titlebar-content ${className}`.trim()} data-tauri-drag-region>
      {children}
    </div>,
    slot,
  );
}
