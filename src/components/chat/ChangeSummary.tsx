import { IconFileDiff } from "@tabler/icons-react";
import type { FileChangeView } from "../../services/ipc/generated/core";
import { FileChanges } from "./FileChanges";
import { Stat } from "../diff/Stat";
import { PILL, StripPopover } from "./strip";

/** Latest reported change for each path, rather than a repository net diff. */
export function ChangeSummary({ changes }: { changes: FileChangeView[] }) {
  if (!changes.length) return null;
  const additions = changes.reduce((sum, item) => sum + (item.additions ?? 0), 0);
  const deletions = changes.reduce((sum, item) => sum + (item.deletions ?? 0), 0);
  const known = changes.some((item) => item.additions != null || item.deletions != null);
  const partial = changes.some((item) => item.additions == null || item.deletions == null);
  return (
    <StripPopover
      width="min(620px, calc(100vw - 48px))"
      target={
        <button type="button" className={PILL} title="Each file's latest change in this conversation; not a Git diff">
          <IconFileDiff size={14} />
          <span>{changes.length} {changes.length === 1 ? "file" : "files"} changed</span>
          {known && <><Stat additions={additions} deletions={deletions} />{partial && <span title="Some line counts are unavailable">*</span>}</>}
        </button>
      }
    >
      <FileChanges changes={changes} title="Changed in this conversation" />
      <p className="m-0 px-1.5 pt-2 pb-1 text-[11px] leading-snug text-faint">
        Each file's latest change in this conversation (not a Git diff).{partial && " Some line counts are unavailable."}
      </p>
    </StripPopover>
  );
}
