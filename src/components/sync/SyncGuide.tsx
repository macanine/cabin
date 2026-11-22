import { Modal } from "@mantine/core";
import { IconArrowsLeftRight, IconArrowRight, IconBrandApple, IconBrandGit, IconBolt, IconFilter, IconGitMerge, IconServer } from "@tabler/icons-react";
import type { ReactNode } from "react";
import type { SyncMode } from "../../services/ipc/generated/core";
import { SYNC_MODES } from "../../lib/syncForm";

/** Mac ↔ machine, drawn for one mode. */
export function SyncFlow({ mode, compact = false }: { mode: SyncMode; compact?: boolean }) {
  const both = mode === "twoWay";
  return (
    <span className={`sync-flow ${mode}${compact ? " compact" : ""}`} aria-hidden>
      <span className="sync-flow-end"><IconBrandApple size={compact ? 12 : 14} />{!compact && "Mac"}</span>
      <span className="sync-flow-line">
        {both ? <IconArrowsLeftRight size={compact ? 12 : 14} /> : <IconArrowRight size={compact ? 12 : 14} />}
      </span>
      <span className="sync-flow-end"><IconServer size={compact ? 12 : 14} />{!compact && "Machine"}</span>
    </span>
  );
}

function Section({ icon, title, children }: { icon: ReactNode; title: string; children: ReactNode }) {
  return (
    <section className="sync-guide-section">
      <span className="sync-guide-icon">{icon}</span>
      <div>
        <h3>{title}</h3>
        {children}
      </div>
    </section>
  );
}

/**
 * How project sync works, in the order people ask: why a copy, which way
 * changes go, what is left out, what happens to git, and conflicts.
 */
export function SyncGuide({ opened, onClose }: { opened: boolean; onClose: () => void }) {
  return (
    <Modal opened={opened} onClose={onClose} title="How project sync works" size="lg" centered classNames={{ body: "sync-guide" }}>
      <Section icon={<IconBolt size={16} />} title="A real copy, kept in step">
        <p>
          The project is copied onto the machine's own disk, so agents, git, installs and builds run at full speed (a
          shared folder would pay for every file access). After the first copy, only what changes is sent, usually
          within a fraction of a second of saving.
        </p>
      </Section>

      <Section icon={<IconArrowsLeftRight size={16} />} title="Which way changes go">
        <div className="sync-guide-modes">
          {SYNC_MODES.map((mode) => (
            <div key={mode.mode} className="sync-guide-mode">
              <SyncFlow mode={mode.mode} />
              <strong>{mode.label}</strong>
              <p>{mode.summary}</p>
              <p className="faint">Agent edits: {mode.agentEdits}</p>
            </div>
          ))}
        </div>
        <p className="faint">You can change the mode at any time from the project's sync panel.</p>
      </Section>

      <Section icon={<IconFilter size={16} />} title="What is left out">
        <ul>
          <li>Anything your <code>.gitignore</code> files ignore, such as <code>node_modules</code> and build output (can be turned off).</li>
          <li>Patterns in a <code>.cabinignore</code> file (same syntax), on either side, and patterns you add when creating the project.</li>
          <li>Folders you untick in the file list.</li>
        </ul>
        <p>
          Left-out paths are never touched on either side: a <code>node_modules</code> installed in the machine stays there, and
          yours stays on your Mac. Install dependencies where the code runs.
        </p>
      </Section>

      <Section icon={<IconBrandGit size={16} />} title="Git history">
        <p>
          Git rewrites small files in <code>.git</code> constantly. If git ran on both sides and those files were merged file by
          file, one side's index could end up next to the other's branches. So by default <code>.git</code> is
          <strong> copied once</strong>: the machine gets your history when the project is created, then each side commits on its
          own while the code itself stays in sync. Both stay real repositories.
        </p>
        <p className="faint">
          With <em>Exact mirror</em>, <code>.git</code> is kept in sync instead, since only your Mac writes. You can choose either way
          for any mode; the dialog warns about risky combinations.
        </p>
      </Section>

      <Section icon={<IconGitMerge size={16} />} title="Conflicts">
        <p>
          A file changed on both sides since the last sync is never overwritten. It is listed in the project's sync panel, where
          you keep the Mac's version or the machine's. Everything else keeps syncing meanwhile.
        </p>
      </Section>
    </Modal>
  );
}
