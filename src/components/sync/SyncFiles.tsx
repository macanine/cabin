import { Checkbox, Collapse, Loader, SegmentedControl, Switch, Textarea } from "@mantine/core";
import { IconAlertTriangle, IconChevronRight, IconFile, IconFolder } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { SyncMode, SyncPreview, SyncRules } from "../../services/ipc/generated/core";
import { syncIpc } from "../../services/ipc/sync";
import { toIpcError, type IpcError } from "../../services/ipc/common";
import { isFolderAccessError } from "../../services/ipc/permissions";
import { FolderAccessHelp } from "./FolderAccessHelp";
import { formatBytes, formatInteger } from "../../lib/format";
import { GIT_CHOICES, gitWarning, parsePatterns, previewSummary, REASON_LABEL, toggleExcluded } from "../../lib/syncForm";

/**
 * Which files of a Mac folder take part: the top level with a tick per item
 * (counts and sizes from the real rules), why the rest is left out, the
 * `.gitignore` switch, the `.git` choice and extra patterns.
 */
export function SyncFiles({ hostPath, mode, rules, onRules, onPreview, onProblem, retry = 0 }: {
  hostPath: string;
  mode: SyncMode;
  rules: SyncRules;
  onRules: (rules: SyncRules) => void;
  onPreview?: (preview: SyncPreview | null) => void;
  onProblem?: (problem: IpcError | null) => void;
  retry?: number;
}) {
  const [preview, setPreview] = useState<SyncPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);
  const [revision, setRevision] = useState(0);
  const [patternsOpen, setPatternsOpen] = useState(rules.ignore.length > 0);
  const [patterns, setPatterns] = useState(rules.ignore.join("\n"));

  const accessError = isFolderAccessError(error);
  const recheck = () => setRevision((value) => value + 1);
  useEffect(() => {
    if (!accessError || onProblem) return;
    window.addEventListener("focus", recheck);
    return () => window.removeEventListener("focus", recheck);
  }, [accessError, onProblem]);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setPreview(null);
    setError(null);
    onPreview?.(null);
    onProblem?.(null);
    const timer = window.setTimeout(() => {
      syncIpc.preview(hostPath, rules, mode)
        .then((next) => { if (alive) { setPreview(next); onPreview?.(next); } })
        .catch((cause) => {
          if (!alive) return;
          const issue = toIpcError(cause);
          setError(issue);
          onProblem?.(issue);
        })
        .finally(() => { if (alive) setLoading(false); });
    }, 250);
    return () => { alive = false; window.clearTimeout(timer); };
  }, [hostPath, mode, rules, onPreview, onProblem, retry, revision]);

  const warning = gitWarning(mode, rules.git);

  return (
    <div className="sync-files">
      <div className="sync-files-head">
        <strong>Files</strong>
        <span className="faint">{loading ? <Loader size={10} /> : preview ? previewSummary(preview) : ""}</span>
      </div>
      {error && !onProblem && (accessError ? (
        <FolderAccessHelp key={hostPath} path={hostPath} needsWrite={mode === "twoWay"} message={error.message}
          privacy={error.code === "mac_folder_privacy"} onCheck={recheck} />
      ) : <div className="callout bad">{error.message}</div>)}
      <ul className="sync-files-list">
        {preview?.items.map((item) => {
          const unticked = item.reason === "unticked";
          const locked = item.reason === "ignored" || item.reason === "git";
          return (
            <li key={item.name} className={item.included ? "" : "out"}>
              <Checkbox
                size="xs"
                checked={item.included}
                disabled={locked}
                onChange={(event) => onRules(toggleExcluded(rules, item.name, event.currentTarget.checked))}
                aria-label={`Sync ${item.name}`}
              />
              {item.dir ? <IconFolder size={14} className="sync-files-icon" /> : <IconFile size={14} className="sync-files-icon" />}
              <span className="sync-files-name truncate">{item.name}</span>
              {item.included ? (
                <span className="sync-files-size">{item.dir ? `${formatInteger(item.files)} · ` : ""}{formatBytes(item.bytes)}</span>
              ) : (
                <span className={`sync-files-tag ${item.reason ?? ""}`}>{unticked ? "left out" : REASON_LABEL[item.reason ?? "ignored"] ?? item.reason}</span>
              )}
            </li>
          );
        })}
        {!preview && !error && <li className="faint">Reading the folder…</li>}
        {preview && preview.items.length === 0 && <li className="faint">This folder is empty.</li>}
      </ul>

      <div className="sync-files-options">
        <Switch
          size="xs"
          label={`Leave out what .gitignore ignores${preview && !preview.gitignore ? " (no .gitignore here)" : ""}`}
          checked={rules.gitignore}
          onChange={(event) => onRules({ ...rules, gitignore: event.currentTarget.checked })}
        />
        {(preview?.git ?? true) && (
          <div className="sync-git">
            <span className="sync-git-label">Git history</span>
            <SegmentedControl
              size="xs"
              value={rules.git}
              onChange={(value) => onRules({ ...rules, git: value as SyncRules["git"] })}
              data={GIT_CHOICES.map((choice) => ({ value: choice.value, label: choice.label }))}
            />
          </div>
        )}
        {(preview?.git ?? true) && <p className="sync-git-detail">{GIT_CHOICES.find((c) => c.value === rules.git)?.detail}</p>}
        {warning && <div className="sync-warning"><IconAlertTriangle size={14} /> {warning}</div>}
        <button type="button" className="sync-more" aria-expanded={patternsOpen} onClick={() => setPatternsOpen((v) => !v)}>
          <IconChevronRight size={12} className={patternsOpen ? "open" : undefined} /> More patterns to leave out
          {rules.ignore.length > 0 && <span className="faint"> · {rules.ignore.length}</span>}
        </button>
        <Collapse expanded={patternsOpen}>
          <Textarea
            size="xs"
            autosize
            minRows={2}
            maxRows={6}
            placeholder={"*.log\ndist/\n.env.local"}
            description="One per line, .gitignore syntax. A .cabinignore file in the folder works too."
            value={patterns}
            onChange={(event) => setPatterns(event.currentTarget.value)}
            onBlur={() => onRules({ ...rules, ignore: parsePatterns(patterns) })}
            styles={{ input: { fontFamily: "var(--mono)" } }}
          />
        </Collapse>
      </div>
    </div>
  );
}
