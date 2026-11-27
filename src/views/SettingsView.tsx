import { IconAdjustments, IconDeviceDesktop, IconDeviceSdCard, IconPlugConnected, IconShieldLock, IconTerminal2, IconTool, IconWorld } from "@tabler/icons-react";
import { useMacPermissions } from "../state/macPermissions";
import { PermissionsSettings } from "./settings/PermissionsSettings";
import type { ReactNode } from "react";
import { MachineGate } from "../components/MachineGate";
import { ViewHeader } from "../components/TitleBar";
import { selectNetworkSummary } from "../lib/networkSummary";
import { useApp } from "../state/store";
import { SETTINGS_TABS, type SettingsTab, uiActions } from "../state/ui";
import { GeneralSettings } from "./settings/GeneralSettings";
import { HostCliSettings } from "./settings/HostCliSettings";
import { MachineSettings } from "./settings/MachineSettings";
import { NetworkSettings } from "./settings/NetworkSettings";
import { RemoteSettings } from "./settings/RemoteSettings";
import { StorageSettings } from "./settings/StorageSettings";
import { ToolsSettings } from "./settings/ToolsSettings";
import { type StaticMessageKey, useI18n } from "../i18n";

const TABS: Record<SettingsTab, { labelKey: StaticMessageKey; icon: typeof IconAdjustments; guest: boolean }> = {
  general: { labelKey: "settings.tab.general", icon: IconAdjustments, guest: true },
  network: { labelKey: "settings.tab.network", icon: IconWorld, guest: false },
  remote: { labelKey: "settings.tab.remote", icon: IconPlugConnected, guest: false },
  cli: { labelKey: "settings.tab.cli", icon: IconTerminal2, guest: false },
  tools: { labelKey: "settings.tab.tools", icon: IconTool, guest: true },
  machine: { labelKey: "settings.tab.machine", icon: IconDeviceDesktop, guest: false },
  storage: { labelKey: "settings.tab.storage", icon: IconDeviceSdCard, guest: false },
  permissions: { labelKey: "settings.tab.permissions", icon: IconShieldLock, guest: false },
};

/** Tabs that need the user's attention get a dot; everything else stays quiet. */
function useAttention(): Partial<Record<SettingsTab, boolean>> {
  const machine = useApp((state) => state.machine);
  const needsNetworkAttention = useApp((state) => selectNetworkSummary(state).tone === "bad");
  const { projects } = useMacPermissions();
  return {
    permissions: projects?.some(({ access }) => access.state !== "available"),
    network: needsNetworkAttention,
    machine: machine != null && (machine.error != null || machine.state === "notInstalled"),
  };
}

export function SettingsView({ tab = "general" }: { tab?: SettingsTab }) {
  const { t } = useI18n();
  const attention = useAttention();
  const { guest } = TABS[tab];
  let content: ReactNode;
  switch (tab) {
    case "general":
      content = <GeneralSettings />;
      break;
    case "network":
      content = <NetworkSettings />;
      break;
    case "remote":
      content = <RemoteSettings />;
      break;
    case "cli":
      content = <HostCliSettings />;
      break;
    case "tools":
      content = <ToolsSettings />;
      break;
    case "machine":
      content = <MachineSettings />;
      break;
    case "storage":
      content = <StorageSettings />;
      break;
    case "permissions":
      content = <PermissionsSettings />;
      break;
  }

  return (
    <>
    <ViewHeader title={t("settings.title")} />
    <div className="split-page">
      <nav className="split-list !w-[226px] max-[860px]:!w-auto" aria-label={t("settings.title")}>
        {SETTINGS_TABS.map((key) => {
          const { labelKey, icon: Icon } = TABS[key];
          return (
            <button key={key} type="button" className="split-choice press" aria-current={key === tab ? "page" : undefined}
              onClick={() => uiActions.navigate({ kind: "settings", tab: key })}>
              <Icon size={16} stroke={1.7} />
              <span>{t(labelKey)}</span>
              {attention[key] && <span className="dot warn" aria-label="Needs attention" />}
            </button>
          );
        })}
      </nav>
      <div key={tab} className="view-enter flex min-h-0 min-w-0 flex-1 flex-col [container-type:inline-size]">
        {guest ? <MachineGate>{content}</MachineGate> : content}
      </div>
    </div>
    </>
  );
}

/** Header + body of one settings tab: a readable column, the title and its actions first. */
export function SettingsPage({ title, subtitle, actions, children, wide = false }: {
  title: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  wide?: boolean;
}) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-[clamp(16px,4cqi,32px)] pt-7 pb-8 [scrollbar-gutter:stable_both-edges]">
      <div className={`mx-auto w-full ${wide ? "max-w-[940px]" : "max-w-[760px]"}`}>
        <header className="mb-5 flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h2 className="m-0 text-2xl font-semibold tracking-tight">{title}</h2>
            {subtitle && <div className="mt-1 text-md text-muted">{subtitle}</div>}
          </div>
          {actions}
        </header>
        {children}
      </div>
    </div>
  );
}
