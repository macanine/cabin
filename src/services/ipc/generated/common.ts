// Generated from src-tauri; regenerate with
// CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings (in src-tauri/).

export type AppError = { code: string, message: string, details?: string, };
export type TaskEvent<T> = { "type": "completed", result: T, } | { "type": "failed", error: AppError, };
export type WindowChrome = { 
/**
 * Right edge of the last button (zoom): content must start after it.
 */
controlsRight: number, 
/**
 * Vertical centre of the buttons: the title bar is centred on it.
 */
controlsCenterY: number, };
export type DeviceReportState = "missing" | "stale" | "unverified" | "failed" | "warnings" | "passed";
export type DeviceFinding = { id: string, group: string, severity: string, title: string, residual: boolean, };
export type DeviceDetails = { preset: string | null, cpu: string | null, storage: string | null, nic: string | null, runtime: boolean, sealed: boolean, state: DeviceReportState, measuredAt: number | null, reportPath: string | null, findings: Array<DeviceFinding>, };
export type ResourceRequest = { cpus: number, memoryMb: number, diskGib: number | null, };
export type MachineDetails = { osName: string, accelerator: string, qemuRelease: string | null, imageUrl: string | null, imageChecksum: string | null, aptMirror: string | null, diskPath: string, diskAllocatedBytes: number | null, pendingResources: ResourceRequest | null, diskGrowthNotice: string | null, instancePath: string, logsPath: string, networkMode: string, sshEndpoint: string | null, mac: string, timezone: string, locale: string, desktop: string | null, displayMode: string, resolution: string, scale: number, gpu: string, audio: string, spice: boolean, pid: number | null, startedAt: number | null, device: DeviceDetails | null, };
export type Fix = { "kind": "enableWhpx" } | { "kind": "restartHost" } | { "kind": "reinstallApp" } | { "kind": "openUrl", url: string, };
export type Readiness = { id: string, label: string, ok: boolean, 
/**
 * A warning: the install can proceed.
 */
optional: boolean, detail: string, fix: Fix | null, };
export type HostFacts = { memoryMb: number, cpus: number, 
/**
 * The host account name, as the OS reports it (not yet sanitized).
 */
username: string | null, deviceProfileSupported: boolean, 
/**
 * macos | windows | linux
 */
platform: string, maxDiskGb: number, };
export type ReadinessReport = { items: Array<Readiness>, host: HostFacts, };
export type FixOutcome = { restartRequired: boolean, };
export type StorageKind = "application" | "virtualDisk" | "guestFiles" | "systemImages" | "engine" | "downloads" | "logs" | "settings" | "webkitCache" | "webkitData" | "otherData" | "developerCache";
export type StorageEntry = { kind: StorageKind, bytes: number, cleanableBytes: number, complete: boolean, };
export type StorageVolume = { totalBytes: number, availableBytes: number, };
export type CleanupTarget = "downloads" | "logs";
export type CleanupResult = { removedBytes: number, failures: Array<string>, };
export type StorageReport = { entries: Array<StorageEntry>, totalBytes: number, volume: StorageVolume | null, dataPath: string, appPath: string | null, diskCapacity: string | null, cleanupBlocked: string | null, logRetentionDays: number, };
export type FolderAccessState = "available" | "readDenied" | "writeDenied" | "missing" | "unavailable";
export type FolderAccess = { state: FolderAccessState, message: string | null, privacySettings: boolean, };
export type MacProjectAccess = { id: string, name: string, path: string, needsWrite: boolean, access: FolderAccess, };
export type SystemRoute = { 
/**
 * The interface a public destination leaves through (e.g. `utun1024`).
 */
internetInterface: string | null, 
/**
 * The interface of the `default` route (the physical adapter, e.g. `en0`).
 */
defaultInterface: string | null, 
/**
 * Internet traffic is captured by a tunnel instead of the physical adapter.
 */
tunnel: boolean, 
/**
 * The tunnel routes through a fake-IP gateway (198.18.0.0/15), typical
 * of Clash/mihomo and sing-box TUN modes.
 */
fakeIp: boolean, };
export type IpcCommand =
  | "app_subscribe"
  | "app_quit"
  | "app_background"
  | "guest_invoke"
  | "frontend_diagnostic"
  | "machine_checks"
  | "machine_diagnostics"
  | "host_fix"
  | "machine_details"
  | "machine_save_resources"
  | "storage_status"
  | "storage_clean"
  | "machine_plan"
  | "machine_install"
  | "machine_cancel_install"
  | "machine_test_mirrors"
  | "machine_verify"
  | "machine_resume"
  | "machine_start"
  | "machine_stop"
  | "machine_destroy"
  | "chrome_status"
  | "browser_environment"
  | "browser_settings_save"
  | "chrome_install"
  | "chrome_open"
  | "chrome_stop"
  | "chrome_paste"
  | "chrome_native_display"
  | "chrome_enable_spice"
  | "chrome_display"
  | "desktop_apps"
  | "desktop_launch"
  | "preview_forward"
  | "preview_close"
  | "read_text_file"
  | "write_codex_export"
  | "window_chrome"
  | "mac_permissions"
  | "mac_folder_access"
  | "mac_open_file_settings"
  | "sync_list"
  | "sync_preview"
  | "sync_create"
  | "sync_update"
  | "sync_remove"
  | "sync_now"
  | "sync_resolve";
