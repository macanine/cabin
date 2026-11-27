import { invokeIpc } from "./common";
import type {
  HostCliBinding,
  HostCliConfig,
  HostCliStatus,
  LauncherState,
  NativeChannelState,
} from "./generated/common";

export type {
  HostCliBinding,
  HostCliConfig,
  HostCliStatus,
  LauncherState,
  NativeChannelState,
};

/** Host `cabin` CLI launcher, shell passthrough configuration, and folder bindings. */
export const hostCliIpc = {
  status: () => invokeIpc<HostCliStatus>("host_cli_status"),
  saveConfig: (config: HostCliConfig) => invokeIpc<HostCliStatus>("host_cli_save_config", { config }),
  installLauncher: (replace = false) => invokeIpc<HostCliStatus>("host_cli_install_launcher", { replace }),
  uninstallLauncher: () => invokeIpc<HostCliStatus>("host_cli_uninstall_launcher"),
  repairChannels: () => invokeIpc<HostCliStatus>("host_cli_repair_channels"),
  bindFolder: (hostPath: string, guestPath: string | null = null) =>
    invokeIpc<HostCliStatus>("host_cli_bind_folder", { hostPath, guestPath }),
  unbindFolder: (id: string) => invokeIpc<HostCliStatus>("host_cli_unbind_folder", { id }),
};
