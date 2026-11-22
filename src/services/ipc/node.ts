import type { NodeRelease, NodeState, OperationRecord } from "./generated/core";
import { invokeGuest } from "./common";

/** Node.js versions inside the VM (see core/src/node.rs). */
export const nodeIpc = {
  state: () => invokeGuest<NodeState>("node_state"),
  releases: (mirror: string) => invokeGuest<NodeRelease[]>("node_releases", { mirror }),
  install: (version: string, mirror: string, activate: boolean) =>
    invokeGuest<OperationRecord>("node_install", { request: { version, mirror, activate } }),
  use: (version: string) => invokeGuest<OperationRecord>("node_use", { version }),
  remove: (version: string) => invokeGuest<OperationRecord>("node_remove", { version }),
};
