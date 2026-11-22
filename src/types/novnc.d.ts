declare module "@novnc/novnc" {
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, url: string, options?: { shared?: boolean });
    scaleViewport: boolean;
    background: string;
    /** JPEG quality hint for Tight encoding, 0–9. */
    qualityLevel: number;
    /** Compression hint, 0 (fastest) – 9. */
    compressionLevel: number;
    disconnect(): void;
  }
}
