import { IconAdjustments, IconDeviceDesktop, IconDeviceSdCard, IconShieldLock, IconTool, IconWorld } from "@tabler/icons-react";
import { useMacPermissions } from "../state/macPermissions";
import { PermissionsSettings } from "./settings/PermissionsSettings";
import type { ReactNode } from "react";
import { MachineGate } from "../components/MachineGate";
import { selectNetworkSummary } from "../lib/networkSummary";
import { useApp } from "../state/store";
import { SETTINGS_TABS, type SettingsTab, uiActions } from "../state/ui";
import { GeneralSettings } from "./settings/GeneralSettings";
import { MachineSettings } from "./settings/MachineSettings";
import { NetworkSettings } from "./settings/NetworkSettings";
import { StorageSettings } from "./settings/StorageSettings";
import { ToolsSettings } from "./settings/ToolsSettings";

const TABS: Record<SettingsTab, { label: string; icon: typeof IconAdjustments; guest: boolean }> = {
  general: { label: "General", icon: IconAdjustments, guest: true },
  network: { label: "Network", icon: IconWorld, guest: false },
  tools: { label: "Tools", icon: IconTool, guest: true },
  machine: { label: "Machine", icon: IconDeviceDesktop, guest: false },
  storage: { label: "Storage", icon: IconDeviceSdCard, guest: false },
  permissions: { label: "Folder permissions", icon: IconShieldLock, guest: false },
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
    <div className="flex min-h-0 flex-1 max-[760px]:flex-col">
      <nav className="flex w-52 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-line/70 px-2.5 py-6 max-[760px]:w-auto max-[760px]:flex-row max-[760px]:flex-wrap max-[760px]:border-r-0 max-[760px]:border-b max-[760px]:py-2.5" aria-label="Settings">
        <h1 className="mx-2.5 mb-2.5 text-[13px] font-semibold text-muted max-[760px]:hidden">Settings</h1>
        {SETTINGS_TABS.map((key) => {
          const { label, icon: Icon } = TABS[key];
          const active = key === tab;
          return (
            <button
              key={key}
              className={`press flex w-full cursor-pointer items-center gap-2.5 rounded-lg border px-2.5 py-[7px] text-left text-md whitespace-nowrap text-fg max-[760px]:w-auto ${active ? "glass font-medium" : "border-transparent bg-transparent hover:bg-hover"}`}
              aria-current={active ? "page" : undefined}
              onClick={() => uiActions.navigate({ kind: "settings", tab: key })}
            >
              <Icon size={16} stroke={1.7} className={`shrink-0 transition-colors duration-200 ${active ? "text-accent" : "text-muted"}`} />
              <span className="min-w-0 flex-1">{label}</span>
              {attention[key] && <span className="dot warn" aria-label="Needs attention" />}
            </button>
          );
        })}
      </nav>
      <div key={tab} className="view-enter flex min-h-0 min-w-0 flex-1 flex-col [container-type:inline-size]">
        {guest ? <MachineGate>{content}</MachineGate> : content}
      </div>
    </div>
  );
}

/** Header + body of one settings tab: a readable column, the title and its actions first. */
export function SettingsPage({ title, subtitle, actions, children }: {
  title: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-[clamp(16px,4cqi,32px)] pt-7 pb-8 [scrollbar-gutter:stable_both-edges]">
      <div className="mx-auto w-full max-w-[760px]">
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
