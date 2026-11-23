// Common TUI rules are drawn across the full cell. Font fallback glyphs
// can be narrower than the grid or end before the next row's baseline.
const lightRules = new Map([
  [0x2500, 0b1010], [0x2502, 0b0101], [0x250c, 0b0110], [0x2510, 0b1100],
  [0x2514, 0b0011], [0x2518, 0b1001], [0x251c, 0b0111], [0x2524, 0b1101],
  [0x252c, 0b1110], [0x2534, 0b1011], [0x253c, 0b1111],
]);
const heavyRules = new Map([
  [0x2501, 0b1010], [0x2503, 0b0101], [0x250f, 0b0110], [0x2513, 0b1100],
  [0x2517, 0b0011], [0x251b, 0b1001], [0x2523, 0b0111], [0x252b, 0b1101],
  [0x2533, 0b1110], [0x253b, 0b1011], [0x254b, 0b1111],
]);

export function paintTerminalGlyph(
  ctx: CanvasRenderingContext2D, codepoint: number,
  x: number, y: number, width: number, height: number, ratio: number,
): boolean {
  if (codepoint < 0x2500 || codepoint > 0x2590) return false;
  const snap = (value: number) => Math.round(value * ratio) / ratio;
  const left = snap(x);
  const top = snap(y);
  const right = snap(x + width);
  const bottom = snap(y + height);
  const rect = (l: number, t: number, r: number, b: number) => ctx.fillRect(l, t, r - l, b - t);
  if (codepoint >= 0x2580) {
    if (codepoint === 0x2580) rect(left, top, right, snap(y + height / 2));
    else if (codepoint <= 0x2588) rect(left, snap(y + height * (0x2588 - codepoint) / 8), right, bottom);
    else if (codepoint <= 0x258f) rect(left, top, snap(x + width * (0x2590 - codepoint) / 8), bottom);
    else rect(snap(x + width / 2), top, right, bottom);
    return true;
  }

  const heavy = heavyRules.has(codepoint);
  const rule = lightRules.get(codepoint) ?? heavyRules.get(codepoint);
  if (rule === undefined) return false;
  const thickness = Math.max(1, Math.round(width * (heavy ? 0.24 : 0.12) * ratio)) / ratio;
  const centerX = snap(x + (width - thickness) / 2);
  const centerY = snap(y + (height - thickness) / 2);
  const strokeRight = centerX + thickness;
  const strokeBottom = centerY + thickness;
  // Bits run clockwise from the top. Draw each strip once for dim SGR text.
  if (rule & 0b0001) rect(centerX, top, strokeRight, centerY);
  if (rule & 0b0100) rect(centerX, strokeBottom, strokeRight, bottom);
  rect(rule & 0b1000 ? left : centerX, centerY, rule & 0b0010 ? right : strokeRight, strokeBottom);
  return true;
}
