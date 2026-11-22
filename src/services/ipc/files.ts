import { invokeIpc } from "./common";

/** Read a small text file the user picked on the Mac (e.g. a subscription). */
export function readTextFile(path: string): Promise<string> {
  return invokeIpc<string>("read_text_file", { path });
}

/** Save an explicitly exported credential in a user-selected folder. */
export function writeCodexExport(directory: string, contents: string): Promise<string> {
  return invokeIpc<string>("write_codex_export", { directory, contents });
}
