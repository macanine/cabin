import { IconWorld } from "@tabler/icons-react";
import { useEffect, useState } from "react";

/**
 * Country flags from the SVG set bundled into the app (country-flag-icons,
 * 3:2). Each flag is its own small chunk, loaded on first use and cached, so
 * flags never depend on the system emoji font (which shows letters on some
 * systems and different artwork on others).
 */
const FLAGS = import.meta.glob<string>("../../node_modules/country-flag-icons/string/3x2/[A-Z][A-Z].js", { import: "default" });
const cache = new Map<string, string>();

function source(code: string) {
  return FLAGS[`../../node_modules/country-flag-icons/string/3x2/${code}.js`];
}

function toUrl(svg: string) {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

/** `code`: ISO 3166-1 alpha-2 ("JP"). Unknown codes show a globe. */
export function Flag({ code, size = 14, title }: { code: string | null | undefined; size?: number; title?: string }) {
  const upper = code?.toUpperCase() ?? null;
  const [url, setUrl] = useState<string | null>(() => (upper ? cache.get(upper) ?? null : null));
  const load = upper ? source(upper) : undefined;

  useEffect(() => {
    if (!upper || !load) return setUrl(null);
    const hit = cache.get(upper);
    if (hit) return setUrl(hit);
    let alive = true;
    void load().then((svg) => {
      const next = toUrl(svg);
      cache.set(upper, next);
      if (alive) setUrl(next);
    }).catch(() => {});
    return () => { alive = false; };
  }, [upper, load]);

  const height = size;
  const width = Math.round(size * 1.5);
  if (!upper || !load) {
    return <IconWorld size={size} className="flag-fallback" aria-hidden={title ? undefined : true} aria-label={title} />;
  }
  return (
    <span className="flag" style={{ width, height }} role={title ? "img" : undefined} aria-label={title} aria-hidden={title ? undefined : true}>
      {url && <img src={url} alt="" width={width} height={height} draggable={false} />}
    </span>
  );
}
