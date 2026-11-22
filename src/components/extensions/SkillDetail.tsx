import { Badge, Button, Loader } from "@mantine/core";
import { IconSparkles, IconTrash } from "@tabler/icons-react";
import type { SkillEntry, SkillFile } from "../../services/ipc/generated/core";
import { Markdown } from "../chat/Markdown";

/** One skill's front matter, SKILL.md and actions. */
export function SkillDetail({
  skill,
  file,
  loading,
  error,
  onCopy,
  onDelete,
}: {
  skill: SkillEntry;
  file: SkillFile | null;
  loading: boolean;
  error: string | null;
  onCopy: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="x-detail">
      <header className="x-detail-head">
        <IconSparkles size={16} className="x-detail-icon" />
        <div className="x-detail-title">
          <strong>{skill.name}</strong>
          <span className="x-path" title={skill.path}>{skill.path}</span>
        </div>
        <Badge size="xs" variant="light" color={skill.scope === "project" ? "blue" : "gray"}>
          {skill.scope}
        </Badge>
      </header>

      {skill.description && <p className="x-skill-desc">{skill.description}</p>}

      <section className="x-detail-section x-skill-body">
        <div className="x-section-head">
          <span>SKILL.md</span>
          {file && <span className="x-count">{file.body.split("\n").length} lines</span>}
        </div>
        {loading ? (
          <div className="x-loading"><Loader size="xs" /> Reading SKILL.md…</div>
        ) : error ? (
          <p className="x-error">{error}</p>
        ) : file ? (
          <div className="x-markdown markdown"><Markdown text={file.body || "No body."} /></div>
        ) : null}
      </section>

      <div className="x-detail-actions">
        <Button size="xs" variant="default" onClick={onCopy}>Copy to agents</Button>
        <span className="grow" />
        <Button size="xs" variant="light" color="red" leftSection={<IconTrash size={13} />} onClick={onDelete}>
          Delete
        </Button>
      </div>
    </div>
  );
}
