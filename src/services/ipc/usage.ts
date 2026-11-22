import { invokeGuest } from "./common";
import type { UsageDashboard, UsageFilter, UsagePrice, UsageSyncResult } from "./generated/core";

export type { UsageCounts, UsageDaily, UsageDashboard, UsageFilter, UsagePrice } from "./generated/core";

export const usageIpc = {
  dashboard: (filter: UsageFilter) => invokeGuest<UsageDashboard>("usage_dashboard", { filter }),
  sync: () => invokeGuest<UsageSyncResult>("usage_sync"),
  savePrice: (price: UsagePrice) => invokeGuest<void>("usage_save_price", { price }),
};
