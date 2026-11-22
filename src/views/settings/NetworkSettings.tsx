import { Button, Select, Tooltip } from "@mantine/core";
import { IconBulb, IconInfoCircle, IconArrowsExchange } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { EnvironmentReview, EnvironmentSpec, FieldState } from "../../services/ipc/generated/core";
import { environmentIpc } from "../../services/ipc/environment";
import { useApp } from "../../state/store";
import { uiActions, useUi } from "../../state/ui";
import { runOperation } from "../../lib/operations";
import { relativeTime } from "../../lib/format";
import { Section } from "../../components/common";
import { SettingsPage } from "../SettingsView";
import { clockNote, hostTime, hostZone, useClock } from "../../lib/clock";
import { syncClock } from "../../state/clockSync";
import { NetworkDashboard } from "../../components/network/NetworkDashboard";
import { useExitLookup } from "../../components/network/useNetworkLive";
import { environmentMatch } from "../../lib/geo";

function Reason({ field }: { field?: FieldState }) {
  if (!field?.reason) return null;
  return <div className="field-reason">{field.reason}</div>;
}

function Suggestion({ field, onUse }: { field?: FieldState; onUse: (value: string) => void }) {
  if (!field?.suggestion) return null;
  const value = field.suggestion;
  return (
    <div className="field-reason">
      <IconBulb size={13} />
      Suggested: <span className="mono">{value}</span>
      <Button size="compact-xs" variant="subtle" onClick={() => onUse(value)}>
        Use it
      </Button>
    </div>
  );
}

/** The VM's timezone and language, applied together (and rolled back
 * together) in the guest. Routing is the dashboard's. */
function Region() {
  const env = useApp((state) => state.environment);
  const clock = useClock();
  const running = useApp((state) =>
    state.operations.some((op) => op.kind === "environment.apply" && (op.state === "running" || op.state === "pending")),
  );
  const [draft, setDraft] = useState<EnvironmentSpec | null>(null);
  const [review, setReview] = useState<EnvironmentReview | null>(null);
  const exit = useExitLookup().value;

  // Start from the committed environment; before the first apply, from what
  // the system reports, so nothing changes unless asked.
  useEffect(() => {
    if (!env || draft) return;
    const base = env.committed;
    setDraft(
      env.appliedAt == null
        ? { ...base, timezone: env.systemTimezone ?? base.timezone, language: env.systemLanguage ?? base.language }
        : base,
    );
  }, [env, draft]);

  useEffect(() => {
    if (!draft) return;
    const timer = setTimeout(() => {
      void environmentIpc.review(draft).then(setReview).catch(() => undefined);
    }, 250);
    return () => clearTimeout(timer);
  }, [draft]);

  if (!env || !draft) return null;
  const field = (name: string) => review?.fields.find((f) => f.field === name);
  const update = (patch: Partial<EnvironmentSpec>) => setDraft({ ...draft, ...patch });
  const changes = review?.changes ?? [];
  const blocked = review?.blocked ?? null;
  const note = clockNote(clock, hostZone());
  const match = environmentMatch(exit, env.systemTimezone, env.systemLanguage);
  const differs = match.zoneMatches === false || match.localeMatches === false;
  // The exit's locale as one of this machine's generated locales, if any.
  const exitLanguage = match.exitLocale ? review?.languages.find((l) => l.split(".")[0] === match.exitLocale) ?? null : null;
  const missingExitLocale = match.localeMatches === false && !exitLanguage;

  function applyExit() {
    if (!draft) return;
    setDraft({
      ...draft,
      timezone: match.exitZone ?? draft.timezone,
      language: exitLanguage ?? draft.language,
    });
  }

  return (
    <>
      <h3 className="net-region-title">
        Region
        {env.appliedAt != null && <span className="faint fs-12"> · applied {relativeTime(hostTime(env.appliedAt))}</span>}
      </h3>
      {differs && exit && (
        <div className="region-differs" role="status">
          <IconArrowsExchange size={15} />
          <span className="grow">
            The exit is in <strong>{[match.zoneMatches === false ? match.exitZone : null, match.localeMatches === false ? match.exitLocale?.replace("_", "-") : null].filter(Boolean).join(" · ")}</strong>
            {" "}but the VM uses {[match.zoneMatches === false ? env.systemTimezone : null, match.localeMatches === false ? env.systemLanguage : null].filter(Boolean).join(" · ")}.
            {missingExitLocale && ` The ${match.exitLocale?.replace("_", "-")} locale is not installed in the VM.`}
          </span>
          <Button size="xs" variant="default" onClick={applyExit}>Use exit's</Button>
        </div>
      )}
      <Section
        title="Timezone"
        hint={
          <span className="region-status">
            VM: {env.systemTimezone ?? "unknown"} · Mac: {hostZone() ?? "unknown"}
            <Tooltip
              label={`Times in this app are always shown in the Mac's zone.${note ? ` ${note}` : " The VM's clock is in step with the Mac's."}`}
              multiline
              w={280}
            >
              <IconInfoCircle size={14} className="region-info" aria-label="About time zones" />
            </Tooltip>
          </span>
        }
      >
        <Select
          className="net-region-select"
          searchable
          data={review?.timezones ?? [draft.timezone]}
          value={draft.timezone}
          onChange={(value) => value && update({ timezone: value })}
          limit={200}
        />
        <Suggestion field={field("timezone")} onUse={(value) => update({ timezone: value })} />
      </Section>
      <Section title="Language" hint="Only locales generated on this machine are listed. English is the default.">
        <Select
          className="net-region-select"
          searchable
          data={review?.languages.length ? review.languages : [draft.language]}
          value={draft.language}
          onChange={(value) => value && update({ language: value })}
          disabled={field("language")?.enabled === false}
        />
        <Reason field={field("language")} />
        <Suggestion field={field("language")} onUse={(value) => update({ language: value })} />
      </Section>
      {(changes.length > 0 || running) && (
        <div className="apply-bar">
          <div className="row" style={{ alignItems: "flex-start" }}>
            <div className="grow">
              {blocked ? (
                <span style={{ color: "var(--bad)", fontSize: 13 }}>{blocked}</span>
              ) : (
                <ul className="changes">{changes.map((change) => <li key={change}>{change}</li>)}</ul>
              )}
            </div>
            <Button variant="default" disabled={running} onClick={() => setDraft(null)}>Discard</Button>
            <Button
              loading={running}
              disabled={blocked != null || changes.length === 0}
              onClick={async () => {
                await runOperation(environmentIpc.apply(draft), "Region applied");
                void syncClock(true); // the zone may have changed
              }}
            >
              Apply changes
            </Button>
          </div>
        </div>
      )}
    </>
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
      <section id="network-region" className="network-region" tabIndex={-1} aria-label="Region">
        {regionReady ? <Region /> : (
          <>
            <h3 className="net-region-title">Region</h3>
            <p className="muted">Region settings are available when the app connects to the machine.</p>
          </>
        )}
      </section>
    </SettingsPage>
  );
}
