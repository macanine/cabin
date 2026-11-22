import { createContext, type ReactNode, useContext, useEffect, useState } from "react";
import { hostZone, useClock } from "../../lib/clock";
import { useAutoRefresh } from "../../lib/useAutoRefresh";
import { rangeStart } from "../../lib/usageView";
import { toIpcError } from "../../services/ipc/common";
import { usageIpc } from "../../services/ipc/usage";
import { useApp } from "../../state/store";

function useUsageStatus() {
  const enabled = useApp((state) => state.guestReady);
  const { offset: clockOffset } = useClock();
  const zone = hostZone();
  const [revision, setRevision] = useState(0);
  const [importErrors, setImportErrors] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [syncedAt, setSyncedAt] = useState<number | null>(null);
  const [todayCost, setTodayCost] = useState<number | null>(null);

  useAutoRefresh(async (isCurrent) => {
    try {
      const result = await usageIpc.sync();
      if (!isCurrent()) return;
      setImportErrors(result.errors);
      setSyncedAt(Date.now());
      setError(null);
    } catch (cause) {
      if (isCurrent()) setError(toIpcError(cause).message);
    } finally {
      if (isCurrent()) setRevision((value) => value + 1);
    }
  }, { enabled, interval: 60_000, minGap: 20_000 });

  const dayStart = rangeStart("1");
  useEffect(() => {
    if (!enabled) return;
    let current = true;
    const fromMs = dayStart == null ? null : dayStart + clockOffset;
    void usageIpc.dashboard({ fromMs, toMs: null, agent: null, model: null, limit: 1, offset: 0, hostZone: zone, clockOffset })
      .then((result) => { if (current) setTodayCost(result.summary.cost); }, () => undefined);
    return () => { current = false; };
  }, [enabled, revision, dayStart, clockOffset, zone]);

  return {
    revision,
    syncedAt,
    todayCost: enabled ? todayCost : null,
    bump: () => setRevision((value) => value + 1),
    importErrors,
    clearImportErrors: () => setImportErrors([]),
    error,
    clearError: () => setError(null),
  };
}

const UsageContext = createContext<ReturnType<typeof useUsageStatus> | null>(null);

/** The sidebar, start page and report share the same import and daily total. */
export function UsageMonitor({ children }: { children: ReactNode }) {
  const usage = useUsageStatus();
  return <UsageContext.Provider value={usage}>{children}</UsageContext.Provider>;
}

export function useUsage() {
  const usage = useContext(UsageContext);
  if (!usage) throw new Error("UsageMonitor is missing");
  return usage;
}
