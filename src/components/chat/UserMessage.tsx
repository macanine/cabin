import { useEffect, useRef, useState } from "react";
import type { ChatAttachment } from "../../services/ipc/generated/core";
import { libraryIpc } from "../../services/ipc/library";
import type { TimelineRow } from "../../lib/chatTimeline";
import { formatClockTime, formatGuestDateTime } from "../../lib/time";
import { hostTime } from "../../lib/clock";
import { useChat } from "./context";
import { CopyButton, EditButton } from "./RowControls";

/** An attachment's preview, fetched only when it scrolls near the view. */
function SavedAttachment({ file }: { file: ChatAttachment }) {
  const { sessionId } = useChat();
  const root = useRef<HTMLSpanElement>(null);
  const [src, setSrc] = useState<string | null>(null);
  const isImage = ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(file.mimeType);
  useEffect(() => {
    if (!isImage || !root.current) return;
    let cancelled = false;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
      void libraryIpc.chatAttachment(sessionId, file.id).then((data) => {
        if (!cancelled) setSrc(`data:${data.mimeType};base64,${data.base64}`);
      }).catch(() => {});
    }, { rootMargin: "200px" });
    observer.observe(root.current);
    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [file.id, isImage, sessionId]);
  return (
    <span ref={root} className={`overflow-hidden rounded-xl border border-line bg-hover ${src ? "" : "px-2.5 py-1 text-xs text-muted"}`}>
      {src && <img src={src} alt={file.name} className="block max-h-40 max-w-[220px] animate-fade object-cover" />}
      {!src && <span className="truncate">{file.name}</span>}
    </span>
  );
}

/** The user's prompt: a compact bubble on the end side, its actions beside it
 * and only visible while the row is pointed at. */
export function UserMessage({ row }: { row: Extract<TimelineRow, { kind: "user" }> }) {
  const item = row.item;
  const note = item.status === "failed";
  return (
    <div className="group/user flex min-w-0 items-start justify-end gap-0.5">
      <div className="flex items-center gap-px pt-2 opacity-0 transition-opacity duration-200 group-hover/user:opacity-100 group-focus-within/user:opacity-100">
        <EditButton turn={row.turn} />
        {item.text && <CopyButton text={item.text} label="Copy message" />}
      </div>
      <div className="flex max-w-[min(88%,640px)] min-w-0 flex-col items-end gap-1">
        {item.attachments.length > 0 && (
          <div className="flex flex-wrap justify-end gap-1.5">
            {item.attachments.map((file) => <SavedAttachment key={file.id} file={file} />)}
          </div>
        )}
        {item.text && (
          <div className="selectable rounded-2xl rounded-br-md border border-line/70 bg-surface px-4 py-2.5 text-[14.5px] leading-relaxed whitespace-pre-wrap shadow-[0_1px_2px_rgb(0_0_0/0.03)] [overflow-wrap:anywhere]">{item.text}</div>
        )}
        <div className={`flex min-h-4 items-center gap-2 text-[11px] text-faint tabular-nums transition-opacity duration-150 ${note ? "opacity-100" : "opacity-0 group-hover/user:opacity-100 group-focus-within/user:opacity-100"}`}>
          {item.status === "failed" && <span className="text-bad">Not sent: {item.detail}</span>}
          {item.startedAt > 0 && item.status !== "pending" && (
            <span title={formatGuestDateTime(item.startedAt)}>{formatClockTime(hostTime(item.startedAt))}</span>
          )}
        </div>
      </div>
    </div>
  );
}
