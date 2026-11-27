import { commonEn } from "./common.ts";
import { navEn } from "./nav.ts";
import { homeEn } from "./home.ts";
import { settingsEn } from "./settings.ts";
import { networkEn } from "./network.ts";
import { featuresEn } from "./features.ts";

export const en = {
  ...commonEn,
  ...navEn,
  ...homeEn,
  ...settingsEn,
  ...networkEn,
  ...featuresEn,
} as const;

export type EnMessages = typeof en;
export type MessageKey = keyof EnMessages;
export type LocaleCatalog = Record<MessageKey, string>;
