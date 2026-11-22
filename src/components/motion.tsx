import { IconChevronRight } from "@tabler/icons-react";
import { type ReactNode, useState } from "react";

/** Follow the automatic default until the reader toggles the disclosure. */
export function useAutoOpen(auto: boolean): [boolean, () => void] {
  const [choice, setChoice] = useState<boolean | null>(null);
  const open = choice ?? auto;
  return [open, () => setChoice(!open)];
}

/** A disclosure that eases its height and stays out of the tab order while closed. */
export function Reveal({ open, children, className = "" }: { open: boolean; children: ReactNode; className?: string }) {
  return (
    <div
      aria-hidden={!open}
      data-open={open}
      className={`grid grid-rows-[0fr] opacity-0 transition-[grid-template-rows,opacity] duration-[var(--motion-reveal)] ease-[var(--ease-out)] data-[open=true]:grid-rows-[1fr] data-[open=true]:opacity-100 ${className}`}
    >
      <div className="min-h-0 min-w-0 overflow-hidden" inert={!open}>{children}</div>
    </div>
  );
}

export function DisclosureChevron({ open, size = 12, className = "" }: { open: boolean; size?: number; className?: string }) {
  return <IconChevronRight size={size} aria-hidden className={`shrink-0 transition-transform duration-[var(--motion-reveal)] ease-[var(--ease-out)] ${open ? "rotate-90" : ""} ${className}`} />;
}
