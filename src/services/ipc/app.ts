import type {
  AppEvent, AppSettings, GuestClock, HostCheck, HostSnapshot, InstallPlan, InstallStep, MachineInstallOptions, MachineStatus,
  GuestMachineObservation, MirrorResult, OperationRecord,
  SetupAction, SetupCheck,
} from "./generated/core";
import { invokeGuest, invokeIpc, invokeTask, subscribe } from "./common";
import type { CleanupResult, CleanupTarget, MachineDetails, StorageReport } from "./generated/common";

export const appIpc = {
  subscribe: (onEvent: (event: AppEvent) => void) => subscribe<AppEvent, HostSnapshot>("app_subscribe", onEvent),
  clock: () => invokeGuest<GuestClock>("clock"),
  setupChecks: () => invokeGuest<SetupCheck[]>("setup_checks"),
  runSetup: (action: SetupAction) => invokeGuest<OperationRecord>("setup_run", { action }),
  operations: () => invokeGuest<OperationRecord[]>("operations_list"),
  cancelOperation: (id: string) => invokeGuest<void>("operation_cancel", { id }),
  retryOperation: (id: string) => invokeGuest<OperationRecord>("operation_retry", { id }),
  updateSettings: (settings: AppSettings) => invokeGuest<AppSettings>("settings_update", { settings }),
};

export const storageIpc = {
  status: () => invokeIpc<StorageReport>("storage_status"),
  clean: (target: CleanupTarget) => invokeIpc<CleanupResult>("storage_clean", { target }),
};

export const machineIpc = {
  checks: () => invokeIpc<HostCheck[]>("machine_checks"),
  details: () => invokeIpc<MachineDetails | null>("machine_details"),
  observation: () => invokeGuest<GuestMachineObservation>("machine_observation"),
  plan: (options: MachineInstallOptions) => invokeIpc<InstallPlan>("machine_plan", { options }),
  install: (options: MachineInstallOptions) => invokeTask<MachineStatus>("machine_install", { options }),
  cancelInstall: () => invokeIpc<void>("machine_cancel_install"),
  testMirrors: (os: string | null, extra: string[] = []) => invokeIpc<MirrorResult[]>("machine_test_mirrors", { os, extra }),
  verify: () => invokeIpc<InstallStep[]>("machine_verify"),
  resume: (changeMirror: boolean, aptMirror: string | null) =>
    invokeTask<MachineStatus>("machine_resume", { changeMirror, aptMirror }),
  start: () => invokeTask<MachineStatus>("machine_start"),
  stop: () => invokeTask<MachineStatus>("machine_stop"),
  destroy: () => invokeTask<MachineStatus>("machine_destroy"),
};
