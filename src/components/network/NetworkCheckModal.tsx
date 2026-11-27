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
import { useI18n } from "../../i18n";

function NetworkChangeTimeline({ reportId }: { reportId: string | undefined }) {
  const { t } = useI18n();
  const [entries, setEntries] = useState<NetworkHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useAutoRefresh(async (isCurrent) => {
    try {
      const latest = await networkIpc.history();
      if (!isCurrent()) return;
      setEntries(latest);
      setError(null);
    } catch {
      if (isCurrent()) setError(t("network.historyUnavailable"));
    }
  }, { interval: 30_000, refreshKey: reportId });

  const timeline = useMemo(() => entries ? networkTimeline(entries) : [], [entries]);
  const feedback = "flex items-center gap-2 text-sm text-faint";
  return (
    <section aria-label="IP and environment changes" className="mt-2.5 grid gap-2">
      {error && <div className={feedback} role="alert">{error}</div>}
      {!error && !entries && <div className={feedback} role="status"><Loader size={14} /> {t("network.loadingChanges")}</div>}
      {!error && entries && timeline.length === 0 && <div className={feedback}>{t("network.noChangesRecorded")}</div>}
      {timeline.length > 0 && (
        <ol className="m-0 max-h-80 list-none overflow-y-auto py-0.5 pr-2 pl-1">
          {timeline.map(({ id, at, changes }, index) => {
            const isLast = index === timeline.length - 1;
            return (
              <li key={id} className={`relative grid gap-1 pl-5 text-sm ${index === 0 ? "pt-0.5" : "pt-1"} ${isLast ? "pb-1" : "pb-3.5"}`}>
                {!isLast && <span className="absolute top-[13px] bottom-[-6px] left-[3.5px] w-px bg-line" aria-hidden="true" />}
                <span className={`absolute top-[7px] left-0 size-2 rounded-full ring-2 ring-surface ${index === 0 ? "bg-accent" : "bg-muted/70"}`} aria-hidden="true" />
                <time className="text-xs leading-none text-faint" dateTime={isoTimestamp(at)}>{formatDateTime(at)}</time>
                <div className="mt-0.5 grid gap-0.5">
                  {changes.map(({ label, before, after }) => (
                    <div key={label} className="min-w-0 break-words leading-snug">
                      <span className="font-medium text-fg">{label}</span>{" "}
                      <span className="text-muted">{before}</span>{" → "}<span className="text-fg">{after}</span>
                    </div>
                  ))}
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}

export function NetworkCheckModal() {
  const { t } = useI18n();
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
      title={t("network.check")}
      closeButtonProps={{ "aria-label": t("common.close") }}
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
            <IconChevronRight size={13} className="group-open/history:rotate-90" /> {t("network.connectionHistory")}
          </summary>
          {historyOpen && <NetworkChangeTimeline reportId={status.report?.id} />}
        </details>
      )} />}
    </Modal>
  );
}
