import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { Button, Loader, Modal } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import { toIpcError } from "../../services/ipc/common";
import type { NetworkHistoryEntry, NetworkStatus } from "../../services/ipc/generated/core";
import { networkIpc } from "../../services/ipc/network";
import { useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { NetworkCheckReportView, NetworkGatePanel } from "./NetworkGatePanel";
import { formatGuestDateTime, isoTimestamp } from "../../lib/time";
import { hostTime } from "../../lib/clock";
import { DisclosureChevron, Reveal } from "../motion";

const PAGE_SIZE = 25;

function mergeHistory(current: NetworkHistoryEntry[], page: NetworkHistoryEntry[]) {
  const unique = new Map(current.map((entry) => [entry.id, entry]));
  for (const entry of page) unique.set(entry.id, entry);
  return [...unique.values()].sort((a, b) => b.id - a.id);
}

function NetworkHistory({ status, open }: { status: NetworkStatus; open: boolean }) {
  const [entries, setEntries] = useState<NetworkHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  useAutoRefresh(() => setRevision((value) => value + 1), { enabled: open, initial: false, interval: 30_000 });
  const [hasMore, setHasMore] = useState(true);
  const [moreError, setMoreError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  useEffect(() => {
    if (!open) return;
    let active = true;
    void networkIpc.history(null, PAGE_SIZE).then((page) => {
      if (!active) return;
      setEntries((current) => current ? mergeHistory(current, page) : page);
      setError(null);
    }).catch((error: unknown) => {
      if (active) setError(toIpcError(error).message);
    });
    return () => { active = false; };
  }, [open, status, revision]);

  async function older(beforeId: number) {
    setLoadingMore(true);
    setMoreError(null);
    try {
      const page = await networkIpc.history(beforeId, PAGE_SIZE);
      setEntries((current) => mergeHistory(current ?? [], page));
      setHasMore(page.length === PAGE_SIZE);
    } catch (error) {
      setMoreError(toIpcError(error).message);
    } finally {
      setLoadingMore(false);
    }
  }

  const oldestId = entries && entries.length >= PAGE_SIZE ? entries[entries.length - 1].id : null;
  const feedback = "flex items-center gap-2 text-sm text-faint";
  const DOT = { bad: "bg-bad", warn: "bg-warn", ok: "bg-ok" } as const;
  return (
    <div className="grid gap-2 pt-1.5">
      {error ? (
        <div className={feedback} role="alert">{error}</div>
      ) : !entries ? (
        <div className={feedback} role="status"><Loader size={14} /> Loading history</div>
      ) : entries.length === 0 ? (
        <div className={feedback}>No checks yet.</div>
      ) : null}
      {entries && entries.length > 0 && (
        <ol className="m-0 max-h-80 list-none overflow-y-auto p-0">
          {entries.map((entry) => {
            const tone = entry.kind === "blocked" ? "bad" : entry.kind === "rechecking" || entry.failedChecks > 0 ? "warn" : "ok";
            return (
              <li key={entry.id} className="flex items-start gap-2.5 border-b border-line/60 py-2 text-sm last:border-0">
                <span className={`mt-1.5 size-[7px] shrink-0 rounded-full ${DOT[tone]}`} aria-hidden="true" />
                <div className="min-w-0 flex-1">
                  {entry.report ? (
                    <details>
                      <summary className="cursor-pointer"><span>{entry.message}</span>{entry.failedChecks > 0 && <small className="text-faint"> · {entry.failedChecks} failed</small>}</summary>
                      <div className="mt-2 mb-1"><NetworkCheckReportView report={entry.report} /></div>
                    </details>
                  ) : <span>{entry.message}</span>}
                </div>
                <time className="shrink-0 text-xs text-faint" dateTime={isoTimestamp(hostTime(entry.at))}>{formatGuestDateTime(entry.at)}</time>
              </li>
            );
          })}
        </ol>
      )}
      {moreError && <div className={feedback} role="alert">{moreError}</div>}
      {oldestId != null && hasMore && <Button size="xs" variant="default" loading={loadingMore} onClick={() => void older(oldestId)}>Older</Button>}
    </div>
  );
}

export function NetworkCheckModal() {
  const opened = useUi((state) => state.networkCheckOpen);
  const status = useApp((state) => state.network);
  const destination = useRef<"route" | "region" | "browser" | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historySeen, setHistorySeen] = useState(false);

  return (
    <Modal
      opened={opened}
      onClose={uiActions.closeNetworkCheck}
      onExitTransitionEnd={() => {
        setHistoryOpen(false);
        setHistorySeen(false);
        const target = destination.current;
        destination.current = null;
        if (target === "browser") uiActions.openBrowserEnvironment();
        else if (target) uiActions.focusNetworkSettings(target);
      }}
      title="Internet access"
      closeButtonProps={{ "aria-label": "Close" }}
      size="lg"
      centered
    >
      <NetworkGatePanel status={status} onRemediate={(target) => {
        destination.current = target;
        uiActions.closeNetworkCheck();
      }} />
      <div className="mt-3 border-t border-line/70 pt-2">
        <button type="button" aria-expanded={historyOpen} onClick={() => { setHistorySeen(true); setHistoryOpen(!historyOpen); }} className="press -ml-1 inline-flex cursor-pointer items-center gap-1 rounded-md border-0 bg-transparent px-1.5 py-1 text-xs text-faint hover:bg-hover hover:text-muted">
          <DisclosureChevron open={historyOpen} />
          History
        </button>
        <Reveal open={historyOpen}>{historySeen && <NetworkHistory status={status} open={historyOpen} />}</Reveal>
      </div>
    </Modal>
  );
}
