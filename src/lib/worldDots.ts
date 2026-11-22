/**
 * Land for the dotted world map: a 144 × 58 grid (2.5° cells) from 84°N to
 * 60°S, one bit per cell, row by row from the north-west, set where the
 * cell's centre is on land. Generated from Natural Earth's 1:110m land
 * polygons (public domain); regenerate rather than edit.
 */
import { MAP_NORTH, MAP_SOUTH } from "./geo.ts";

export const DOT_COLS = 144;
export const DOT_ROWS = 58;

const LAND =
  "AAAAAAH+A/8AAAAAAAAAAAAAAAAAAHn3//+AAcAQAAcAAAAAAAAAEEOf//8AAsAAAAAAAAAAAAAAfN8Af/8AAAABgA/8AeAAAAAD" +
  "xK8AP/4AAAAECP//4AAAAfgAHmf4P/wAAHAAG/////8Ag///xn5cH+AAA/8v+///////Z/////g6HwHAB7z/////////A////+AI" +
  "DgAAHn//////////A/v//8BwAgAAPl////////xgAEA//+B+AAACBj///////wGAAQAf//x/AAACHP///////gOAAAAP//5/wAAP" +
  "P////////4IAAAAD////AAACf////////0AAAAAD///84AAD/////////wAAAAAD////AAAA+/X//////gAAAAAD///wAAAPjeD/" +
  "/////MAAAAAD///AAAAPEu//////cIAAAAAB//+AAAAPAN/////+IAAAAAAA///AAAAB8EP/////FwAAAAAAf/8AAAAP+AP/////" +
  "CAAAAAAAH/4AAAAP/v//////gAAAAAAAP4EAAAAf///n////AAAAAAAAH4EAAAA///37////AAAAAAAAB4AAAAB///z/D//+gAAA" +
  "AEAAA4hAAAB///7+B+fgAAAAAAAAAfg0AAA///5+B8PggAAAAAAAAFwAAAB///94BwHggAAAAAAAAA4AAAB////AAwHwwAAAAAAA" +
  "AAI5AAA////wAwFgQAAAAAAAAAF/AAAf///wAIEAYAAAAAAAAAB/wAAPP//wAACCAAAAAAAAAAB/8AAAD//gAAGGAAAAAAAAAAD/" +
  "8AAAD//AAACOwAAAAAAAAAD//AAAD/+AAADOhAAAAAAAAAD//4AAD/8AAABAA8AAAAAAAAD//8AAB/8AAAAoAeAAAAAAAAB//8AA" +
  "B/8AAAAAAICAAAAAAAB//4AAB/8AAAAAAAAAAAAAAAB//wAAB/8QAAAALAAAAAAAAAAf/wAAB/8wAAAAfsAAAAAAAAAP/wAAB/ww" +
  "AAAAf+AAAAAAAAAP/wAAA/xgAAAD//AgAAAAAAAP/AAAA/xgAAAH//gAAAAAAAAP+AAAA/gAAAAD//gAAAAAAAAf8AAAAfgAAAAD" +
  "//gAAAAAAAAf8AAAAfAAAAAD//gAAAAAAAAf4AAAAYAAAAADA/gAAAAAAAAfgAAAAAAAAAAAAPAAAAAAAAAfAAAAAAAAAAAAACAC" +
  "AAAAAAA8AAAAAAAAAAAAACAEAAAAAAAcAAAAAAAAAAAAAAAIAAAAAAA4AAAAAAAAAAAAAAAQAAAAAAA4AAAAAAAAAAAAAAAAAAAA" +
  "AAAwgAAAAAAAAAAAAAAAAAAAAAAYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

let cells: { lat: number; lon: number }[] | null = null;

/** The land cells' centres. */
export function landDots(): { lat: number; lon: number }[] {
  if (cells) return cells;
  const bytes = Uint8Array.from(atob(LAND), (c) => c.charCodeAt(0));
  const found: { lat: number; lon: number }[] = [];
  for (let row = 0; row < DOT_ROWS; row++) {
    for (let col = 0; col < DOT_COLS; col++) {
      const bit = row * DOT_COLS + col;
      if (bytes[bit >> 3] & (0x80 >> (bit & 7))) {
        found.push({
          lat: MAP_NORTH - ((row + 0.5) * (MAP_NORTH - MAP_SOUTH)) / DOT_ROWS,
          lon: -180 + ((col + 0.5) * 360) / DOT_COLS,
        });
      }
    }
  }
  cells = found;
  return found;
}
