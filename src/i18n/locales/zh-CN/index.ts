import type { LocaleCatalog } from "../en/index.ts";
import { commonZhCN } from "./common.ts";
import { navZhCN } from "./nav.ts";
import { homeZhCN } from "./home.ts";
import { settingsZhCN } from "./settings.ts";
import { networkZhCN } from "./network.ts";
import { featuresZhCN } from "./features.ts";

export const zhCN = {
  ...commonZhCN,
  ...navZhCN,
  ...homeZhCN,
  ...settingsZhCN,
  ...networkZhCN,
  ...featuresZhCN,
} as const satisfies LocaleCatalog;
