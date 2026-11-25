// One-key answers to an approval. A key only counts as an answer once the card
// has been up, with nothing typed, for a moment: a message being typed when
// the card appears must never approve a command by its first letter.

/** Quiet time before a key can answer. */
export const ARM_MS = 600;

export interface ApprovalArm {
  /** A key was pressed; true when it may answer. */
  press(now: number): boolean;
}

export function approvalArm(shownAt: number, quiet = ARM_MS): ApprovalArm {
  let quietFrom = shownAt;
  let armed = false;
  return {
    press(now) {
      if (armed || now - quietFrom >= quiet) return (armed = true);
      quietFrom = now;
      return false;
    },
  };
}

export interface KeyPress {
  key: string;
  repeat: boolean;
  isComposing: boolean;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
}

/** The key as an answer, or null when it belongs to a text field or a shortcut. */
export function answerKey(event: KeyPress, inTextField: boolean): string | null {
  if (inTextField || event.repeat || event.isComposing || event.metaKey || event.ctrlKey || event.altKey) return null;
  return event.key.toLowerCase();
}
