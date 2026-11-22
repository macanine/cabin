import { useAutoRefresh } from "../lib/useAutoRefresh";
import { Autocomplete, Button, Drawer, Loader, NumberInput, SegmentedControl, Select, Switch, Textarea } from "@mantine/core";
import { IconAlertTriangle, IconCheck, IconInfoCircle } from "@tabler/icons-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { browserIpc, chromeIpc, type BrowserEnvironmentView, type BrowserSettings } from "../services/ipc/chrome";
import { toIpcError } from "../services/ipc/common";
import type { BrowserEnvironmentState } from "../services/ipc/generated/core";
import {
  environmentSummary, MEMORY_BUCKETS, PLATFORMS, restartsChrome, settingsChanged, settingsProblem, type EnvTone,
} from "../lib/browserEnvironment";
import { formatTime } from "../lib/time";

const LANGUAGES = [
  "en-US", "en-GB", "de-DE", "fr-FR", "es-ES", "it-IT", "pt-BR", "nl-NL", "sv-SE", "pl-PL",
  "ru-RU", "tr-TR", "ja-JP", "ko-KR", "zh-CN", "zh-TW", "hi-IN", "ar-SA",
];

function timeZones(): string[] {
  try {
    const list = (Intl as unknown as { supportedValuesOf?: (key: string) => string[] }).supportedValuesOf?.("timeZone");
    if (list?.length) return list.includes("UTC") ? list : ["UTC", ...list];
  } catch {
    // Older engines: fall through.
  }
  return ["UTC"];
}

function ToneIcon({ tone }: { tone: EnvTone }) {
  if (tone === "busy") return <Loader size={16} color="gray" />;
  if (tone === "ok") return <IconCheck size={16} stroke={2.4} />;
  if (tone === "muted") return <IconInfoCircle size={16} />;
  return <IconAlertTriangle size={16} />;
}

