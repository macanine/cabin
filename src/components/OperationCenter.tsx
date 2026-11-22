import { Button, Drawer, Modal } from "@mantine/core";
import { IconRotate } from "@tabler/icons-react";
import type { OperationRecord, StepRecord } from "../services/ipc/generated/core";
import { appIpc } from "../services/ipc/app";
import { activeOperations } from "../state/reducer";
import { useApp } from "../state/store";
import { uiActions, useUi } from "../state/ui";
import { attempt, runOperation } from "../lib/operations";
import { relativeTime } from "../lib/format";
import { OperationPill, type StateTone } from "./common";
import { StepProgress } from "./StepProgress";
import { hostTime } from "../lib/clock";

const STEP_TONE: Record<StepRecord["state"], StateTone> = {
  pending: "pending", running: "running", succeeded: "ok", failed: "bad", rolledBack: "undone", rollbackFailed: "bad",
};

const STEP_NOTE: Partial<Record<StepRecord["state"], string>> = {
  rolledBack: "rolled back",
  rollbackFailed: "rollback failed",
  failed: "failed",
};

export function OperationDetail({ record }: { record: OperationRecord }) {
  const finished = record.state !== "running" && record.state !== "pending";
  const canRetry = finished && record.state !== "succeeded" && record.retryable;
  return (
    <div style={{ display: "grid", gap: 14 }}>
      <div className="row">
        <OperationPill state={record.state} />
        <span className="muted">{relativeTime(hostTime(record.updatedAt))}</span>
      </div>
      {record.error && <div className="callout bad">{record.error}</div>}
      <StepProgress
        steps={record.steps.map((step, index) => ({
          key: `${index}:${step.name}`,
          title: step.name,
          tone: STEP_TONE[step.state],
          status: STEP_NOTE[step.state],
          finished: step.state === "succeeded" || step.state === "rolledBack",
          log: step.log,
        }))}
      />
      <div className="row" style={{ justifyContent: "flex-end" }}>
        {!finished && (
          <Button variant="default" onClick={() => attempt(appIpc.cancelOperation(record.id))}>
            Cancel
          </Button>
        )}
        {canRetry && (
          <Button onClick={() => runOperation(appIpc.retryOperation(record.id))} leftSection={<IconRotate size={16} />}>
            Retry
          </Button>
        )}
      </div>
    </div>
  );
}

export function OperationModal() {
  const id = useUi((ui) => ui.operationId);
  const record = useApp((state) => state.operations.find((op) => op.id === id));
  return (
    <Modal opened={record != null} onClose={() => uiActions.openOperation(null)} title={record?.title} size="lg">
      {record && <OperationDetail record={record} />}
    </Modal>
  );
}

/** Running/failed summary for the machine menu button. */
export function useOperationSummary(): { running: number; failed: boolean } {
  const operations = useApp((state) => state.operations);
  const running = activeOperations(operations).length;
  const failed = operations.some(
    (op) => (op.state === "failed" || op.state === "rolledBack") && Date.now() - hostTime(op.updatedAt) < 10 * 60 * 1000,
  );
  return { running, failed };
}

export function OperationsDrawer() {
  const operations = useApp((state) => state.operations);
  const open = useUi((ui) => ui.operationsOpen);
  return (
    <Drawer opened={open} onClose={() => uiActions.showOperations(false)} title="Operations" position="left" size={420}>
      <p className="muted" style={{ marginTop: 0, fontSize: 13 }}>
        Every change to configs or the system runs as an operation: it is checked first, applied step by step, and
        rolled back if a step fails.
      </p>
      {operations.length === 0 ? (
        <p className="faint">Nothing has run yet.</p>
      ) : (
        <div className="list">
          {operations.map((op) => (
            <button key={op.id} className="list-row" onClick={() => uiActions.openOperation(op.id)}>
              <span className="grow truncate">{op.title}</span>
              <span className="faint fs-12">
                {relativeTime(hostTime(op.createdAt))}
              </span>
              <OperationPill state={op.state} />
            </button>
          ))}
        </div>
      )}
    </Drawer>
  );
}
