import { Popover } from "@mantine/core";
import type { ReactNode } from "react";

/** A quiet chip in the rail at the top of the composer dock. */
export const PILL =
  "press inline-flex h-7 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-transparent px-2 text-xs text-muted hover:bg-hover hover:text-fg aria-expanded:bg-hover aria-expanded:text-fg";

/** What a pill opens: a glass popover above it. `target` is the pill (a button). */
export function StripPopover({ target, children, opened, onChange, width = 360 }: {
  target: ReactNode;
  children: ReactNode;
  opened?: boolean;
  onChange?: (opened: boolean) => void;
  width?: number | string;
}) {
  return (
    <Popover position="top-start" withinPortal offset={8} width={width} opened={opened} onChange={onChange} shadow="lg" radius="lg">
      <Popover.Target>{target}</Popover.Target>
      <Popover.Dropdown p="xs" style={{ maxHeight: "min(60vh, 460px)", overflow: "auto" }}>{children}</Popover.Dropdown>
    </Popover>
  );
}

/** A small section label inside a strip popover. */
export const SectionLabel = ({ children }: { children: ReactNode }) => (
  <div className="px-2 pt-1.5 pb-1 text-[11px] font-semibold tracking-wide text-faint uppercase">{children}</div>
);
