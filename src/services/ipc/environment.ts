import type {
  EnvironmentReview, EnvironmentSpec, EnvironmentState, OperationRecord,
} from "./generated/core";
import { invokeGuest } from "./common";

export const environmentIpc = {
  state: () => invokeGuest<EnvironmentState>("environment_state"),
  review: (draft: EnvironmentSpec) => invokeGuest<EnvironmentReview>("environment_review", { draft }),
  apply: (spec: EnvironmentSpec) => invokeGuest<OperationRecord>("environment_apply", { spec }),
};
