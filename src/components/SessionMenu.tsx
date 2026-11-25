import { Menu } from "@mantine/core";
import { IconArchive, IconArchiveOff, IconCopy, IconPencil, IconRefresh, IconStar, IconStarOff, IconTrash } from "@tabler/icons-react";
import type { Session } from "../services/ipc/generated/core";
import { libraryIpc } from "../services/ipc/library";
import { uiActions } from "../state/ui";
import { attempt } from "../lib/operations";
import { chatQueue } from "../lib/chatQueue";
import { confirmAction } from "../services/ipc/dialogs";

/** The same actions everywhere a session appears (sidebar, header). */
export function SessionMenuItems({ session, onRename }: { session: Session; onRename: () => void }) {
  return (
    <>
      <Menu.Item
        leftSection={session.starred ? <IconStarOff size={14} /> : <IconStar size={14} />}
        onClick={() => attempt(libraryIpc.updateSession(session.id, { starred: !session.starred }))}
      >
        {session.starred ? "Unstar" : "Star"}
      </Menu.Item>
      <Menu.Item leftSection={<IconPencil size={14} />} onClick={onRename}>
        Rename
      </Menu.Item>
      <Menu.Item
        leftSection={<IconCopy size={14} />}
        onClick={async () => {
          const copy = await attempt(libraryIpc.duplicateSession(session.id));
          if (copy) uiActions.navigate({ kind: "session", id: copy.id });
        }}
      >
        Duplicate
      </Menu.Item>
      {session.mode === "ui" && (
        <Menu.Item
          leftSection={<IconRefresh size={14} />}
          disabled={session.status.state !== "running" && session.status.state !== "starting"}
          onClick={async () => {
            // The conversation stays; the next message starts the agent again
            // and so loads skills, commands and MCP servers changed since.
            await attempt(libraryIpc.stopSession(session.id));
            uiActions.toast("The agent restarts with your next message", "info");
          }}
        >
          Restart agent
        </Menu.Item>
      )}
      <Menu.Item
        leftSection={session.archived ? <IconArchiveOff size={14} /> : <IconArchive size={14} />}
        onClick={() => attempt(libraryIpc.updateSession(session.id, { archived: !session.archived }))}
      >
        {session.archived ? "Unarchive" : "Archive"}
      </Menu.Item>
      <Menu.Divider />
      <Menu.Item
        color="red"
        leftSection={<IconTrash size={14} />}
        onClick={async () => {
          if (!await confirmAction(`Delete the session “${session.name}”? This cannot be undone.`)) return;
          if (await attempt(libraryIpc.deleteSession(session.id).then(() => true))) chatQueue.forget(session.id);
          const view = uiActions.getView();
          if (view.kind === "session" && view.id === session.id) uiActions.navigate({ kind: "new" });
        }}
      >
        Delete
      </Menu.Item>
    </>
  );
}
