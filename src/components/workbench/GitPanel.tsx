import {
  IconArrowBackUp, IconArrowDown, IconArrowLeft, IconArrowUp, IconCheck, IconChevronRight, IconGitBranch,
  IconGitCommit, IconLayoutColumns, IconLayoutRows, IconMinus, IconPlus,
} from "@tabler/icons-react";
import { useEffect, useMemo, useState } from "react";
import type { GitFileDiff, GitFileStatus, GitStatus } from "../../services/ipc/generated/core";
import { toIpcError } from "../../services/ipc/common";
import { confirmAction } from "../../services/ipc/dialogs";
import { workbenchIpc, type WorkbenchRoot } from "../../services/ipc/workbench";
import { changeGroups, splitPath, STATUS_LABEL, statusLetter } from "../../lib/workbench";
import { uiActions } from "../../state/ui";
import { Diff } from "../diff/Diff";
import { Stat } from "../diff/Stat";
import { FileIcon } from "./FileExplorer";
import type { GitStatusState } from "./useGitStatus";

type Group = "conflicted" | "staged" | "changes";
const TITLE: Record<Group, string> = { conflicted: "Merge conflicts", staged: "Staged changes", changes: "Changes" };

/** Source control like an editor's: branch, commit box, grouped changes with
 * stage / unstage / discard, and a per-file diff (inline or side by side). */
export function GitPanel({ root, git }: { root: WorkbenchRoot; git: GitStatusState }) {
  const { status } = git;
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<Group>>(new Set());
  const [open, setOpen] = useState<{ path: string; staged: boolean } | null>(null);
  const groups = useMemo(() => changeGroups(status), [status]);

  async function act(run: () => Promise<GitStatus>) {
    setBusy(true);
    try { git.accept(await run()); } catch (err) { uiActions.toast(toIpcError(err).message, "error"); } finally { setBusy(false); }
  }
  const stage = (paths: string[]) => act(() => workbenchIpc.stage(root, paths));
  const unstage = (paths: string[]) => act(() => workbenchIpc.unstage(root, paths));
  async function discard(files: GitFileStatus[]) {
    const label = files.length === 1 ? splitPath(files[0].path)[1] : `${files.length} files`;
    const untracked = files.some((file) => file.status === "?");
    if (!(await confirmAction(`Discard changes to ${label}?${untracked ? " Untracked files will be deleted." : ""} This cannot be undone.`, "Discard"))) return;
    await act(() => workbenchIpc.discard(root, files.map((file) => file.path)));
  }
  async function commit() {
    const all = groups.staged.length === 0;
    setBusy(true);
    try {
      const done = await workbenchIpc.commit(root, message, all);
      setMessage("");
      uiActions.toast(`Committed ${done.head}: ${done.summary}`, "success");
      await git.refresh();
    } catch (err) {
      uiActions.toast(toIpcError(err).message, "error");
    } finally {
      setBusy(false);
    }
  }

  if (open) {
    const current = (open.staged ? groups.staged : [...groups.changes, ...groups.conflicted]).find((file) => file.path === open.path);
    return (
      <DiffView root={root} path={open.path} staged={open.staged} file={current ?? null} busy={busy} onBack={() => setOpen(null)}
        onToggleStage={() => void (open.staged ? unstage([open.path]) : stage([open.path])).then(() => setOpen(null))} />
    );
  }
  if (git.error && !status) return <div className="wb-empty"><strong>Git unavailable</strong><span>{git.error}</span></div>;
  if (!status) return <div className="wb-empty faint">Reading git status…</div>;
  if (!status.repository) {
    return <div className="wb-empty"><IconGitBranch size={22} className="faint" /><strong>Not a git repository</strong><span>Run <code>git init</code> in this workspace to track changes here.</span></div>;
  }
  const total = groups.staged.length + groups.changes.length + groups.conflicted.length;
  const commitLabel = groups.staged.length ? "Commit" : "Commit all";

  return (
    <div className="wb-git">
      <div className="wb-branch">
        <IconGitBranch size={14} />
        <strong className="truncate">{status.branch ?? (status.head ? `detached ${status.head}` : "no commits yet")}</strong>
        {status.ahead > 0 && <span className="wb-ab" title="Commits to push"><IconArrowUp size={11} />{status.ahead}</span>}
        {status.behind > 0 && <span className="wb-ab" title="Commits to pull"><IconArrowDown size={11} />{status.behind}</span>}
        <span className="grow" />
        {status.head && <span className="wb-meta mono">{status.head}</span>}
      </div>
      <div className="wb-commit">
        <textarea
          value={message}
          onChange={(e) => setMessage(e.currentTarget.value)}
          placeholder={`Message (${navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl"}↵ to ${commitLabel.toLowerCase()})`}
          rows={message.includes("\n") ? 4 : 2}
          onKeyDown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && message.trim() && total) { e.preventDefault(); void commit(); } }}
        />
        <button className="wb-commit-btn" disabled={busy || !message.trim() || total === 0} onClick={() => void commit()}>
          <IconGitCommit size={15} /> {commitLabel}
        </button>
      </div>
      <div className="wb-scroll">
        {total === 0 && <div className="wb-empty"><IconCheck size={22} className="wb-ok" /><strong>Working tree clean</strong><span>Changes the agent makes show up here.</span></div>}
        {(["conflicted", "staged", "changes"] as Group[]).map((group) => {
          const files = groups[group];
          if (!files.length) return null;
          const shut = collapsed.has(group);
          return (
            <section key={group} className="wb-group">
              <div className="wb-group-head">
                <button className="wb-group-toggle" onClick={() => setCollapsed((old) => { const next = new Set(old); if (next.has(group)) next.delete(group); else next.add(group); return next; })}>
                  <IconChevronRight size={13} className={`wb-chevron${shut ? "" : " open"}`} />
                  <span>{TITLE[group]}</span>
                  <span className="wb-count">{files.length}</span>
                </button>
                <div className="wb-row-actions">
                  {group === "changes" && <button className="wb-icon-btn" title="Discard all changes" disabled={busy} onClick={() => void discard(files)}><IconArrowBackUp size={14} /></button>}
                  {group === "staged"
                    ? <button className="wb-icon-btn" title="Unstage all" disabled={busy} onClick={() => void unstage(files.map((f) => f.path))}><IconMinus size={14} /></button>
                    : <button className="wb-icon-btn" title="Stage all" disabled={busy} onClick={() => void stage(files.map((f) => f.path))}><IconPlus size={14} /></button>}
                </div>
              </div>
              {!shut && files.map((file) => {
                const [dir, name] = splitPath(file.path);
                const staged = group === "staged";
                return (
                  <div key={`${group}:${file.path}`} className="wb-change" role="button" tabIndex={0}
                    onClick={() => setOpen({ path: file.path, staged })} onKeyDown={(e) => { if (e.key === "Enter") setOpen({ path: file.path, staged }); }}
                    title={`${file.path} — ${STATUS_LABEL[file.status] ?? file.status}`}>
                    <FileIcon path={file.path} />
                    <span className={`wb-change-name truncate${file.status === "D" ? " deleted" : ""}`}>{name}</span>
                    <span className="wb-tree-dir truncate">{dir}</span>
                    <span className="grow" />
                    <Stat additions={file.additions} deletions={file.deletions} />
                    <div className="wb-row-actions" onClick={(e) => e.stopPropagation()}>
                      {group === "changes" && <button className="wb-icon-btn" title="Discard changes" disabled={busy} onClick={() => void discard([file])}><IconArrowBackUp size={14} /></button>}
                      {staged
                        ? <button className="wb-icon-btn" title="Unstage" disabled={busy} onClick={() => void unstage([file.path])}><IconMinus size={14} /></button>
                        : <button className="wb-icon-btn" title="Stage" disabled={busy} onClick={() => void stage([file.path])}><IconPlus size={14} /></button>}
                    </div>
                    <span className={`wb-letter s-${statusLetter(file)}`}>{statusLetter(file)}</span>
                  </div>
                );
              })}
            </section>
          );
        })}
        {status.truncated && <div className="wb-empty faint">Showing the first 2,000 changes.</div>}
      </div>
    </div>
  );
}

