export const DISPLAY_ZOOMS = [100, 125, 150, 175, 200];

/** Map the content area to a framebuffer while bounding the pixels we encode. */
export function matchDisplay(width: number, height: number, dpr: number) {
  const pixelRatio = Math.min(dpr, 1920 / width, 1200 / height);
  return { width: Math.round(width * pixelRatio), height: Math.round(height * pixelRatio), pixelRatio };
}

export function zoomScale(base: number, zoom: number) {
  return Math.max(1, Math.round(base * (zoom / 100) * 120) / 120);
}
