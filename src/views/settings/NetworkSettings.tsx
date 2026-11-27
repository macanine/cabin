import { Button, Select, Switch } from "@mantine/core";
import { IconCheck } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { EnvironmentReview, EnvironmentSpec } from "../../services/ipc/generated/core";
import { environmentIpc } from "../../services/ipc/environment";
import { networkIpc } from "../../services/ipc/network";
import { patchState, useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { attempt, runOperation } from "../../lib/operations";
import { SettingGroup, SettingRow } from "../../components/common";
import { SettingsPage } from "../SettingsView";
import { syncClock } from "../../state/clockSync";
import { NetworkDashboard } from "../../components/network/NetworkDashboard";
import { useExitMonitor } from "../../state/networkMonitor";
import { baseLocale, environmentMatch } from "../../lib/geo";
import { useI18n } from "../../i18n";

const localeLabel = (locale: string | null | undefined) => locale?.split(".")[0].replace("_", "-") ?? "unknown";

/** "Matches exit" / "Differs" for one setting, when the exit is known. */
function MatchPill({ matches }: { matches: boolean | null }) {
  const { t } = useI18n();
  if (matches == null) return null;
  return matches ? (
    <span className="inline-flex items-center gap-1 rounded-full bg-ok/12 px-2 py-0.5 text-xs font-medium text-ok">
      <IconCheck size={12} stroke={2.5} aria-hidden="true" />{t("network.matchesExit")}
    </span>
  ) : (
    <span className="rounded-full bg-warn/14 px-2 py-0.5 text-xs font-medium text-warn">{t("network.differsExit")}</span>
  );
}

/** One region setting: label and match state, then the exit's value and its fix, with the control on the right. */
function RegionRow({ label, matches, children, footer }: {
  label: string;
  matches: boolean | null;
  children: React.ReactNode;
  footer?: React.ReactNode;
}) {
  return (
    <div className="set-row">
      <div className="grid min-w-0 flex-[1_1_240px] gap-1">
        <span className="flex flex-wrap items-center gap-2 text-md font-medium">{label}<MatchPill matches={matches} /></span>
        {footer && <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">{footer}</div>}
      </div>
      <div className="w-56 max-w-full">{children}</div>
    </div>
  );
}

/**
 * The VM's time zone and language next to the verified exit's. With "Follow
 * exit" on, the Mac sets the time zone to the exit's during each network
 * check (the check only passes when they match). Language is set by hand;
 * edits apply together, in one guest operation.
 */
function Region() {
  const env = useApp((state) => state.environment);
  const network = useApp((state) => state.network);
  const running = useApp((state) =>
    state.operations.some((op) => op.kind === "environment.apply" && (op.state === "running" || op.state === "pending")),
  );
  const exit = useExitMonitor().value;
  const [edits, setEdits] = useState<Partial<EnvironmentSpec>>({});
  const [review, setReview] = useState<EnvironmentReview | null>(null);
  const [toggling, setToggling] = useState(false);

  const current: EnvironmentSpec | null = env
    ? { timezone: env.systemTimezone ?? env.committed.timezone, language: env.systemLanguage ?? env.committed.language }
    : null;
  const next: EnvironmentSpec | null = current ? { ...current, ...edits } : null;
  const nextKey = next ? `${next.timezone}|${next.language}` : "";

  useEffect(() => {
    if (!next) return;
    const timer = setTimeout(() => {
      void environmentIpc.review(next).then(setReview).catch(() => undefined);
    }, 200);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nextKey]);

  if (!env || !current || !next) return null;
  const follow = network.regionFollowsExit;
  const match = environmentMatch(exit, current.timezone, current.language);
  const exitLanguage = match.exitLocale
    ? review?.languages.find((language) => baseLocale(language) === match.exitLocale) ?? null
    : null;
  const languageField = review?.fields.find((field) => field.field === "language");
  const dirty = next.timezone !== current.timezone || next.language !== current.language;
  const blocked = dirty ? review?.blocked ?? null : null;

  async function setFollow(enabled: boolean) {
    setToggling(true);
    const status = await attempt(networkIpc.setRegionFollow(enabled));
    if (status) patchState((state) => ({ ...state, network: status }));
    setToggling(false);
    // Follow now rather than at the next scheduled check.
    if (status && enabled) void networkIpc.verify().catch(() => undefined);
  }

  async function apply() {
    if (!next) return;
    const record = await runOperation(environmentIpc.apply(next), "Region applied");
    if (record?.state === "succeeded") {
      setEdits({});
      void syncClock(true);
      // The gate checks the VM's region; check again now instead of waiting.
      void networkIpc.verify().catch(() => undefined);
    }
  }

  const exitValue = (value: string) => <span className="mono text-fg">{value}</span>;
  return (
    <div className="net-section">
      <header className="net-section-head">
        <div>
          <h3>VM region</h3>
          <p>Keeps Ubuntu’s time zone and language aligned with your verified exit location.</p>
        </div>
      </header>
      <SettingGroup>
      <SettingRow label="Follow exit" hint="Set the machine's time zone to the verified exit's during each network check.">
        <Switch
          size="sm"
          aria-label="Follow exit"
          checked={follow}
          disabled={toggling || !network.available}
          onChange={(event) => void setFollow(event.currentTarget.checked)}
        />
      </SettingRow>
        <RegionRow
          label="Time zone"
          matches={match.zoneMatches}
          footer={follow ? (
            match.zoneMatches === false && match.exitZone ? (
              <>
                <span className="grow">Changes to {exitValue(match.exitZone)} at the next check</span>
                <Button size="compact-xs" variant="light" onClick={() => void attempt(networkIpc.verify())}>Check now</Button>
              </>
            ) : <span>Follows the exit automatically</span>
          ) : match.zoneMatches === false && match.exitZone ? (
            <>
              <span className="grow">Exit uses {exitValue(match.exitZone)}</span>
              <Button size="compact-xs" variant="light" onClick={() => setEdits((old) => ({ ...old, timezone: match.exitZone ?? old.timezone }))}>Use</Button>
            </>
          ) : null}
        >
          {follow ? (
            <div className="flex h-[30px] items-center justify-end truncate text-md font-medium" title={current.timezone}>{current.timezone}</div>
          ) : (
            <Select
              size="xs"
              searchable
              data={review?.timezones ?? [next.timezone]}
              value={next.timezone}
              onChange={(value) => value && setEdits((old) => ({ ...old, timezone: value }))}
              disabled={running}
              limit={200}
              aria-label="Time zone"
            />
          )}
        </RegionRow>
        <RegionRow
          label="Language"
          matches={match.localeMatches}
          footer={match.localeMatches === false && match.exitLocale ? (
            exitLanguage ? (
              <>
                <span className="grow">Exit uses {exitValue(localeLabel(match.exitLocale))}</span>
                <Button size="compact-xs" variant="light" onClick={() => setEdits((old) => ({ ...old, language: exitLanguage }))}>Use</Button>
              </>
            ) : (
              <span>Exit uses {exitValue(localeLabel(match.exitLocale))} · {languageField?.reason ?? "language packs arrive with the base tools"}</span>
            )
          ) : dirty && languageField?.reason ? <span>{languageField.reason}</span> : null}
        >
          <Select
            size="xs"
            searchable
            data={review?.languages.length ? review.languages : [next.language]}
            value={next.language}
            onChange={(value) => value && setEdits((old) => ({ ...old, language: value }))}
            disabled={running || languageField?.enabled === false}
            limit={200}
            aria-label="Language"
          />
        </RegionRow>
      {(dirty || running) && (
        <div className="flex flex-wrap items-center gap-2 rounded-b-[var(--card-radius)] border-t border-line bg-hover/60 px-4 py-2.5">
          <span className={`grow text-sm ${blocked ? "text-bad" : "text-muted"}`}>
            {blocked ?? [
              next.timezone !== current.timezone ? `Time zone → ${next.timezone}` : null,
              next.language !== current.language ? `Language → ${localeLabel(next.language)}` : null,
            ].filter(Boolean).join(" · ")}
          </span>
          <Button size="xs" variant="default" disabled={running} onClick={() => setEdits({})}>Discard</Button>
          <Button size="xs" loading={running} disabled={blocked != null || !dirty} onClick={() => void apply()}>Apply</Button>
        </div>
      )}
    </SettingGroup>
    </div>
  );
}

export function NetworkSettings() {
  const { t } = useI18n();
  const regionReady = useApp((state) => state.guestReady && state.environment != null);
  const focus = useUi((state) => state.networkSettingsFocus);

  useEffect(() => {
    if (!focus) return;
    const section = document.getElementById(`network-${focus}`);
    section?.focus({ preventScroll: true });
    section?.scrollIntoView({ block: "start" });
    uiActions.clearNetworkSettingsFocus();
  }, [focus]);

  return (
    <SettingsPage wide title={t("network.title")} subtitle={t("network.subtitle")}>
      <NetworkDashboard />
      <section id="network-region" className="mt-5 scroll-mt-4 outline-none" tabIndex={-1} aria-label={t("network.vmRegion")}>
        {regionReady ? <Region /> : (
          <div className="net-section">
            <header className="net-section-head">
              <div>
                <h3>{t("network.vmRegion")}</h3>
                <p>{t("network.vmRegionSubtitle")}</p>
              </div>
            </header>
            <SettingGroup><SettingRow label={t("network.timeZoneAndLang")} hint={t("network.regionUnavailable")} /></SettingGroup>
          </div>
        )}
      </section>
    </SettingsPage>
  );
}
