import { invokeIpc, type IpcError } from "./common";
import type { FolderAccess, MacProjectAccess } from "./generated/common";

export const macPermissionsIpc = {
  list: () => invokeIpc<MacProjectAccess[]>("mac_permissions"),
  checkFolder: (path: string, needsWrite: boolean, request = false) =>
    invokeIpc<FolderAccess>("mac_folder_access", { path, needsWrite, request }),
  openFileSettings: () => invokeIpc<void>("mac_open_file_settings"),
};

export function isFolderAccessError(error: IpcError | null): boolean {
  return error?.code === "mac_folder_access" || error?.code === "mac_folder_privacy";
}
