import { ask, open } from "@tauri-apps/plugin-dialog";
import {
  readText, writeHtml, writeText,
} from "@tauri-apps/plugin-clipboard-manager";

export async function chooseFolder(defaultPath?: string): Promise<string | null> {
  const result = await open({ directory: true, multiple: false, defaultPath });
  return typeof result === "string" ? result : null;
}

export async function chooseFile(): Promise<string | null> {
  const result = await open({ directory: false, multiple: false });
  return typeof result === "string" ? result : null;
}

/** Text and rich text; `writeHtml` takes a plain-text alternative for targets
 * that accept only text. */
export const clipboard = {
  read: readText,
  write: writeText,
  writeHtml,
};

/**
 * Native confirmation dialog. `window.confirm` does not work in the Tauri
 * webview (it returns false without showing anything), so always use this.
 */
export function confirmAction(message: string, okLabel = "Continue", title = "Cabin"): Promise<boolean> {
  return ask(message, { title, kind: "warning", okLabel, cancelLabel: "Cancel" });
}
