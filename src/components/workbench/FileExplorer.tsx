import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { asyncDataLoaderFeature, hotkeysCoreFeature, selectionFeature } from "@headless-tree/core";
import { useTree } from "@headless-tree/react";
import { DefaultFolderOpenedIcon } from "@react-symbols/icons/utils";
import { FileIcon as SymbolFile, FolderIcon as SymbolFolder } from "@react-symbols/icons/utils";
import { IconArrowLeft, IconAt, IconChevronRight, IconCopy, IconSearch, IconX } from "@tabler/icons-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { clipboard } from "../../services/ipc/dialogs";
import type { FsFile } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { libraryIpc } from "../../services/ipc/library";
import { workbenchIpc, type WorkbenchRoot } from "../../services/ipc/workbench";
import { fileGlyph, formatBytes, fuzzyFilter, splitPath, type Decoration } from "../../lib/workbench";
import { uiActions } from "../../state/ui";
import { Markdown } from "../chat/Markdown";
import { CodeView } from "./CodeView";
import { mention } from "./mention";

/** A file's icon from the Symbols set, chosen by its name. */
export function FileIcon({ path }: { path: string }) {
  return <SymbolFile fileName={splitPath(path)[1]} autoAssign width={16} height={16} className="shrink-0" />;
}

interface Node { name: string; path: string; dir: boolean; ignored: boolean }
const ROOT: Node = { name: "", path: "", dir: true, ignored: false };

const DECO_TEXT: Record<Decoration, string> = {
  modified: "text-warn", added: "text-ok", deleted: "text-bad", untracked: "text-ok", conflict: "text-bad", renamed: "text-info",
};
const DECO_LETTER: Record<Decoration, string> = { modified: "M", added: "A", deleted: "D", untracked: "U", conflict: "!", renamed: "R" };
const INDENT = 14;

interface Props {
  root: WorkbenchRoot;
  decorations: Map<string, Decoration>;
  sessionId: string | null;
  /** Bumped by the panel to reload open folders (git status changed). */
  revision: number;
  visible: boolean;
}

/** The session's files: a lazy tree (@headless-tree) with git colours, a fuzzy filter, and a viewer. */
export function FileExplorer({ root, decorations, sessionId, revision, visible }: Props) {
  const key = `${root.workspaceId}:${root.sessionId ?? ""}`;
  const [selected, setSelected] = useState<string | null>(null);
  useEffect(() => setSelected(null), [key]);
  if (selected) return <FileViewer key={`${key}:${selected}`} root={root} path={selected} sessionId={sessionId} visible={visible} onBack={() => setSelected(null)} />;
  return <Explorer key={key} root={root} decorations={decorations} revision={revision} visible={visible} onOpen={setSelected} />;
}

