import { BootHero, BootPage, Note } from "../components/Boot";
import { ActionIcon, Button, Loader, Menu, Modal, Popover, TextInput, Tooltip } from "@mantine/core";
import {
  IconAlertTriangle, IconApps, IconArrowsMaximize, IconBrandChrome, IconCheck, IconDots, IconFileText, IconFingerprint, IconPlayerPlay,
  IconPower, IconRefresh, IconSearch, IconX,
} from "@tabler/icons-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  chromeIpc, desktopIpc, type ChromeStatus, type DesktopApp, type DisplayProtocol, type NativeDisplayStatus,
} from "../services/ipc/chrome";
import { machineIpc } from "../services/ipc/app";
import { toIpcError } from "../services/ipc/common";
import { confirmAction } from "../services/ipc/dialogs";
import { useApp } from "../state/store";
import { uiActions, useUi } from "../state/ui";
import { BrowserEnvironmentPanel } from "../components/BrowserEnvironmentPanel";
import { ViewHeader } from "../components/TitleBar";
import { StepProgress } from "../components/StepProgress";
import { ChromeViewer, type ViewerState } from "../components/ChromeViewer";
import { DISPLAY_ZOOMS, matchDisplay, zoomScale } from "../lib/chromeDisplay";
import { useI18n } from "../i18n";

const INSTALLING = "Installing Chrome";
const PROTOCOL_KEY = "cabin.chromeDisplayProtocol";
const FIT_KEY = "cabin.chromeFitWindow";
/** Resize settles this long before the desktop follows the window. */
const FIT_DELAY_MS = 900;
/** Differences smaller than this (px) are not worth a resolution change. */
const FIT_SLACK = 24;

type Work = "install" | "open" | "stop" | "restart" | "display" | "spice";

function readFit(): boolean {
  try { return localStorage.getItem(FIT_KEY) === "1"; } catch { return false; }
}

function preferredProtocol(): DisplayProtocol {
  try { return localStorage.getItem(PROTOCOL_KEY) === "spice" ? "spice" : "vnc"; }
  catch { return "vnc"; }
}

/**
 * The Chrome page: the browser, and its environment panel (opened from the
 * header, or from the Network Check's browser fixes).
 */
export function ChromePage() {
  const panelOpen = useUi((ui) => ui.browserPanelOpen);
  useEffect(() => () => uiActions.setBrowserPanel(false), []);
  return (
    <>
      <ChromeView />
      <BrowserEnvironmentPanel opened={panelOpen} onClose={() => uiActions.setBrowserPanel(false)} />
    </>
  );
}

/**
 * Chrome inside the machine, shown full size. The header
 * holds what people use (apps, fit to window, environment, restart); the rest (display
 * protocol, scale, setup log, quit) is in its "More" menu. The display
 * reconnects on its own, and with "Fit to window" on, the desktop follows
 * the window size.
 */
