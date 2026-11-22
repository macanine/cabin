import type { OperationRecord } from "../services/ipc/generated/core";
import { toIpcError } from "../services/ipc/common";
import { uiActions } from "../state/ui";

/** Await an operation task and report its real final state. Failures open
 * the operation detail so the user sees the failed step and retry path. */
export async function runOperation(
  task: Promise<OperationRecord>,
  success?: string,
): Promise<OperationRecord | null> {
  try {
    const record = await task;
    if (record.state === "succeeded") {
      uiActions.toast(success ?? `${record.title}: done`, "success");
    } else {
      uiActions.toast(record.error ?? `${record.title}: ${record.state}`, "error");
      uiActions.openOperation(record.id);
    }
    return record;
  } catch (error) {
    uiActions.toast(toIpcError(error).message, "error");
    return null;
  }
}

/** Run a plain command, toasting its error. Returns undefined on failure. */
export async function attempt<T>(task: Promise<T>, success?: string): Promise<T | undefined> {
  try {
    const result = await task;
    if (success) uiActions.toast(success, "success");
    return result;
  } catch (error) {
    uiActions.toast(toIpcError(error).message, "error");
    return undefined;
  }
}

export const STATE_LABEL: Record<OperationRecord["state"], string> = {
  pending: "Waiting",
  running: "Running",
  succeeded: "Done",
  failed: "Failed",
  cancelled: "Cancelled",
  rolledBack: "Rolled back",
};
