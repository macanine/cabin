/** "+12 −3" in the added / removed colours. Renders nothing for zero. */
export function Stat({ additions = 0, deletions = 0, className = "" }: { additions?: number | null; deletions?: number | null; className?: string }) {
  const add = additions ?? 0;
  const del = deletions ?? 0;
  if (add <= 0 && del <= 0) return null;
  return (
    <span className={`inline-flex shrink-0 gap-1.5 font-mono text-xs font-medium tabular-nums ${className}`}>
      {add > 0 && <span className="text-ok">+{add}</span>}
      {del > 0 && <span className="text-bad">−{del}</span>}
    </span>
  );
}

/** Five blocks showing the share of additions against deletions, as on a pull request. */
export function StatBar({ additions = 0, deletions = 0 }: { additions?: number | null; deletions?: number | null }) {
  const add = additions ?? 0;
  const del = deletions ?? 0;
  const total = add + del;
  if (total <= 0) return null;
  const green = Math.round((add / total) * 5);
  return (
    <span className="inline-flex shrink-0 gap-px" aria-hidden="true">
      {Array.from({ length: 5 }, (_, i) => (
        <span key={i} className={`h-2 w-2 rounded-[2px] ${i < green ? "bg-ok" : i < green + Math.round((del / total) * 5) ? "bg-bad" : "bg-line"}`} />
      ))}
    </span>
  );
}
