import { libraryIpc } from "../services/ipc/library";
import { uiActions } from "../state/ui";
import { attempt } from "./operations";

/** Open a new terminal (the user's login shell) in a workspace and show it. */
export async function openTerminal(workspaceId: string): Promise<void> {
  const session = await attempt(libraryIpc.createTerminal(workspaceId));
  if (session) uiActions.navigate({ kind: "session", id: session.id });
}
