import { IconChevronRight } from "@tabler/icons-react";
import { Loader, Modal } from "@mantine/core";
import { useMemo, useRef, useState } from "react";
import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { networkTimeline } from "../../lib/networkTimeline";
import { formatDateTime, isoTimestamp } from "../../lib/time";
import type { NetworkHistoryEntry } from "../../services/ipc/generated/core";
import { networkIpc } from "../../services/ipc/network";
import { useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { NetworkGatePanel } from "./NetworkGatePanel";

function NetworkChangeTimeline({ reportId }: { reportId: string | undefined }) {
  const [entries, setEntries] = useState<NetworkHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useAutoRefresh(async (isCurrent) => {
    try {
      const latest = await networkIpc.history();
      if (!isCurrent()) return;
      setEntries(latest);
      setError(null);
    } catch {
      if (isCurrent()) setError("Connection history is temporarily unavailable.");
    }
  }, { interval: 30_000, refreshKey: reportId });

  const timeline = useMemo(() => entries ? networkTimeline(entries) : [], [entries]);
  const feedback = "flex items-center gap-2 text-sm text-faint";
  return (
    <section aria-label="IP and environment changes" className="mt-2 grid gap-2 border-l border-line pl-3">
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
  const [historyOpen, setHistoryOpen] = useState(false);
  const destination = useRef<"route" | "region" | "browser" | null>(null);

  return (
    <Modal
      opened={opened}
      onClose={() => { setHistoryOpen(false); uiActions.closeNetworkCheck(); }}
      onExitTransitionEnd={() => {
        const target = destination.current;
        destination.current = null;
        if (target === "browser") uiActions.openBrowserEnvironment();
        else if (target) uiActions.focusNetworkSettings(target);
      }}
      title="Network check"
      closeButtonProps={{ "aria-label": "Close" }}
      size={488}
      padding="lg"
      centered
    >
      {opened && <NetworkGatePanel status={status} onRemediate={(target) => {
        setHistoryOpen(false);
        destination.current = target;
        uiActions.closeNetworkCheck();
      }} history={(
        <details className="group/history text-sm text-muted" open={historyOpen} onToggle={(event) => setHistoryOpen(event.currentTarget.open)}>
          <summary className="flex cursor-pointer list-none items-center gap-1.5 font-medium hover:text-fg [&::-webkit-details-marker]:hidden">
            <IconChevronRight size={13} className="group-open/history:rotate-90" /> Connection history
          </summary>
          {historyOpen && <NetworkChangeTimeline reportId={status.report?.id} />}
        </details>
      )} />}
    </Modal>
  );
}