/** Installed and live values side by side (only rows that are known). */
function Compare({ installed, live }: { installed: BrowserEnvironmentState | null; live: BrowserEnvironmentState | null }) {
  if (!installed && !live) return null;
  const rows: [string, (state: BrowserEnvironmentState) => string | null][] = [
    ["Time zone", (s) => s.timezone],
    ["Language", (s) => s.language],
    ["Languages", (s) => (s.languages.length ? s.languages.join(", ") : null)],
    ["Platform", (s) => s.platform],
  ];
  return (
    <table className="benv-compare">
      <thead>
        <tr><th />{installed && <th>Installed</th>}{live && <th>Open pages</th>}</tr>
      </thead>
      <tbody>
        {rows.map(([label, pick]) => {
          const a = installed ? pick(installed) : null;
          const b = live ? pick(live) : null;
          return (
            <tr key={label} className={installed && live && a !== b ? "differs" : undefined}>
              <th scope="row">{label}</th>
              {installed && <td>{a ?? "—"}</td>}
              {live && <td>{b ?? "—"}</td>}
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/**
 * The managed browser's environment: one line of state with its one action,
 * the installed and live values, and the settings. Region changes apply
 * live; anything else rewrites the profile and restarts Chrome (no reinstall,
 * no reboot). A closed or sleeping Chrome is a normal state here, never a
 * problem.
 */
export function BrowserEnvironmentPanel({ opened, onClose }: { opened: boolean; onClose: () => void }) {
  const [view, setView] = useState<BrowserEnvironmentView | null>(null);
  const [draft, setDraft] = useState<BrowserSettings | null>(null);
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState<"save" | "restart" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const zones = useMemo(timeZones, []);

  const load = useCallback(async (showProgress = false) => {
    if (showProgress) setChecking(true);
    try {
      const next = await browserIpc.environment(true);
      setError(null);
      setView(next);
      setDraft((current) => current ?? next.settings);
    } catch (err) {
      setError(toIpcError(err).message);
    } finally {
      setChecking(false);
    }
  }, []);

  useEffect(() => {
    if (!opened) return;
    setDraft(null);
    void load(true);
  }, [opened, load]);
  useAutoRefresh(() => { if (!busy) return load(); }, { enabled: opened, initial: false, interval: 30_000, minGap: 10_000 });

  const saved = view?.settings ?? null;
  const summary = environmentSummary(view, checking);
  const changed = !!(saved && draft && settingsChanged(saved, draft));
  const problem = draft ? settingsProblem(draft) : null;
  const restart = !!(saved && draft && restartsChrome(saved, draft));
  const editable = !!view?.enabled && !!view.machineRunning && busy == null;
  const update = (patch: Partial<BrowserSettings>) => setDraft((current) => (current ? { ...current, ...patch } : current));

  async function save() {
    if (!draft || problem) return;
    setBusy("save");
    setError(null);
    try {
      const next = await browserIpc.save({ ...draft, userAgent: draft.userAgent.trim() });
      setView(next);
      setDraft(next.settings);
    } catch (err) {
      setError(toIpcError(err).message);
    } finally {
      setBusy(null);
    }
  }

  async function restartChrome() {
    setBusy("restart");
    setError(null);
    try {
      await chromeIpc.stop();
      await chromeIpc.open();
      await load(true);
    } catch (err) {
      setError(toIpcError(err).message);
    } finally {
      setBusy(null);
    }
  }

  const probe = view?.probe ?? null;
  return (
    <Drawer
      opened={opened}
      onClose={onClose}
      position="right"
      size={400}
      title="Browser environment"
      classNames={{ body: "benv-body", title: "benv-title" }}
      overlayProps={{ backgroundOpacity: 0.15 }}
    >
      <section className={`benv-state ${summary.tone}`} role="status">
        <span className="benv-state-mark" aria-hidden><ToneIcon tone={summary.tone} /></span>
        <div className="benv-state-copy">
          <strong>{summary.title}</strong>
          <span>{summary.detail}</span>
        </div>
        {summary.action === "restart" && (
          <Button size="xs" loading={busy === "restart"} disabled={busy != null} onClick={() => void restartChrome()}>Restart Chrome</Button>
        )}
      </section>

      {probe && <Compare installed={probe.installed} live={probe.live === "observed" ? probe.observed : null} />}

      <div className="benv-check">
        <span className="faint">
          {view?.checkedAt != null ? `Checked ${formatTime(view.checkedAt)}` : "A closed or sleeping Chrome is not checked."}
        </span>
      </div>

      {error && <div className="callout bad benv-error">{error}</div>}

      {draft && (
        <form className="benv-form" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <fieldset className="benv-group" disabled={!editable}>
            <legend>Region</legend>
            <Switch
              checked={draft.followMachine}
              onChange={(event) => update({ followMachine: event.currentTarget.checked })}
              label="Follow the machine"
              description="Time zone, language, CPU, memory and screen follow the machine and change with it."
            />
            <Select
              label="Time zone"
              data={zones.includes(draft.timezone) || !draft.timezone ? zones : [draft.timezone, ...zones]}
              value={draft.timezone || null}
              onChange={(value) => value && update({ timezone: value })}
              searchable
              disabled={draft.followMachine}
              comboboxProps={{ withinPortal: true }}
            />
            <Autocomplete
              label="Language"
              data={LANGUAGES}
              value={draft.locale}
              onChange={(value) => update({ locale: value })}
              disabled={draft.followMachine}
            />
          </fieldset>

          <fieldset className="benv-group" disabled={!editable}>
            <legend>Identity</legend>
            <Textarea
              label="User agent"
              autosize
              minRows={2}
              maxRows={5}
              value={draft.userAgent}
              onChange={(event) => update({ userAgent: event.currentTarget.value })}
              classNames={{ input: "mono benv-mono" }}
            />
            <Autocomplete
              label="Platform"
              data={[...PLATFORMS]}
              value={draft.platform}
              onChange={(value) => update({ platform: value })}
              disabled={draft.followMachine}
            />
            <div className="benv-row">
              <NumberInput
                label="CPU cores"
                min={1}
                max={128}
                allowDecimal={false}
                value={draft.hardwareConcurrency}
                onChange={(value) => typeof value === "number" && update({ hardwareConcurrency: value })}
                disabled={draft.followMachine}
              />
              <div className="benv-memory">
                <span className="benv-label">Memory (GB)</span>
                <SegmentedControl
                  size="xs"
                  fullWidth
                  value={String(draft.deviceMemory)}
                  onChange={(value) => update({ deviceMemory: Number(value) })}
                  data={MEMORY_BUCKETS.map((value) => ({ value: String(value), label: String(value) }))}
                  disabled={draft.followMachine || !editable}
                />
              </div>
            </div>
          </fieldset>

          <div className="benv-apply">
            <span className={problem && changed ? "benv-hint bad" : "benv-hint"}>
              {!changed ? "No changes"
                : problem ? problem
                  : restart ? "Chrome restarts to apply. Open tabs come back."
                    : "Applied live; Chrome keeps running."}
            </span>
            <Button variant="default" size="xs" disabled={!changed || busy != null} onClick={() => setDraft(saved)}>Reset</Button>
            <Button type="submit" size="xs" loading={busy === "save"} disabled={!changed || !!problem || !editable}>Apply</Button>
          </div>
        </form>
      )}
    </Drawer>
  );
}
