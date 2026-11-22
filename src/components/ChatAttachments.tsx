import { ActionIcon, Tooltip } from "@mantine/core";
import { IconPaperclip, IconX } from "@tabler/icons-react";
import { useRef, useState } from "react";
import type { ChatAttachmentInput } from "../services/ipc/generated/core";
import { uiActions } from "../state/ui";

export function useChatAttachments() {
  const [files, setFiles] = useState<ChatAttachmentInput[]>([]);
  const [loading, setLoading] = useState(false);
  async function add(list: FileList | File[] | null) {
    const next = list ? Array.from(list) : [];
    if (!next.length) return;
    if (files.length + next.length > 4 || next.some((file) => file.size > 8 * 1024 * 1024)) {
      uiActions.toast("Attach up to four files, each no larger than 8 MB.", "error"); return;
    }
    setLoading(true);
    try {
      const encoded = await Promise.all(next.map((file) => new Promise<ChatAttachmentInput>((resolve, reject) => {
        const reader = new FileReader();
        reader.onerror = () => reject(new Error(`Could not read ${file.name}`));
        reader.onload = () => { const result = String(reader.result); resolve({ name:file.name, mimeType:file.type || "application/octet-stream", base64:result.slice(result.indexOf(",") + 1) }); };
        reader.readAsDataURL(file);
      })));
      setFiles((old) => [...old, ...encoded]);
    } catch (error) { uiActions.toast(error instanceof Error ? error.message : String(error), "error"); }
    finally { setLoading(false); }
  }
  return { files, loading, add, replace: (next: ChatAttachmentInput[]) => setFiles(next.slice(0, 4)), remove: (index: number) => setFiles((old) => old.filter((_, i) => i !== index)), clear: () => setFiles([]) };
}

/** Images carried by a paste or drop (a screenshot arrives as a file). */
export function clipboardFiles(data: DataTransfer | null): File[] {
  if (!data) return [];
  const files = Array.from(data.files).filter((file) => file.type.startsWith("image/"));
  if (files.length) return files;
  return Array.from(data.items)
    .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
    .map((item) => item.getAsFile())
    .filter((file): file is File => file != null);
}

type AttachmentState = ReturnType<typeof useChatAttachments>;
export function AttachmentPicker({ state, disabled }: { state: AttachmentState; disabled?: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  return <><Tooltip label="Attach files"><ActionIcon variant="subtle" color="gray" size="sm" aria-label="Attach files" loading={state.loading} disabled={disabled || state.files.length >= 4} onClick={() => input.current?.click()}><IconPaperclip size={16} /></ActionIcon></Tooltip><input ref={input} type="file" multiple hidden onChange={(event) => { void state.add(event.currentTarget.files); event.currentTarget.value = ""; }} /></>;
}
export function AttachmentList({ state }: { state: AttachmentState }) {
  if (!state.files.length) return null;
  return (
    <div className="flex flex-wrap gap-1.5 pt-1.5">
      {state.files.map((file, index) => {
        const image = file.mimeType.startsWith("image/");
        return (
          <span key={`${file.name}-${index}`} className={`group/att relative inline-flex items-center gap-1.5 overflow-hidden rounded-xl border border-line bg-hover text-xs text-muted ${image ? "" : "py-1 pr-6 pl-2"}`}>
            {image ? <img src={`data:${file.mimeType};base64,${file.base64}`} alt={file.name} className="block size-14 object-cover" /> : <IconPaperclip size={12} />}
            {!image && file.name}
            <button type="button" aria-label={`Remove ${file.name}`} onClick={() => state.remove(index)} className="absolute top-0.5 right-0.5 grid size-[18px] cursor-pointer place-items-center rounded-full border-0 bg-surface/90 p-0 text-muted opacity-0 shadow-sm transition-opacity duration-150 group-hover/att:opacity-100 hover:text-bad focus-visible:opacity-100"><IconX size={11} /></button>
          </span>
        );
      })}
    </div>
  );
}
