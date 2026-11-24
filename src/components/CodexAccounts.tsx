import { useAutoRefresh } from "../lib/useAutoRefresh";
import {
  Alert,
  Badge,
  Button,
  Checkbox,
  Divider,
  Group,
  Loader,
  Modal,
  Select,
  Stack,
  Text,
  Textarea,
} from "@mantine/core";
import {
  IconAlertTriangle,
  IconDownload,
  IconFileImport,
  IconFileExport,
  IconKey,
  IconRotateClockwise,
  IconTrash,
} from "@tabler/icons-react";
import { useCallback, useState } from "react";
import type {
  AgentStatus,
  CodexAccount,
  CodexAccounts,
  CodexQuota,
  CodexResetCard,
} from "../services/ipc/generated/core";
import { agentsIpc } from "../services/ipc/agents";
import { chooseFile, chooseFolder } from "../services/ipc/dialogs";
import { readTextFile, writeCodexExport } from "../services/ipc/files";
import { attempt } from "../lib/operations";
import { AccountLimit } from "./AccountLimits";
import { toIpcError } from "../services/ipc/common";
import { uiActions } from "../state/ui";
import { formatDate, formatDateTime, formatGuestDateTime } from "../lib/time";

function accountName(account: CodexAccount): string {
  if (account.email) return account.email;
  if (account.authMode === "apikey") return "OpenAI API key";
  return "ChatGPT account";
}