function Explorer({ root, decorations, revision, visible, onOpen }: Omit<Props, "sessionId"> & { onOpen: (path: string) => void }) {
  const nodes = useRef(new Map<string, Node>([["", ROOT]]));
  const children = useRef(new Map<string, { id: string; data: Node }[]>());
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [all, setAll] = useState<string[] | null>(null);

  const tree = useTree<Node>({
    rootItemId: "",
    indent: INDENT,
    getItemName: (item) => item.getItemData().name,
    isItemFolder: (item) => item.getItemData().dir,
    onPrimaryAction: (item) => { if (!item.getItemData().dir) onOpen(item.getId()); },
    dataLoader: {
      getItem: (id) => nodes.current.get(id) ?? ROOT,
      // Children arrive with their data, so a row never renders before its name is known.
      getChildrenWithData: async (id) => {
        try {
          const listing = await workbenchIpc.list(root, id);
          setError(null);
          const entries = listing.entries.map((entry) => {
            const node = { name: entry.name, path: entry.path, dir: entry.kind === "dir", ignored: entry.ignored };
            nodes.current.set(entry.path, node);
            return { id: entry.path, data: node };
          });
          children.current.set(id, entries);
          return entries;
        } catch (err) {
          if (id === "") setError(toIpcError(err).message);
          return children.current.get(id) ?? [];
        }
      },
    },
    features: [asyncDataLoaderFeature, selectionFeature, hotkeysCoreFeature],
  });

  async function refresh() {
    const folders = tree.getItems().filter((item) => item.isFolder() && item.isExpanded());
    await Promise.all([tree.getItemInstance("").invalidateChildrenIds(true), ...folders.map((item) => item.invalidateChildrenIds(true))]);
    if (query) await libraryIpc.workspaceFiles(root.workspaceId, root.sessionId).then(setAll, () => undefined);
    else setAll(null);
  }
  useAutoRefresh(refresh, { enabled: visible, interval: 12_000, minGap: 4_000 });
  useEffect(() => { if (visible && revision) void refresh(); }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!query || all) return;
    libraryIpc.workspaceFiles(root.workspaceId, root.sessionId).then(setAll, () => setAll([]));
  }, [query, all]); // eslint-disable-line react-hooks/exhaustive-deps
  const matches = useMemo(() => (query && all ? fuzzyFilter(query, all, 150) : []), [query, all]);

  return (
    <div className="wb-explorer">
      <div className="wb-search">
        <IconSearch size={14} className="faint" />
        <input value={query} onChange={(e) => setQuery(e.currentTarget.value)} placeholder="Go to file…" spellCheck={false} />
        {query && <button className="wb-icon-btn" aria-label="Clear" onClick={() => setQuery("")}><IconX size={13} /></button>}
      </div>
      <div className="wb-scroll">
        {error && <div className="wb-empty"><strong>Files unavailable</strong><span>{error}</span></div>}
        {query ? (
          all == null ? <div className="wb-empty faint">Searching…</div>
            : matches.length === 0 ? <div className="wb-empty faint">No files match “{query}”</div>
              : matches.map((path) => {
                const [dir, name] = splitPath(path);
                const deco = decorations.get(path);
                return (
                  <button key={path} className="press flex h-7 w-full cursor-pointer items-center gap-2 border-0 bg-transparent px-2.5 text-left text-[13px] text-fg hover:bg-hover" onClick={() => onOpen(path)}>
                    <FileIcon path={path} />
                    <span className={`min-w-0 truncate ${deco ? DECO_TEXT[deco] : ""}`}>{name}</span>
                    <span className="min-w-0 truncate text-xs text-faint">{dir}</span>
                  </button>
                );
              })
        ) : (
          <div {...tree.getContainerProps("Files")} className="px-1.5 py-1 outline-none">
            {tree.getItems().map((item) => {
              const data = item.getItemData();
              const level = item.getItemMeta().level;
              const deco = decorations.get(item.getId());
              const props = item.getProps();
              const open = item.isExpanded();
              return (
                <div
                  {...props}
                  key={item.getId()}
                  title={item.getId()}
                  onClick={(event) => {
                    props.onClick?.(event);
                    if (data.dir) { if (open) item.collapse(); else item.expand(); } else onOpen(item.getId());
                  }}
                  className={`press group flex h-7 cursor-pointer items-center gap-1.5 rounded-md pr-2 text-[13px] ${data.ignored ? "opacity-50" : ""} ${item.isSelected() ? "bg-sel" : "hover:bg-hover"} ${item.isFocused() && !item.isSelected() ? "bg-hover" : ""}`}
                >
                  <span aria-hidden className="h-full shrink-0 self-stretch" style={{ width: level * INDENT + 4, backgroundImage: `repeating-linear-gradient(to right, transparent 0 ${INDENT - 1}px, var(--border) ${INDENT - 1}px ${INDENT}px)`, backgroundPosition: "9px 0", backgroundSize: `${level * INDENT}px 100%`, backgroundRepeat: "no-repeat" }} />
                  {data.dir
                    ? <IconChevronRight size={12} className={`shrink-0 text-faint transition-transform duration-200 ease-out ${open ? "rotate-90" : ""}`} />
                    : <span className="w-3 shrink-0" />}
                  {data.dir
                    ? (open ? <DefaultFolderOpenedIcon width={16} height={16} className="shrink-0" /> : <SymbolFolder folderName={data.name} width={16} height={16} className="shrink-0" />)
                    : <FileIcon path={data.name} />}
                  <span className={`min-w-0 flex-1 truncate ${deco ? DECO_TEXT[deco] : ""}`}>{data.name}</span>
                  {item.isLoading() && !children.current.has(item.getId()) && <span className="size-3 shrink-0 animate-spin rounded-full border-[1.5px] border-line border-t-accent" />}
                  {deco && !data.dir && <span className={`shrink-0 font-mono text-[11px] font-semibold ${DECO_TEXT[deco]}`}>{DECO_LETTER[deco]}</span>}
                  {deco && data.dir && <span className={`size-1.5 shrink-0 rounded-full bg-current ${DECO_TEXT[deco]}`} />}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function FileViewer({ root, path, sessionId, visible, onBack }: { root: WorkbenchRoot; path: string; sessionId: string | null; visible: boolean; onBack: () => void }) {
  const [file, setFile] = useState<FsFile | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rendered, setRendered] = useState(true);
  useAutoRefresh(async (isCurrent) => {
    try {
      const value = await workbenchIpc.read(root, path);
      if (isCurrent()) { setFile(value); setError(null); }
    } catch (err) {
      if (isCurrent()) setError(toIpcError(err).message);
    }
  }, { enabled: visible, interval: 12_000, minGap: 4_000 });
  const [dir, name] = splitPath(path);
  const markdown = fileGlyph(path) === "markdown";
  return (
    <div className="wb-viewer">
      <div className="wb-viewer-head">
        <button className="wb-icon-btn" aria-label="Back to files" title="Back to files" onClick={onBack}><IconArrowLeft size={15} /></button>
        <FileIcon path={path} />
        <div className="wb-viewer-title truncate" title={path}><strong>{name}</strong>{dir && <span className="faint"> {dir}</span>}</div>
        {file && <span className="wb-meta">{formatBytes(file.size)}</span>}
        {markdown && file?.kind === "text" && (
          <button className="wb-chip" onClick={() => setRendered(!rendered)}>{rendered ? "Source" : "Preview"}</button>
        )}
        {sessionId && <button className="wb-icon-btn" title="Mention in chat" aria-label="Mention in chat" onClick={() => mention(sessionId, `@${path} `)}><IconAt size={15} /></button>}
        <button className="wb-icon-btn" title="Copy path" aria-label="Copy path" onClick={() => void clipboard.write(path).then(() => uiActions.toast("Path copied", "success"))}><IconCopy size={14} /></button>
      </div>
      <div className="wb-scroll wb-viewer-body">
        {error && <div className="wb-empty"><strong>Could not open the file</strong><span>{error}</span></div>}
        {!file && !error && <div className="wb-empty faint">Loading…</div>}
        {file?.kind === "text" && (markdown && rendered
          ? <div className="wb-markdown markdown"><Markdown text={file.text ?? ""} /></div>
          : <CodeView path={path} text={file.text ?? ""} />)}
        {file?.kind === "image" && <div className="wb-image"><img src={`data:${file.mimeType};base64,${file.base64}`} alt={name} /></div>}
        {file?.kind === "binary" && <div className="wb-empty"><strong>Binary file</strong><span>{formatBytes(file.size)} — no preview.</span></div>}
        {file?.kind === "tooLarge" && <div className="wb-empty"><strong>Too large to preview</strong><span>{formatBytes(file.size)}</span></div>}
      </div>
    </div>
  );
}
