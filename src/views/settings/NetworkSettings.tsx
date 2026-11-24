import { Button, Select, Switch } from "@mantine/core";
import { IconCheck } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { EnvironmentReview, EnvironmentSpec } from "../../services/ipc/generated/core";
import { environmentIpc } from "../../services/ipc/environment";
import { networkIpc } from "../../services/ipc/network";
import { patchState, useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { attempt, runOperation } from "../../lib/operations";
import { Section } from "../../components/common";
import { SettingsPage } from "../SettingsView";
import { syncClock } from "../../state/clockSync";
import { NetworkDashboard } from "../../components/network/NetworkDashboard";
import { useExitMonitor } from "../../state/networkMonitor";
import { baseLocale, environmentMatch } from "../../lib/geo";

const localeLabel = (locale: string | null | undefined) => locale?.split(".")[0].replace("_", "-") ?? "unknown";

/** "Matches exit" / "Differs" for one setting, when the exit is known. */
function MatchPill({ matches }: { matches: boolean | null }) {
  if (matches == null) return null;
  return matches ? (
    <span className="inline-flex items-center gap-1 rounded-full bg-ok/12 px-2 py-0.5 text-xs font-medium text-ok">
      <IconCheck size={12} stroke={2.5} aria-hidden="true" />Matches exit
    </span>
  ) : (
    <span className="rounded-full bg-warn/14 px-2 py-0.5 text-xs font-medium text-warn">Differs from exit</span>
  );
}

/** One region setting: label and match state, the value, then the exit's value and its fix. */
function RegionTile({ label, matches, children, footer }: {
  label: string;
  matches: boolean | null;
  children: React.ReactNode;
  footer?: React.ReactNode;
}) {
  return (
    <div className="grid content-start gap-2.5 rounded-xl border border-line bg-surface/60 p-3.5">
      <div className="flex min-h-[22px] items-center justify-between gap-2">
        <span className="text-sm font-medium">{label}</span>
        <MatchPill matches={matches} />
      </div>
      {children}
      {footer && <div className="flex min-h-[24px] flex-wrap items-center gap-2 text-sm text-muted">{footer}</div>}
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
    <Section
      title="Region"
      action={(
        <Switch
          size="sm"
          labelPosition="left"
          label="Follow exit"
          checked={follow}
          disabled={toggling || !network.available}
          onChange={(event) => void setFollow(event.currentTarget.checked)}
        />
      )}
    >
      <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,13rem),1fr))] gap-3">
        <RegionTile
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
            <div className="flex h-[30px] items-center truncate text-md font-semibold" title={current.timezone}>{current.timezone}</div>
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
        </RegionTile>
        <RegionTile
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
        </RegionTile>
      </div>
      {(dirty || running) && (
        <div className="mt-3 flex flex-wrap items-center gap-2 rounded-xl bg-hover/60 px-3 py-2">
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
    </Section>
  );
}

export function NetworkSettings() {
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
    <SettingsPage title="Network" subtitle="Route VM traffic through proxy nodes.">
      <NetworkDashboard />
      <section id="network-region" className="mt-4 scroll-mt-4 outline-none" tabIndex={-1} aria-label="Region">
        {regionReady ? <Region /> : (
          <Section title="Region" hint="Available when the machine is connected.">{null}</Section>
        )}
      </section>
    </SettingsPage>
  );
}