function planLabel(plan: string | null | undefined): string {
  if (!plan) return "Plan unknown";
  return plan
    .replace(/_/g, " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function dateTime(ms: number | null | undefined): string {
  if (ms == null) return "No expiry";
  return formatDateTime(ms);
}

function subscriptionDate(value: string | null): string | null {
  if (!value) return null;
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return value;
  return formatDate(timestamp);
}

function QuotaPanel({ quota }: { quota: CodexQuota }) {
  const credits = quota.credits;
  return (
    <div className="codex-quota">
      <Group justify="space-between" align="flex-start">
        <div>
          <Group gap="xs">
            <Text fw={650}>Limits</Text>
            <Badge variant="light" color="orange">
              {planLabel(quota.planType)}
            </Badge>
          </Group>
          <Text size="xs" c="dimmed" mt={3}>
            Last checked {formatGuestDateTime(quota.refreshedAt)}
          </Text>
        </div>
      </Group>
      {quota.error && (
        <Alert color="red" icon={<IconAlertTriangle size={16} />} mt="sm">
          {quota.error}
        </Alert>
      )}
      {quota.ordinaryUsageAllowed === false && (
        <Alert color="yellow" icon={<IconAlertTriangle size={16} />} mt="sm">
          The account has reached an included-usage limit.
        </Alert>
      )}
      {(quota.spendControlReached || quota.rateLimitReachedType) && (
        <Alert color="yellow" icon={<IconAlertTriangle size={16} />} mt="sm">
          Codex reports an active spend-control or rate-limit restriction
          {quota.rateLimitReachedType
            ? ` (${quota.rateLimitReachedType.replace(/_/g, " ")})`
            : ""}.
        </Alert>
      )}
      {quota.windows.length > 0 && (
        <Stack gap="sm" mt="md">
          {quota.windows.map((window) => (
            <AccountLimit key={`${window.kind}-${window.windowMinutes}`} label={window.label} used={window.usedPercent} resetsAt={window.resetsAt} windowMinutes={window.windowMinutes} />
          ))}
        </Stack>
      )}
      {!quota.error && quota.windows.length === 0 && (
        <Text size="sm" c="dimmed" mt="sm">
          This plan did not report a 5-hour or 7-day window.
        </Text>
      )}
      <Group gap="lg" mt="md">
        <div>
          <Text size="xs" c="dimmed">
            Credits
          </Text>
          <Text size="sm">
            {credits?.unlimited
              ? "Unlimited"
              : credits?.hasCredits
                ? `${credits.balance ?? "Available"} remaining`
                : "None"}
          </Text>
        </div>
        <div>
          <Text size="xs" c="dimmed">
            Reset cards
          </Text>
          <Text size="sm">{quota.resetCreditCount}</Text>
        </div>
      </Group>
    </div>
  );
}

function resetCardLabel(card: CodexResetCard, index: number): string {
  return `${card.title || `Reset ${index + 1}`} · ${dateTime(card.expiresAt)}`;
}

export function CodexAccountsPanel({ status }: { status: AgentStatus }) {
  const [data, setData] = useState<CodexAccounts | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [paste, setPaste] = useState("");
  const [activateImport, setActivateImport] = useState(false);
  const [exportTarget, setExportTarget] = useState<CodexAccount | null>(null);
  const [exportFormat, setExportFormat] = useState<"native" | "cliproxyapi" | "sub2api">("native");
  const [removeTarget, setRemoveTarget] = useState<CodexAccount | null>(null);
  const [resetTarget, setResetTarget] = useState<CodexResetCard | null>(null);
  const [resetConfirmed, setResetConfirmed] = useState(false);

  const [readError, setReadError] = useState<string | null>(null);
  const load = useCallback(async (isCurrent: () => boolean) => {
    try {
      const next = await agentsIpc.codexAccounts();
      if (!isCurrent()) return;
      setData(next);
      setReadError(null);
      if (next.activeAccountId) {
        const quota = await agentsIpc.refreshCodexQuota();
        if (isCurrent()) setData(quota);
      }
    } catch (error) { if (isCurrent()) setReadError(toIpcError(error).message); }
  }, []);
  useAutoRefresh(load, { enabled: busy == null, interval: 180_000, minGap: 60_000 });

  async function importFile() {
    const path = await chooseFile();
    if (!path) return;
    setBusy("import");
    const contents = await attempt(readTextFile(path));
    if (contents) {
      const next = await attempt(
        agentsIpc.importCodexAccount(contents, activateImport),
        activateImport ? "Codex credentials imported and activated" : "Codex credentials imported",
      );
      if (next) {
        setData(next);
        if (activateImport) {
          const refreshed = await attempt(agentsIpc.refreshCodexQuota());
          if (refreshed) setData(refreshed);
        }
      }
      await agentsIpc.refresh().catch(() => undefined);
    }
    setBusy(null);
  }

  async function saveCurrent() {
    setBusy("current");
    const next = await attempt(
      agentsIpc.importCodexAccount(null, true),
      "Current Codex login saved",
    );
    if (next) setData(next);
    setBusy(null);
  }

  async function submitPaste() {
    if (!paste.trim()) return;
    setBusy("paste");
    const next = await attempt(
      agentsIpc.importCodexAccount(paste, activateImport),
      activateImport ? "Codex credentials imported and activated" : "Codex credentials imported",
    );
    if (next) {
      setData(next);
      setPaste("");
      setPasteOpen(false);
      if (activateImport) {
          const refreshed = await attempt(agentsIpc.refreshCodexQuota());
        if (refreshed) setData(refreshed);
      }
      await agentsIpc.refresh().catch(() => undefined);
    }
    setBusy(null);
  }

  async function switchAccount(account: CodexAccount) {
    setBusy(`switch:${account.id}`);
    const next = await attempt(
      agentsIpc.switchCodexAccount(account.id),
      `Switched to ${accountName(account)}`,
    );
    if (next) {
      setData(next);
      const refreshed = await attempt(agentsIpc.refreshCodexQuota());
      if (refreshed) setData(refreshed);
    }
    await agentsIpc.refresh().catch(() => undefined);
    setBusy(null);
  }

  async function exportAccount() {
    if (!exportTarget) return;
    const directory = await chooseFolder();
    if (!directory) return;
    setBusy("export");
    try {
      const contents = await agentsIpc.exportCodexAccount(exportTarget.id, exportFormat);
      const path = await writeCodexExport(directory, contents);
      uiActions.toast(`Codex account exported to ${path}`, "success");
      setExportTarget(null);
    } catch (cause) {
      uiActions.toast(toIpcError(cause).message, "error");
    } finally { setBusy(null); }
  }

  async function removeAccount() {
    if (!removeTarget) return;
    setBusy(`remove:${removeTarget.id}`);
    const next = await attempt(
      agentsIpc.removeCodexAccount(removeTarget.id),
      "Saved Codex account removed",
    );
    if (next) setData(next);
    setBusy(null);
    setRemoveTarget(null);
  }

  async function useResetCard() {
    if (!resetTarget) return;
    setBusy("reset");
    const result = await attempt(
      agentsIpc.useCodexResetCard(resetTarget.id),
      "Codex reset request completed",
    );
    if (result) {
      setData((current) =>
        current && result.quota ? { ...current, quota: result.quota } : current,
      );
    }
    setBusy(null);
    setResetTarget(null);
    setResetConfirmed(false);
  }

  const active = data?.accounts.find((account) => account.active) ?? null;
  const availableCards =
    data?.quota?.resetCredits.filter(
      (card) => card.status === "available",
    ) ?? [];
  const subscriptionUntil = subscriptionDate(active?.subscriptionUntil ?? null);

  return (
    <div className="codex-accounts">
      <Group justify="space-between" align="flex-start">
        <div>
          <Text fw={600} size="sm">Saved accounts</Text>
          <Text size="xs" c="dimmed" mt={2}>
            Switch between Codex logins. Switching is blocked while a Codex session is running.
          </Text>
        </div>
        <Group gap="xs">
          <Button
            size="xs"
            variant="default"
            leftSection={<IconDownload size={14} />}
            disabled={!status.authenticated || busy != null}
            onClick={saveCurrent}
          >
            Save current
          </Button>
          <Button
            size="xs"
            leftSection={<IconFileImport size={14} />}
            disabled={busy != null}
            onClick={importFile}
          >
            Import file
          </Button>
          <Button
            size="xs"
            variant="subtle"
            leftSection={<IconKey size={14} />}
            disabled={busy != null}
            aria-expanded={pasteOpen}
            aria-controls={pasteOpen ? "codex-import-paste" : undefined}
            onClick={() => {
              setPasteOpen(!pasteOpen);
              setPaste("");
            }}
          >
            Paste credentials
          </Button>
        </Group>
      </Group>

      <Checkbox size="xs" mt="sm" checked={activateImport} onChange={(event) => setActivateImport(event.currentTarget.checked)} label="Switch to the first imported account" />

      {pasteOpen && (
        <form id="codex-import-paste" className="mt-3 grid gap-3 rounded-lg border border-line bg-hover p-3"
          onSubmit={(event) => { event.preventDefault(); void submitPaste(); }}>
          <Textarea label="Credential JSON" autoFocus value={paste} onChange={(event) => setPaste(event.currentTarget.value)}
            description="Accepts Codex auth.json, a CLIProxyAPI Codex auth file, or a Sub2API export. Cabin stores credentials privately in the VM."
            autosize minRows={3} maxRows={10} spellCheck={false} autoComplete="off" />
          <Group justify="flex-end">
            <Button type="submit" size="xs" loading={busy === "paste"} disabled={!paste.trim() || busy != null}>Import</Button>
          </Group>
        </form>
      )}

      {!data ? !readError && (
        <div className="row muted" style={{ marginTop: 14 }}>
          <Loader size="xs" /> Reading Codex accounts…
        </div>
      ) : data.accounts.length === 0 ? (
        <Alert color="gray" mt="md">
          No Codex login is saved yet. Sign in with <span className="mono">codex login</span>,
          then choose “Save current”, or import an existing Codex, CLIProxyAPI or
          Sub2API credential file.
        </Alert>
      ) : (
        <div className="list" style={{ marginTop: 14 }}>
          {data.accounts.map((account) => (
            <div className="list-row flex-wrap" key={account.id}>
              <div className="grow" style={{ minWidth: 0 }}>
                <Group gap="xs">
                  <Text size="sm" fw={600} truncate>
                    {accountName(account)}
                  </Text>
                  {account.active && (
                    <Badge size="xs" color="teal" variant="light">
                      Active
                    </Badge>
                  )}
                  <Badge size="xs" color="gray" variant="light">
                    {planLabel(account.planType)}
                  </Badge>
                </Group>
                <Text size="xs" c="dimmed">
                  {account.authMode === "apikey" ? "API key login" : "ChatGPT login"}
                  {account.email && account.active && subscriptionUntil
                    ? ` · Paid through ${subscriptionUntil}`
                    : ""}
                </Text>
              </div>
              {!account.active && (
                <Button
                  size="xs"
                  variant="default"
                  loading={busy === `switch:${account.id}`}
                  disabled={busy != null}
                  onClick={() => void switchAccount(account)}
                >
                  Switch
                </Button>
              )}
              <Button size="compact-sm" variant="subtle" color="gray" aria-label={`Export ${accountName(account)}`}
                leftSection={<IconFileExport size={15} />} disabled={busy != null} onClick={() => { setExportTarget(account); setExportFormat("native"); }}>
                Export
              </Button>
              <Button
                size="compact-sm"
                variant="subtle"
                color="gray"
                aria-label={`Remove ${accountName(account)}`}
                disabled={account.active || busy != null}
                onClick={() => setRemoveTarget(account)}
              >
                <IconTrash size={14} />
              </Button>
            </div>
          ))}
        </div>
      )}

      {data?.quota ? (
        <>
          <Divider my="lg" />
          <QuotaPanel quota={data.quota} />
          {availableCards.length > 0 && (
            <div className="codex-reset">
              <Text size="sm" fw={600} mb={6}>
                Reset cards
              </Text>
              <Stack gap="xs">
                {availableCards.map((card, index) => (
                  <Group key={card.id} justify="space-between" gap="sm">
                    <div>
                      <Text size="sm">{card.title || `Reset card ${index + 1}`}</Text>
                      <Text size="xs" c="dimmed">
                        Expires {dateTime(card.expiresAt)}
                      </Text>
                    </div>
                    <Button
                      size="compact-sm"
                      variant="light"
                      color="orange"
                      leftSection={<IconRotateClockwise size={13} />}
                      disabled={busy != null}
                      onClick={() => {
                        setResetTarget(card);
                        setResetConfirmed(false);
                      }}
                    >
                      Use card
                    </Button>
                  </Group>
                ))}
              </Stack>
            </div>
          )}
        </>
      ) : null}
      {readError && <Text size="xs" c="dimmed" role="status">{readError}</Text>}

      <Modal opened={exportTarget != null} onClose={() => setExportTarget(null)} title="Export Codex account" centered>
        <Stack gap="md">
          <Text size="sm">Export {exportTarget ? accountName(exportTarget) : ""} to a file on this Mac.</Text>
          <Alert color="yellow" icon={<IconAlertTriangle size={16} />}>
            This file contains live credentials. Anyone with it can use the account. A ChatGPT refresh token can be renewed in one place only; using the export elsewhere may sign out the other copy.
          </Alert>
          <Select label="Format" value={exportFormat} onChange={(value) => setExportFormat((value ?? "native") as typeof exportFormat)} allowDeselect={false}
            data={[
              { value: "native", label: "Codex auth.json" },
              { value: "cliproxyapi", label: "CLIProxyAPI" },
              { value: "sub2api", label: "Sub2API" },
            ]} />
          <Group justify="flex-end"><Button variant="default" onClick={() => setExportTarget(null)}>Cancel</Button><Button loading={busy === "export"} onClick={() => void exportAccount()}>Choose folder & export</Button></Group>
        </Stack>
      </Modal>

      <Modal
        opened={removeTarget != null}
        onClose={() => setRemoveTarget(null)}
        title="Remove saved account?"
        centered
      >
        <Text size="sm">
          This deletes the saved credential for{" "}
          <strong>{removeTarget ? accountName(removeTarget) : ""}</strong>. It does not sign
          that account out elsewhere.
        </Text>
        <Group justify="flex-end" mt="lg">
          <Button variant="default" onClick={() => setRemoveTarget(null)}>
            Cancel
          </Button>
          <Button color="red" loading={busy?.startsWith("remove:")} onClick={removeAccount}>
            Remove
          </Button>
        </Group>
      </Modal>

      <Modal
        opened={resetTarget != null}
        onClose={() => {
          setResetTarget(null);
          setResetConfirmed(false);
        }}
        title="Use a Codex reset card?"
        centered
      >
        <Stack gap="md">
          <Alert color="red" icon={<IconAlertTriangle size={16} />}>
            This starts the account’s eligible usage windows again and spends the selected
            card. It cannot be undone.
          </Alert>
          <Select
            label="Card"
            value={resetTarget?.id ?? null}
            onChange={(id) =>
              setResetTarget(
                availableCards.find((card) => card.id === id) ?? null,
              )
            }
            data={availableCards.map((card, index) => ({
              value: card.id,
              label: resetCardLabel(card, index),
            }))}
          />
          <Checkbox
            checked={resetConfirmed}
            onChange={(event) => setResetConfirmed(event.currentTarget.checked)}
            label="I understand this reset cannot be undone"
          />
          <Group justify="flex-end">
            <Button variant="default" onClick={() => setResetTarget(null)}>
              Cancel
            </Button>
            <Button
              color="red"
              disabled={!resetConfirmed}
              loading={busy === "reset"}
              onClick={useResetCard}
            >
              Use reset card
            </Button>
          </Group>
        </Stack>
      </Modal>
    </div>
  );
}
