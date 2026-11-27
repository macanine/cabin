import { useAutoRefresh } from "./lib/useAutoRefresh";
import { MantineProvider } from "@mantine/core";
import { theme } from "./theme";
import { useEffect, useState } from "react";
import { Sidebar } from "./components/Sidebar";
import { Toasts } from "./components/Toasts";
import { QuitDialog } from "./components/QuitDialog";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { MachineGate } from "./components/MachineGate";
import { BootHero, BootPage } from "./components/Boot";
import { OperationModal, OperationsDrawer } from "./components/OperationCenter";
import { NetworkCheckModal } from "./components/network/NetworkCheckModal";
import { UsageMonitor } from "./components/usage/UsageMonitor";
import { CommandPalette } from "./components/CommandPalette";
import { IS_MAC } from "./lib/platform";
import { connect, refreshNetworkStatus, useApp } from "./state/store";
import { refreshMacPermissions } from "./state/macPermissions";
import { uiActions, useUi } from "./state/ui";
import { NewSessionView } from "./views/NewSessionView";
import { InstallWizard } from "./views/InstallWizard";
import { SessionView } from "./views/SessionView";
import { SettingsView } from "./views/SettingsView";
import { ChromePage } from "./views/ChromeView";
import { AgentsView } from "./views/AgentsView";
import { ExtensionsView } from "./views/ExtensionsView";
import { UsageView } from "./views/UsageView";
import { TitleBar, TitleBarSlotProvider, useWindowChrome, WindowStrip } from "./components/TitleBar";
import { toIpcError } from "./services/ipc/common";
import { useI18n } from "./i18n";
import "./App.css";

function Main() {
  const view = useUi((ui) => ui.view);
  const [titlebarSlot, setTitlebarSlot] = useState<HTMLElement | null>(null);
  return (
    <TitleBarSlotProvider value={titlebarSlot}>
      <TitleBar slotRef={setTitlebarSlot} />
      <ErrorBoundary resetKey={JSON.stringify(view)}>
        <div key={view.kind === "session" ? view.id : view.kind} className={`flex min-h-0 flex-1 flex-col ${view.kind === "settings" ? "" : "view-enter"}`}><View /></div>
      </ErrorBoundary>
    </TitleBarSlotProvider>
  );
}

function View() {
  const view = useUi((ui) => ui.view);
  switch (view.kind) {
    case "session":
      return <SessionView key={view.id} id={view.id} />;
    case "settings":
      return <SettingsView tab={view.tab} />;
    case "install":
      return <InstallWizard />;
    case "chrome":
      return <ChromePage />;
    case "agents":
      return <AgentsView agent={view.agent} section={view.section} />;
    case "mcp":
      return <ExtensionsView key={`mcp:${view.agent ?? "all"}`} initialTab="mcp" agent={view.agent} />;
    case "skills":
      return <ExtensionsView key={`skills:${view.agent ?? "all"}`} initialTab="skills" agent={view.agent} />;
    case "usage":
      return <UsageView />;
    case "new":
      return <NewSessionView workspaceId={view.workspaceId} />;
  }
}

export default function App() {
  const { t } = useI18n();
  const ready = useApp((state) => state.ready);
  const needsInstall = useApp((state) => state.machine?.state === "notInstalled");
  const workspaceReady = useApp((state) => state.guestReady && state.start?.state !== "running" && state.start?.state !== "failed");
  const viewKind = useUi((ui) => ui.view.kind);
  const showSettings = ready && viewKind === "settings";
  const install = ready && (viewKind === "install" || (needsInstall && !showSettings));
  const showWorkspace = workspaceReady && !install;
  const focus = !showWorkspace;
  useEffect(() => {
    if (ready && needsInstall && uiActions.getView().kind !== "settings") uiActions.navigate({ kind: "install" });
  }, [ready, needsInstall]);
  const [error, setError] = useState<string | null>(null);
  useWindowChrome();

  const start = () => connect().then(
    () => setError(null),
    (err: unknown) => setError(toIpcError(err).message),
  );
  useAutoRefresh(start, { enabled: !ready, interval: 12_000 });
  const networkReadFailed = useApp((state) => state.networkLoadError != null);
  useAutoRefresh(refreshNetworkStatus, { enabled: ready && networkReadFailed, initial: false, interval: 10_000 });

  const syncFolders = useApp((state) => JSON.stringify(state.syncs.map(({ project }) => [project.id, project.name, project.hostPath, project.mode])));
  const inspectingPermissions = useUi((ui) => ui.view.kind === "settings" && ui.view.tab === "permissions");
  useAutoRefresh(refreshMacPermissions, { enabled: ready, refreshKey: syncFolders, interval: inspectingPermissions ? 30_000 : 0 });

  // The palette belongs to the workspace; it must not come back over a later one.
  useEffect(() => { if (!showWorkspace) uiActions.closePalette(); }, [showWorkspace]);

  // Desktop shortcuts. On macOS ⌘N / ⌘K / ⌘[ / ⌘] (⌘ never reaches the
  // terminal's PTY). Everywhere, Ctrl+Shift keeps plain Ctrl+N/K/B for shells
  // and agents running in the terminal.
  useEffect(() => {
    if (!showWorkspace) return;
    const onKey = (event: KeyboardEvent) => {
      if (IS_MAC && event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey) {
        if (event.code === "KeyN") uiActions.navigate({ kind: "new" });
        else if (event.code === "KeyK") uiActions.openPalette();
        else if (event.code === "Comma") uiActions.navigate({ kind: "settings" });
        else if (event.code === "BracketLeft") uiActions.back();
        else if (event.code === "BracketRight") uiActions.forward();
        else return;
        event.preventDefault();
        return;
      }
      if (!IS_MAC && event.altKey && !event.ctrlKey && !event.metaKey && !event.shiftKey) {
        if (event.code === "ArrowLeft") uiActions.back();
        else if (event.code === "ArrowRight") uiActions.forward();
        else return;
        event.preventDefault();
        return;
      }
      if (!IS_MAC && event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey && event.code === "Comma") {
        uiActions.navigate({ kind: "settings" });
        event.preventDefault();
        return;
      }
      if (!event.ctrlKey || !event.shiftKey || event.altKey) return;
      if (event.code === "KeyN") uiActions.navigate({ kind: "new" });
      else if (event.code === "KeyB") uiActions.toggleSidebar();
      else if (event.code === "KeyK") uiActions.openPalette();
      else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [showWorkspace]);

  return (
    <MantineProvider theme={theme} defaultColorScheme="auto"><UsageMonitor>
      <div className={`shell${focus ? " focus" : ""}`}>
        <WindowStrip focus={focus} back={showSettings || (install && !needsInstall)} />
        {showWorkspace && <Sidebar />}
        <main className="main">
          {install ? (
            <ErrorBoundary resetKey="install"><InstallWizard /></ErrorBoundary>
          ) : !ready ? (
            <BootPage>
              {error ? (
                <BootHero tone="bad" progress={1} title={t("boot.loadFailed")} subtitle={error} />
              ) : (
                <BootHero tone="running" progress={null} title={t("boot.opening")} />
              )}
            </BootPage>
          ) : showWorkspace || showSettings ? <Main /> : <MachineGate />}
        </main>
      </div>
      <OperationsDrawer />
      <OperationModal />
      <NetworkCheckModal />
      {showWorkspace && <CommandPalette />}
      <QuitDialog />
      <Toasts />
    </UsageMonitor></MantineProvider>
  );
}
