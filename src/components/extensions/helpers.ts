/** `KEY=value` lines ↔ a map (env vars; headers use `Name: value`). */
export function parsePairs(text: string, separator: "=" | ":"): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const at = line.indexOf(separator);
    if (at <= 0) continue;
    out[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  return out;
}

export function pairsText(map: Record<string, string>, separator: "=" | ":"): string {
  return Object.entries(map)
    .map(([key, value]) => (separator === "=" ? `${key}=${value}` : `${key}: ${value}`))
    .join("\n");
}

/** Shell-like split for the args field: spaces separate, quotes group. */
export function splitArgs(text: string): string[] {
  const out: string[] = [];
  const re = /"((?:\\.|[^"\\])*)"|'([^']*)'|(\S+)/g;
  for (let match = re.exec(text); match; match = re.exec(text)) {
    out.push(match[1] != null ? match[1].replace(/\\(.)/g, "$1") : match[2] ?? match[3]);
  }
  return out;
}

export function joinArgs(args: string[]): string {
  return args.map((arg) => (/[\s"']/.test(arg) || !arg ? JSON.stringify(arg) : arg)).join(" ");
}
