import { IconCheck } from "@tabler/icons-react";

/** Wizard progress: numbered dots on a track that fills as the steps advance.
 *  `onGo` makes finished steps clickable. */
export function Stepper({ steps, step, onGo }: { steps: readonly string[]; step: number; onGo?: (step: number) => void }) {
  return (
    <nav aria-label="Progress" className="relative flex items-center justify-between px-2">
      <span className="absolute inset-x-6 top-[13px] h-0.5 rounded-full bg-line" />
      <span className="absolute top-[13px] left-6 h-0.5 rounded-full bg-accent transition-[width] duration-500 ease-out" style={{ width: `calc((100% - 48px) * ${step / (steps.length - 1)})` }} />
      {steps.map((name, index) => (
        <button
          key={name}
          type="button"
          disabled={!onGo || index >= step}
          aria-current={index === step ? "step" : undefined}
          onClick={() => onGo?.(index)}
          className="relative z-10 grid cursor-pointer justify-items-center gap-1.5 border-0 bg-transparent p-0 text-xs disabled:cursor-default"
        >
          <span className={`press grid size-7 place-items-center rounded-full border text-xs font-semibold ${
            index < step ? "border-accent-fill bg-accent-fill text-white" : index === step ? "border-accent bg-surface text-accent shadow-[0_0_0_4px_var(--accent-soft)]" : "border-line bg-surface text-faint"
          }`}>
            {index < step ? <IconCheck size={14} stroke={3} /> : index + 1}
          </span>
          <span className={index === step ? "font-semibold text-fg" : "text-faint"}>{name}</span>
        </button>
      ))}
    </nav>
  );
}