export function ChromeView() {
  const { t } = useI18n();
  const machine = useApp((state) => state.machine);
  const [status, setStatus] = useState<ChromeStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState<Work | null>(null);
  const [preferred, setPreferred] = useState(preferredProtocol);
  const [viewer, setViewer] = useState<ViewerState>({ phase: "connecting" });
  const [nativeDisplay, setNativeDisplay] = useState<NativeDisplayStatus | null>(null);
  const [nativeError, setNativeError] = useState<string | null>(null);
  const [showInstallLog, setShowInstallLog] = useState(false);
  const [fit, setFit] = useState(readFit);
  const statusRequest = useRef<Promise<ChromeStatus> | null>(null);
  const screen = useRef<HTMLDivElement>(null);

  const load = useCallback(async (quiet = false) => {
    const request = statusRequest.current ??= chromeIpc.status();
    try {
      setStatus(await request);
      if (!quiet) setError(null);
    } catch (cause) {
      if (!quiet) setError(toIpcError(cause).message);
    } finally {
      if (statusRequest.current === request) statusRequest.current = null;
    }
  }, []);

  useEffect(() => {
    if (machine && machine.state !== "notInstalled") void load();
    // Narrowed to the fields this effect reads: the machine object changes on
    // every resource sample, and `load` writes state this effect owns.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [machine?.state, machine?.connection, machine?.busy, load]);

  const installing = working === "install" || machine?.busy === INSTALLING;
  const watchStatus = machine?.state === "running" && status?.configured;
  useEffect(() => {
    if (!installing && !watchStatus) return;
    const timer = window.setInterval(() => void load(true), installing ? 750 : 5000);
    return () => window.clearInterval(timer);
  }, [installing, watchStatus, load]);

  const run = useCallback(async (kind: Work, task: () => Promise<unknown>) => {
    setError(null);
    setWorking(kind);
    try {
      await task();
      return true;
    } catch (cause) {
      setError(toIpcError(cause).message);
      return false;
    } finally {
      await statusRequest.current?.catch(() => {});
      await load(true);
      setWorking(null);
    }
  }, [load]);

  const desktopRunning = status?.configured && status.installed !== false && machine?.state === "running" && !installing;
  const nativeAvailable = desktopRunning && machine?.connection === "connected" && working !== "spice";
  useEffect(() => {
    setNativeDisplay(null);
    if (!nativeAvailable) return;
    let cancelled = false;
    chromeIpc.nativeDisplay({ action: "status" }).then((next) => {
      if (!cancelled) setNativeDisplay(next);
    }).catch((cause) => {
      if (!cancelled) setNativeError(toIpcError(cause).message);
    });
    return () => { cancelled = true; };
  }, [nativeAvailable, status?.running]);

  async function install() {
    const accepted = await confirmAction(
      "Install Google Chrome in the machine?\n\nThe machine restarts during setup and running tasks stop. If setup fails, the machine's settings are put back and you can try again.",
      "Install Chrome", "Install Chrome",
    );
    if (!accepted) return;
    await run("install", async () => {
      await chromeIpc.install();
      uiActions.toast("Chrome is ready", "success");
    });
  }

  const protocols = status?.displayProtocols ?? [];
  const spiceEnabled = protocols.includes("spice");
  const protocol = spiceEnabled ? preferred : "vnc";
  const nativeZoom = nativeDisplay?.zoom ?? 100;
  const baseScale = (nativeDisplay?.scale ?? 1) * 100 / nativeZoom;
  const canResize = nativeAvailable && status?.running === true && nativeDisplay && !nativeDisplay.recoveryRequired && viewer.phase === "connected" && !working;

  async function selectProtocol(next: DisplayProtocol) {
    if (next === protocol) return;
    if (next === "spice" && !spiceEnabled) {
      if (!await confirmAction(
        `Enable SPICE on this machine? The machine restarts once and running tasks stop.${nativeDisplay?.enabled ? " The desktop returns to its default resolution and scale." : ""}`,
        "Enable SPICE", "Chrome display",
      )) return;
      if (!await run("spice", chromeIpc.enableSpice)) return;
    }
    setViewer({ phase: "connecting" });
    setPreferred(next);
    try { localStorage.setItem(PROTOCOL_KEY, next); } catch { /* The selection still applies. */ }
  }

  async function changeDisplay(action: "zoom" | "match" | "restore", zoom = nativeZoom, silent = false) {
    if (!nativeDisplay || !nativeAvailable || working) return;
    let { width, height } = nativeDisplay;
    let scale = zoomScale(baseScale, zoom);
    if (action === "match") {
      const element = screen.current;
      if (!element) return;
      const size = matchDisplay(element.clientWidth, element.clientHeight, window.devicePixelRatio || 1);
      ({ width, height } = size);
      scale = zoomScale(size.pixelRatio, zoom);
    }
    if (action !== "restore" && (width < 640 || height < 360 || scale > 4)) {
      if (!silent) setNativeError("This window is too small for the desktop. Make it larger or choose a smaller scale.");
      return;
    }
    if (silent && action === "match" && Math.abs(width - nativeDisplay.width) < FIT_SLACK && Math.abs(height - nativeDisplay.height) < FIT_SLACK) return;
    const explanation = action === "restore"
      ? "Restore the previous resolution and scale? Chrome may restart and reopen its tabs."
      : !nativeDisplay.chromeWayland ? "Apply native display settings? Chrome will restart and reopen its tabs." : null;
    if (explanation && (silent || !await confirmAction(explanation, action === "restore" ? "Restore" : "Apply", "Chrome display"))) return;
    setNativeError(null);
    setWorking("display");
    try {
      setNativeDisplay(await chromeIpc.nativeDisplay(action === "restore"
        ? { action: "restore" }
        : { action: "apply", width, height, scale, zoom }));
    } catch (cause) {
      setNativeError(toIpcError(cause).message);
      try { setNativeDisplay(await chromeIpc.nativeDisplay({ action: "status" })); } catch { /* Keep the operation error visible. */ }
    } finally {
      setWorking(null);
      await load(true);
    }
  }

  // ------------------------------------------------------------ states

  if (!machine || (machine.state !== "notInstalled" && !status && !error)) {
    return <Center><Loader size="sm" /></Center>;
  }
  if (machine.state === "notInstalled") {
    return (
      <Center icon title={t("chrome.setupFirst")} action={<Button onClick={() => uiActions.navigate({ kind: "install" })}>{t("chrome.setupMachine")}</Button>}>
        {t("chrome.setupFirstHint")}
      </Center>
    );
  }

  if (installing) {
    return <ChromeSetup running install={status?.install ?? null} />;
  }

  if (!status) {
    return <Center tone="bad" title={t("chrome.unavailable")} error={error} action={<Button variant="default" onClick={() => void load()}>{t("common.retry")}</Button>} />;
  }

  if (!status.configured) {
    if (status.install) {
      return <ChromeSetup install={status.install} error={error} onRetry={() => void install()} />;
    }
    return (
      <Center icon title={t("chrome.notInstalled")} action={<Button leftSection={<IconBrandChrome size={16} />} onClick={() => void install()}>{t("chrome.installBtn")}</Button>} error={error}>
        {t("chrome.notInstalledHint")}
      </Center>
    );
  }

  if (machine.state !== "running") {
    const starting = !!machine.busy;
    return (
      <Center
        icon
        title={starting ? t("chrome.startingMachine") : t("chrome.machineStopped")}
        spinner={starting}
        error={error ?? machine.error}
        action={!starting && <Button leftSection={<IconPlayerPlay size={16} />} onClick={() => void machineIpc.start().catch((cause) => setError(toIpcError(cause).message))}>{t("nav.startMachine")}</Button>}
      />
    );
  }

  if (status.installed === false) {
    return (
      <Center tone="bad" title={t("chrome.missing")} error={error} action={<Button onClick={() => void install()}>{t("chrome.repairBtn")}</Button>}>
        {t("chrome.missingHint")}
      </Center>
    );
  }

  const ready = viewer.phase === "connected" && working !== "spice";
  const toggleFit = () => {
    const next = !fit;
    setFit(next);
    try { localStorage.setItem(FIT_KEY, next ? "1" : "0"); } catch { /* Still applies for this view. */ }
    if (next) void changeDisplay("match");
  };
  return (
    <div className="chrome-page">
      <ViewHeader title={t("nav.chrome")} subtitle={!ready ? (viewer.phase === "failed" ? t("chrome.reconnecting") : t("chrome.connecting")) : status.running === false ? t("chrome.closed") : status.version ?? undefined}>
        <div className={`chrome-tools${ready ? "" : " dock-hidden"}`} role="toolbar" aria-label={t("nav.chrome")} inert={!ready}>
        <AppLauncher onError={setError} />
        <Tooltip label={fit ? t("chrome.fitWindowOn") : t("chrome.fitWindowHint")} withinPortal>
          <ActionIcon variant={fit ? "light" : "subtle"} color={fit ? undefined : "gray"} aria-pressed={fit} aria-label={t("chrome.fitWindow")}
            loading={working === "display"} disabled={!canResize && !fit} onClick={toggleFit}>
            <IconArrowsMaximize size={16} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={t("chrome.environment")} withinPortal>
          <ActionIcon variant="subtle" color="gray" aria-label={t("chrome.environment")} onClick={() => uiActions.setBrowserPanel(true)}>
            <IconFingerprint size={16} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={t("chrome.restartHint")} withinPortal>
          <ActionIcon variant="subtle" color="gray" aria-label={t("chrome.restartChrome")} loading={working === "restart"} disabled={!!working}
            onClick={() => void run("restart", async () => { await chromeIpc.stop(); await chromeIpc.open(); })}>
            <IconRefresh size={16} />
          </ActionIcon>
        </Tooltip>
        <Menu position="bottom-end" withinPortal shadow="md" width={230} offset={8}>
          <Menu.Target>
            <ActionIcon variant="subtle" color="gray" aria-label="More"><IconDots size={16} /></ActionIcon>
          </Menu.Target>
          <Menu.Dropdown>
            <Menu.Label>Chrome {status.version ?? ""}</Menu.Label>
            <Menu.Label>{t("chrome.display")}</Menu.Label>
            <Menu.Item rightSection={protocol === "vnc" ? <IconCheck size={14} /> : null} disabled={!!working} onClick={() => void selectProtocol("vnc")}>VNC</Menu.Item>
            <Menu.Item
              rightSection={protocol === "spice" ? <IconCheck size={14} /> : null}
              disabled={!!working || (!spiceEnabled && (!status.canEnableSpice || !nativeAvailable))}
              onClick={() => void selectProtocol("spice")}
            >
              {spiceEnabled ? "SPICE" : status.canEnableSpice ? "SPICE (restarts the machine)" : "SPICE (not available)"}
            </Menu.Item>
            {nativeDisplay && (
              <>
                <Menu.Label>{t("chrome.scale")} · {nativeDisplay.width} × {nativeDisplay.height}</Menu.Label>
                <div className="chrome-scale" role="group" aria-label={t("chrome.scale")}>
                  {DISPLAY_ZOOMS.map((value) => (
                    <button key={value} type="button" aria-pressed={nativeZoom === value}
                      disabled={!canResize || zoomScale(baseScale, value) > 4}
                      onClick={() => void changeDisplay("zoom", value)}>{value}%</button>
                  ))}
                </div>
                {(nativeDisplay.enabled || nativeDisplay.recoveryRequired) && (
                  <Menu.Item disabled={!!working} onClick={() => { setFit(false); void changeDisplay("restore"); }}>{t("chrome.resetDisplay")}</Menu.Item>
                )}
              </>
            )}
            <Menu.Divider />
            {status.install && <Menu.Item leftSection={<IconFileText size={14} />} onClick={() => setShowInstallLog(true)}>{t("chrome.setupLog")}</Menu.Item>}
            <Menu.Item color="red" leftSection={<IconPower size={14} />} disabled={working === "stop"} onClick={() => void run("stop", () => chromeIpc.stop())}>{t("chrome.quitChrome")}</Menu.Item>
          </Menu.Dropdown>
        </Menu>
        </div>
      </ViewHeader>
      <div ref={screen} className="chrome-screen">
        {working !== "spice" && <ChromeViewer key={protocol} protocol={protocol} onState={setViewer} onError={setError} />}
        {working === "spice" || viewer.phase === "connecting" ? (
          <div className="chrome-connecting" role="status">
            <Loader size="sm" color="gray" />
            {working === "spice" ? t("chrome.turningOnSpice") : t("chrome.connectingDesktop")}
          </div>
        ) : viewer.phase === "failed" && (
          <div className="chrome-connecting">
            <div className="chrome-center" role="alert">
              <Loader size="sm" color="gray" />
              <h2>{t("chrome.reconnectingDesktop")}</h2>
              <p>{viewer.message}</p>
              <div className="chrome-details-actions">
                {protocol === "vnc" && status.viewerReady === false && (
                  <Button size="xs" variant="default" onClick={() => void install()}>{t("chrome.repairBtn")}</Button>
                )}
              </div>
            </div>
          </div>
        )}
      </div>

      {ready && status.running === false && !error && (
        <div className="chrome-toast" role="status">
          {working === "open" || working === "restart" ? (
            <><Loader size={13} color="gray" /><span>{t("chrome.starting")}</span></>
          ) : (
            <><span>{t("chrome.isClosed")}</span><Button variant="subtle" size="compact-xs" disabled={working !== null} onClick={() => void run("open", chromeIpc.open)}>{t("chrome.openChrome")}</Button></>
          )}
        </div>
      )}
      {(error || nativeError) && (
        <div className="chrome-toast" role="alert">
          <IconAlertTriangle size={15} aria-hidden="true" />
          <span>{error ?? nativeError}</span>
          <ActionIcon variant="subtle" color="gray" size="sm" aria-label="Dismiss" onClick={() => { setError(null); setNativeError(null); }}><IconX size={14} /></ActionIcon>
        </div>
      )}

      {ready && (
        <FitFollower
          enabled={fit && !!canResize && !!nativeDisplay?.chromeWayland}
          target={screen}
          onSettle={() => void changeDisplay("match", nativeZoom, true)}
        />
      )}

      <Modal opened={showInstallLog} onClose={() => setShowInstallLog(false)} title="Chrome setup log" size="lg">
        {status.install && (
          <StepProgress steps={[{
            key: "chrome-setup-log",
            title: "Desktop, Chrome and its display",
            tone: status.install.running ? "running" : "ok",
            status: status.install.running ? "In progress" : "Done",
            finished: false,
            log: status.install.lines,
            logPath: status.install.logPath,
          }]} />
        )}
      </Modal>
    </div>
  );
}

function ChromeSetup({ install, running = false, error, onRetry }: {
  install: ChromeStatus["install"];
  running?: boolean;
  error?: string | null;
  onRetry?: () => void;
}) {
  const failure = error?.split("\n")[0];
  return (
    <BootPage wide>
      <BootHero
        tone={running ? "running" : "bad"}
        progress={null}
        title={running ? "Installing Chrome" : "Chrome setup stopped"}
        subtitle={running
          ? "The machine restarts a couple of times. You can leave this page; setup continues in the background."
          : "Chrome was not installed. The full setup log is below."}
        aside={!running && onRetry ? <Button onClick={onRetry}>Retry Chrome setup</Button> : undefined}
      />
      {failure && <Note tone="bad" role="alert">{failure.length > 240 ? `${failure.slice(0, 237)}…` : failure}</Note>}
      <StepProgress steps={[{
        key: "chrome-setup",
        title: "Desktop, Chrome and its display",
        tone: running ? "running" : "bad",
        status: running ? "In progress" : "Failed",
        finished: false,
        log: install?.lines ?? [],
        logPath: install?.logPath,
        detail: install?.lines.length ? `${install.lines.length} log lines` : "Waiting for the first log line…",
      }]} />
    </BootPage>
  );
}

function Center({ title, children, action, error, icon, spinner, tone }: {
  title?: string;
  children?: ReactNode;
  action?: ReactNode;
  error?: string | null;
  icon?: boolean;
  spinner?: boolean;
  tone?: "bad";
}) {
  return (
    <div className="chrome-page chrome-page-center">
      <div className="glass grid max-w-[420px] justify-items-center gap-2 rounded-2xl px-8 py-7 text-center" role={spinner ? "status" : undefined}>
        <span className={`mb-1 grid size-12 place-items-center rounded-2xl ${tone === "bad" ? "bg-bad/13 text-bad" : "bg-hover text-muted"}`}>
          {spinner ? <Loader size="sm" /> : tone === "bad" ? <IconAlertTriangle size={24} stroke={1.6} /> : icon && <IconBrandChrome size={26} stroke={1.5} />}
        </span>
        {title && <h2 className="m-0 text-lg font-semibold">{title}</h2>}
        {children && <p className="m-0 text-md text-muted">{children}</p>}
        {error && <p className="m-0 text-sm text-bad [overflow-wrap:anywhere]" role="alert">{error}</p>}
        {action}
      </div>
    </div>
  );
}

/**
 * Any graphical app installed in the machine, not only Chrome. Each one
 * opens on its own workspace of the same desktop (switch with the bar at
 * the top); Chrome keeps the first.
 */
function AppLauncher({ onError }: { onError: (message: string) => void }) {
  const [opened, setOpened] = useState(false);
  const [apps, setApps] = useState<DesktopApp[] | null>(null);
  const [query, setQuery] = useState("");
  const [starting, setStarting] = useState<string | null>(null);

  useEffect(() => {
    if (!opened) return;
    let cancelled = false;
    desktopIpc.apps().then((list) => { if (!cancelled) setApps(list); }).catch((cause) => {
      if (!cancelled) { setApps([]); onError(toIpcError(cause).message); }
    });
    return () => { cancelled = true; };
  }, [opened, onError]);

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return (apps ?? []).filter((app) => !needle || `${app.name} ${app.comment ?? ""}`.toLowerCase().includes(needle));
  }, [apps, query]);

  async function launch(app: DesktopApp) {
    setStarting(app.id);
    try {
      await desktopIpc.launch(app.id);
      setOpened(false);
    } catch (cause) {
      onError(toIpcError(cause).message);
    } finally {
      setStarting(null);
    }
  }

  return (
    <Popover opened={opened} onChange={setOpened} position="bottom-end" shadow="md" width={280} withinPortal offset={8}>
      <Popover.Target>
        <ActionIcon variant={opened ? "light" : "subtle"} color={opened ? undefined : "gray"} aria-label="Apps" title="Apps" onClick={() => setOpened((v) => !v)}>
          <IconApps size={16} />
        </ActionIcon>
      </Popover.Target>
      <Popover.Dropdown className="chrome-details">
        <TextInput size="xs" autoFocus leftSection={<IconSearch size={13} />} placeholder="Find an app" value={query} onChange={(event) => setQuery(event.currentTarget.value)} />
        <div className="chrome-apps">
          {apps == null && <div className="faint"><Loader size={12} /> Loading apps…</div>}
          {apps != null && shown.length === 0 && <div className="faint">No apps found. Install one in a terminal with apt.</div>}
          {shown.map((app) => (
            <button key={app.id} className="chrome-app" disabled={starting != null} onClick={() => void launch(app)} title={app.comment ?? app.name}>
              <span className="truncate grow">{app.name}</span>
              {starting === app.id && <Loader size={11} />}
            </button>
          ))}
        </div>
      </Popover.Dropdown>
    </Popover>
  );
}

/** Calls `onSettle` once the observed element stops resizing. */
function FitFollower({ enabled, target, onSettle }: {
  enabled: boolean;
  target: React.RefObject<HTMLDivElement | null>;
  onSettle: () => void;
}) {
  const settle = useRef(onSettle);
  settle.current = onSettle;
  useEffect(() => {
    const element = target.current;
    if (!enabled || !element) return;
    let timer: number | undefined;
    const observer = new ResizeObserver(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => settle.current(), FIT_DELAY_MS);
    });
    observer.observe(element);
    return () => { observer.disconnect(); window.clearTimeout(timer); };
  }, [enabled, target]);
  return null;
}
