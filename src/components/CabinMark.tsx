import { useId } from "react";

export type CabinMarkVariant = "color" | "mono" | "tile";

/**
 * The Cabin brand mark: a minimalist three-plane architectural cabin matching
 * `src-tauri/icons/icon.svg` and `src-tauri/icons/tray.svg`.
 *
 * - `color`: bare emblem that adapts to light and dark themes (`currentColor`
 *   for the gable and side slab, `var(--accent)` for the terracotta roof).
 * - `mono`: pure single-colour silhouette matching the macOS tray template.
 * - `tile`: full warm-cream squircle app icon.
 */
export function CabinMark({
  size = 24,
  variant = "color",
  className,
}: {
  size?: number;
  variant?: CabinMarkVariant;
  className?: string;
}) {
  const id = useId();

  if (variant === "tile") {
    const bg = `${id}-bg`;
    const roof = `${id}-roof`;
    const gable = `${id}-gable`;
    const side = `${id}-side`;
    const rim = `${id}-rim`;
    const clip = `${id}-clip`;
    return (
      <svg
        className={className}
        width={size}
        height={size}
        viewBox="44 44 424 424"
        aria-hidden="true"
        focusable="false"
      >
        <defs>
          <linearGradient id={bg} x1="0.2" y1="0" x2="0.8" y2="1">
            <stop offset="0%" stopColor="#f9f7f2" />
            <stop offset="100%" stopColor="#ebe4d8" />
          </linearGradient>
          <linearGradient id={roof} x1="0.1" y1="0" x2="0.9" y2="1">
            <stop offset="0%" stopColor="#e58044" />
            <stop offset="100%" stopColor="#b85422" />
          </linearGradient>
          <linearGradient id={gable} x1="0.2" y1="0" x2="0.8" y2="1">
            <stop offset="0%" stopColor="#2a2724" />
            <stop offset="100%" stopColor="#171614" />
          </linearGradient>
          <linearGradient id={side} x1="0.2" y1="0" x2="0.8" y2="1">
            <stop offset="0%" stopColor="#cfc7bb" />
            <stop offset="100%" stopColor="#bab1a4" />
          </linearGradient>
          <linearGradient id={rim} x1="0.5" y1="0" x2="0.5" y2="1">
            <stop offset="0%" stopColor="#ffffff" stopOpacity="0.9" />
            <stop offset="50%" stopColor="#ffffff" stopOpacity="0.2" />
            <stop offset="100%" stopColor="#1d1c1a" stopOpacity="0.14" />
          </linearGradient>
          <clipPath id={clip}>
            <path d="M 168,44 L 344,44 C 426,44 468,86 468,168 L 468,344 C 468,426 426,468 344,468 L 168,468 C 86,468 44,426 44,344 L 44,168 C 44,86 86,44 168,44 Z" />
          </clipPath>
        </defs>
        <g clipPath={`url(#${clip})`}>
          <rect x="44" y="44" width="424" height="424" fill={`url(#${bg})`} />
          <path d="M 227.5,148 L 302,148 Q 312,148 318,156 L 393,256 Q 399,264 389,264 L 314.5,264 Q 304.5,264 298.5,256 L 223.5,156 Q 217.5,148 227.5,148 Z" fill={`url(#${roof})`} />
          <path d="M 194,156 Q 200,148 206,156 L 281,256 Q 284,260 284,265 L 284,354 C 284,362 278,368 270,368 L 233,368 C 230,368 228,366 228,363 L 228,312 A 28,28 0 0 0 172,312 L 172,363 C 172,366 170,368 167,368 L 130,368 C 122,368 116,362 116,354 L 116,265 Q 116,260 119,256 Z" fill={`url(#${gable})`} />
          <path d="M 298,278 L 372,278 C 375.5,278 378,280.5 378,284 L 378,354 C 378,362 372,368 364,368 L 304,368 C 300.5,368 298,365.5 298,362 Z" fill={`url(#${side})`} />
        </g>
        <path d="M 168,45 L 344,45 C 425.4,45 467,86.6 467,168 L 467,344 C 467,425.4 425.4,467 344,467 L 168,467 C 86.6,467 45,425.4 45,344 L 45,168 C 45,86.6 86.6,45 168,45 Z" fill="none" stroke={`url(#${rim})`} strokeWidth="2.5" />
      </svg>
    );
  }

  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <path
        fill={variant === "color" ? "var(--accent)" : "currentColor"}
        d="M 10.7,4.5 L 15.0,4.5 Q 15.7,4.5 16.1,5.1 L 20.8,11.3 Q 21.2,11.9 20.5,11.9 L 16.2,11.9 Q 15.5,11.9 15.1,11.3 L 10.4,5.1 Q 10.0,4.5 10.7,4.5 Z"
      />
      <path
        fill="currentColor"
        d="M 7.7,5.1 Q 8.1,4.5 8.5,5.1 L 13.1,11.2 Q 13.4,11.6 13.4,12.1 L 13.4,18.6 C 13.4,19.1 13.0,19.5 12.5,19.5 L 10.1,19.5 L 10.1,16.0 A 2.0,2.0 0 0 0 6.1,16.0 L 6.1,19.5 L 3.7,19.5 C 3.2,19.5 2.8,19.1 2.8,18.6 L 2.8,12.1 Q 2.8,11.6 3.1,11.2 Z"
      />
      <path
        fill="currentColor"
        opacity={variant === "color" ? 0.24 : 1}
        d="M 14.8,13.3 L 19.2,13.3 C 19.5,13.3 19.7,13.5 19.7,13.8 L 19.7,18.6 C 19.7,19.1 19.3,19.5 18.8,19.5 L 15.2,19.5 C 15.0,19.5 14.8,19.3 14.8,19.1 Z"
      />
    </svg>
  );
}

/** Brand lockup: Cabin mark beside the Cabin wordmark. */
export function CabinWordmark({ size = 20, className = "" }: { size?: number; className?: string }) {
  return (
    <span className={`inline-flex items-center gap-2 text-fg ${className}`.trim()}>
      <CabinMark size={size} variant="color" className="shrink-0" />
      <span className="text-[14px] font-semibold tracking-[-0.02em]">Cabin</span>
    </span>
  );
}
