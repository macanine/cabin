import type { ReactNode } from "react";

/**
 * The one layout for everything that happens to the machine: setup, install,
 * start, stopped and recovery. A heading, the work on the left, the machine
 * card beside it, and a bar for the actions that stays in view.
 */
export function MachineStage({ eyebrow, title, lead, main, aside, bar, lede, center = false }: {
  eyebrow?: ReactNode;
  title: ReactNode;
  lead?: ReactNode;
  /** Beside the title on the right. */
  lede?: ReactNode;
  main: ReactNode;
  aside?: ReactNode;
  bar?: ReactNode;
  /** For screens that stay short: the content sits in the middle. */
  center?: boolean;
}) {
  return (
    <div className="stage">
      <div className="stage-scroll" data-center={center || undefined}>
        <div className="stage-frame view-enter">
          <header className="stage-header">
            <div className="min-w-0">
              {eyebrow && <p className="iw-eyebrow">{eyebrow}</p>}
              <h1><span role="status">{title}</span></h1>
              {lead && <p className="iw-lead" key={typeof lead === "string" ? lead : undefined}>{lead}</p>}
            </div>
            {lede}
          </header>
          <div className={aside ? "iw-grid" : "iw-solo"}>
            <div className="iw-main">{main}</div>
            {aside && <aside className="iw-aside">{aside}</aside>}
          </div>
        </div>
      </div>
      {bar && <footer className="stage-bar"><div className="stage-bar-inner">{bar}</div></footer>}
    </div>
  );
}