const SPLIT_KEY = "cabin.diffSplit";

function DiffView({ root, path, staged, file, busy, onBack, onToggleStage }: {
  root: WorkbenchRoot; path: string; staged: boolean; file: GitFileStatus | null; busy: boolean; onBack: () => void; onToggleStage: () => void;
}) {
  const [diff, setDiff] = useState<GitFileDiff | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [split, setSplit] = useState(() => { try { return localStorage.getItem(SPLIT_KEY) === "1"; } catch { return false; } });
  const stamp = `${file?.additions}:${file?.deletions}:${file?.status}`;
  useEffect(() => {
    workbenchIpc.gitDiff(root, path, staged).then((next) => { setDiff(next); setError(null); }, (err) => setError(toIpcError(err).message));
  }, [path, staged, stamp]); // eslint-disable-line react-hooks/exhaustive-deps
  const [dir, name] = splitPath(path);
  const toggleSplit = () => { setSplit(!split); try { localStorage.setItem(SPLIT_KEY, split ? "0" : "1"); } catch { /* per viewer */ } };
  return (
    <div className="wb-viewer">
      <div className="wb-viewer-head">
        <button className="wb-icon-btn" aria-label="Back to changes" title="Back to changes" onClick={onBack}><IconArrowLeft size={15} /></button>
        <FileIcon path={path} />
        <div className="wb-viewer-title truncate" title={path}><strong>{name}</strong>{dir && <span className="faint"> {dir}</span>}</div>
        <span className="wb-meta">{staged ? "Staged" : file?.status === "?" ? "Untracked" : "Working tree"}</span>
        <button className="wb-icon-btn" title={split ? "Inline diff" : "Side-by-side diff"} onClick={toggleSplit}>{split ? <IconLayoutRows size={15} /> : <IconLayoutColumns size={15} />}</button>
        {file && <button className="wb-chip" disabled={busy} onClick={onToggleStage}>{staged ? "Unstage" : "Stage"}</button>}
      </div>
      <div className="wb-scroll wb-viewer-body">
        {error && <div className="wb-empty"><strong>Diff unavailable</strong><span>{error}</span></div>}
        {!diff && !error && <div className="wb-empty faint">Loading diff…</div>}
        {diff && (diff.binary ? <div className="wb-empty"><strong>Binary file changed</strong></div>
          : !diff.patch ? <div className="wb-empty faint">No textual changes (mode or rename only).</div>
            : <div className="wb-diff"><Diff patch={diff.patch} path={path} toolbar={false} format={split ? "side-by-side" : "line-by-line"} />{diff.truncated && <div className="wb-empty faint">Diff truncated.</div>}</div>)}
      </div>
    </div>
  );
}
