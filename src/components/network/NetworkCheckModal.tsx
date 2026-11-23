import { Loader, Modal } from "@mantine/core";
import { useMemo, useRef, useState } from "react";
import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { networkTimeline } from "../../lib/networkTimeline";
import { formatDateTime, isoTimestamp } from "../../lib/time";
import { toIpcError } from "../../services/ipc/common";
import type { NetworkHistoryEntry } from "../../services/ipc/generated/core";
import { networkIpc } from "../../services/ipc/network";
import { useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { NetworkGatePanel } from "./NetworkGatePanel";

const PAGE_SIZE = 100;
const HISTORY_LIMIT = 500;

function NetworkChangeTimeline({ reportId }: { reportId: string | undefined }) {
  const [entries, setEntries] = useState<NetworkHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useAutoRefresh(async (isCurrent) => {
    try {
      const latest: NetworkHistoryEntry[] = [];
      const previous = entries ?? [];
      let beforeId: number | null = null;
      while (latest.length < HISTORY_LIMIT) {
        const page = await networkIpc.history(beforeId, PAGE_SIZE);
        if (!isCurrent()) return;
        latest.push(...page);
        if (page.length < PAGE_SIZE) break;
        beforeId = page[page.length - 1].id;
        const overlap = previous.findIndex((entry) => entry.id === beforeId);
        if (overlap >= 0) {
          const remaining = HISTORY_LIMIT - latest.length;
          latest.push(...previous.slice(overlap + 1, overlap + 1 + remaining));
          break;
        }
      }
      setEntries(latest);
      setError(null);
    } catch (error) {
      if (isCurrent()) setError(toIpcError(error).message);
    }
  }, { interval: 30_000, refreshKey: reportId });

  const timeline = useMemo(() => entries ? networkTimeline(entries) : [], [entries]);
  const feedback = "flex items-center gap-2 text-sm text-faint";
  return (
    <section aria-labelledby="network-change-heading" className="mt-3 grid gap-2 border-t border-line/70 pt-3">
      <h3 id="network-change-heading" className="m-0 text-sm font-medium">IP and environment changes</h3>
      {error && <div className={feedback} role="alert">{error}</div>}
      {!error && !entries && <div className={feedback} role="status"><Loader size={14} /> Loading changes</div>}
      {!error && entries && timeline.length === 0 && <div className={feedback}>No changes recorded.</div>}
      {timeline.length > 0 && (
        <ol className="m-0 max-h-80 list-none overflow-y-auto pl-1 pr-2">
          {timeline.map(({ id, at, changes }) => (
            <li key={id} className="relative grid gap-1 border-l border-line py-2 pl-4 text-sm">
              <span className="absolute -left-[4px] top-3 size-[7px] rounded-full bg-muted" aria-hidden="true" />
              <time className="text-xs text-faint" dateTime={isoTimestamp(at)}>{formatDateTime(at)}</time>
              {changes.map(({ label, before, after }) => (
                <div key={label} className="min-w-0 break-words">
                  <span className="font-medium">{label}</span>{" "}
                  <span className="text-muted">{before}</span>{" → "}{after}
                </div>
              ))}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

export function NetworkCheckModal() {
  const opened = useUi((state) => state.networkCheckOpen);
  const status = useApp((state) => state.network);
  const destination = useRef<"route" | "region" | "browser" | null>(null);

  return (
    <Modal
      opened={opened}
      onClose={uiActions.closeNetworkCheck}
      onExitTransitionEnd={() => {
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
      {opened && <NetworkChangeTimeline reportId={status?.report?.id} />}
    </Modal>
  );
}
