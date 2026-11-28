import { useId } from "react";

export type CabinMarkVariant = "color" | "mono" | "tile";

const GABLE_24 =
  "M 7.86,5.60 Q 7.92,5.46 8.07,5.46 L 10.87,5.46 Q 11.02,5.46 11.07,5.60 L 14.01,13.43 Q 14.06,13.57 13.91,13.57 L 11.23,13.57 Q 11.08,13.57 11.03,13.43 L 9.52,9.41 Q 9.47,9.27 9.41,9.41 L 6.04,18.40 Q 5.99,18.54 5.84,18.54 L 3.16,18.54 Q 3.01,18.54 3.06,18.40 Z";

const ROOF_24 =
  "M 13.32,5.60 Q 13.26,5.46 13.41,5.46 L 16.09,5.46 Q 16.24,5.46 16.30,5.60 L 19.23,13.43 Q 19.29,13.57 19.14,13.57 L 16.46,13.57 Q 16.31,13.57 16.25,13.43 Z";

const FLOOR_24 =
  "M 8.91,16.40 Q 8.91,16.25 9.06,16.25 L 20.14,16.25 Q 20.29,16.25 20.34,16.39 L 21.10,18.40 Q 21.15,18.54 21.00,18.54 L 9.06,18.54 Q 8.91,18.54 8.91,18.39 Z";

/** Centerline paths clipped to the three architectural pieces for the boot racetrack circuit. */
const TRACK_GABLE_24 = "M 4.50,18.54 L 9.47,6.36 L 12.57,13.57";
const TRACK_ROOF_24 = "M 14.75,5.46 L 17.80,13.57";
const TRACK_FLOOR_24 = "M 20.72,17.40 L 8.91,17.40";

const GABLE_512 =
  "M 193.31,154.06 Q 194.08,152.00 196.28,152.00 L 239.72,152.00 Q 241.92,152.00 242.69,154.06 L 289.51,278.90 Q 290.28,280.96 288.08,280.96 L 246.48,280.96 Q 244.28,280.96 243.51,278.90 L 218.77,212.94 Q 218.00,210.88 217.23,212.94 L 162.85,357.94 Q 162.08,360.00 159.88,360.00 L 118.28,360.00 Q 116.08,360.00 116.85,357.94 Z";

const ROOF_512 =
  "M 276.69,154.06 Q 275.92,152.00 278.12,152.00 L 319.72,152.00 Q 321.92,152.00 322.69,154.06 L 369.51,278.90 Q 370.28,280.96 368.08,280.96 L 326.48,280.96 Q 324.28,280.96 323.51,278.90 Z";

const FLOOR_512 =
  "M 209.05,322.68 Q 209.05,320.48 211.25,320.48 L 382.90,320.48 Q 385.10,320.48 385.87,322.54 L 399.15,357.94 Q 399.92,360.00 397.72,360.00 L 211.25,360.00 Q 209.05,360.00 209.05,357.80 Z";

/**
 * The Cabin brand mark: a layered three-piece architectural mark matching
 * `src-tauri/icons/icon.svg` and `src-tauri/icons/tray.svg`.
 *
 * - `color`: bare emblem that adapts to light and dark themes (`currentColor`
 *   for the gable, `var(--accent)` for the terracotta roof slope, and a softly
 *   lifted `currentColor` foundation beam below the panoramic cut).
 * - `mono`: pure single-colour silhouette matching the macOS tray template.
 * - `tile`: full warm-porcelain squircle app icon with subtle tactile depth.
 * - `animated`: traces a full-circuit racetrack loop through all three pieces for startup.
 */
export function CabinMark({
  size = 24,
  variant = "color",
  animated = false,
  className,
}: {
  size?: number;
  variant?: CabinMarkVariant;
  animated?: boolean;
  className?: string;
}) {
  const id = useId();

  if (variant === "tile") {
    const bg = `${id}-bg`;
    const sheen = `${id}-sheen`;
    const bounce = `${id}-bounce`;
    const bevel = `${id}-bevel`;
    const ink = `${id}-ink`;
    const terra = `${id}-terra`;
    const plinth = `${id}-plinth`;
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
          <linearGradient id={bg} x1="0.18" y1="0" x2="0.82" y2="1">
            <stop offset="0%" stopColor="#fcfaf6" />
            <stop offset="54%" stopColor="#f3eee5" />
            <stop offset="100%" stopColor="#e6dfd2" />
          </linearGradient>
          <radialGradient id={sheen} cx="26%" cy="16%" r="74%">
            <stop offset="0%" stopColor="#ffffff" stopOpacity="0.72" />
            <stop offset="52%" stopColor="#ffffff" stopOpacity="0.08" />
            <stop offset="100%" stopColor="#ffffff" stopOpacity="0" />
          </radialGradient>
          <radialGradient id={bounce} cx="66%" cy="46%" r="36%">
            <stop offset="0%" stopColor="#e58044" stopOpacity="0.12" />
            <stop offset="100%" stopColor="#e58044" stopOpacity="0" />
          </radialGradient>
          <linearGradient id={bevel} x1="0.5" y1="0.82" x2="0.5" y2="1">
            <stop offset="0%" stopColor="#1d1c1a" stopOpacity="0" />
            <stop offset="100%" stopColor="#1d1c1a" stopOpacity="0.055" />
          </linearGradient>
          <linearGradient id={ink} x1="0.2" y1="0" x2="0.8" y2="1">
            <stop offset="0%" stopColor="#332f2b" />
            <stop offset="44%" stopColor="#201e1c" />
            <stop offset="100%" stopColor="#131210" />
          </linearGradient>
          <linearGradient id={terra} x1="0.15" y1="0" x2="0.85" y2="1">
            <stop offset="0%" stopColor="#f29356" />
            <stop offset="48%" stopColor="#d56b2f" />
            <stop offset="100%" stopColor="#b04d1b" />
          </linearGradient>
          <linearGradient id={plinth} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0%" stopColor="#453f39" />
            <stop offset="100%" stopColor="#272421" />
          </linearGradient>
          <linearGradient id={rim} x1="0.5" y1="0" x2="0.5" y2="1">
            <stop offset="0%" stopColor="#ffffff" stopOpacity="0.96" />
            <stop offset="45%" stopColor="#ffffff" stopOpacity="0.22" />
            <stop offset="100%" stopColor="#1d1c1a" stopOpacity="0.16" />
          </linearGradient>
          <clipPath id={clip}>
            <path d="M 168,44 L 344,44 C 426,44 468,86 468,168 L 468,344 C 468,426 426,468 344,468 L 168,468 C 86,468 44,426 44,344 L 44,168 C 44,86 86,44 168,44 Z" />
          </clipPath>
        </defs>
        <g clipPath={`url(#${clip})`}>
          <rect x="44" y="44" width="424" height="424" fill={`url(#${bg})`} />
          <rect x="44" y="44" width="424" height="424" fill={`url(#${sheen})`} />
          <rect x="44" y="44" width="424" height="424" fill={`url(#${bounce})`} />
          <rect x="44" y="44" width="424" height="424" fill={`url(#${bevel})`} />
          <path d={GABLE_512} fill={`url(#${ink})`} />
          <path d={ROOF_512} fill={`url(#${terra})`} />
          <path d={FLOOR_512} fill={`url(#${plinth})`} />
        </g>
        <path d="M 168,45 L 344,45 C 425.4,45 467,86.6 467,168 L 467,344 C 467,425.4 425.4,467 344,467 L 168,467 C 86.6,467 45,425.4 45,344 L 45,168 C 45,86.6 86.6,45 168,45 Z" fill="none" stroke={`url(#${rim})`} strokeWidth="2.5" />
      </svg>
    );
  }

  const roofColor = variant === "color" ? "var(--accent)" : "currentColor";
  const floorOpacity = variant === "color" ? 0.78 : 1;

  if (animated) {
    const clipGable = `${id}-cg`;
    const clipRoof = `${id}-cr`;
    const clipFloor = `${id}-cf`;
    return (
      <svg
        className={className}
        width={size}
        height={size}
        viewBox="0 0 24 24"
        aria-hidden="true"
        focusable="false"
      >
        <defs>
          <clipPath id={clipGable}><path d={GABLE_24} /></clipPath>
          <clipPath id={clipRoof}><path d={ROOF_24} /></clipPath>
          <clipPath id={clipFloor}><path d={FLOOR_24} /></clipPath>
        </defs>
        {/* Subtle etched track base (rises to full opacity in Reduce Motion / Lite mode) */}
        <path className="cabin-track-base" fill="currentColor" opacity={0.15} d={GABLE_24} />
        <path className="cabin-track-base" fill={roofColor} opacity={0.22} d={ROOF_24} />
        <path className="cabin-track-base" fill="currentColor" opacity={0.14} d={FLOOR_24} />
        {/* Luminous circuit racing through gable -> terracotta roof -> foundation bar */}
        <g className="cabin-track-run" clipPath={`url(#${clipGable})`}>
          <path
            className="cabin-track-main"
            d={TRACK_GABLE_24}
            fill="none"
            stroke="currentColor"
            strokeWidth={4.6}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </g>
        <g className="cabin-track-run" clipPath={`url(#${clipRoof})`}>
          <path
            className="cabin-track-slash"
            d={TRACK_ROOF_24}
            fill="none"
            stroke={roofColor}
            strokeWidth={4.6}
            strokeLinecap="round"
          />
        </g>
        <g className="cabin-track-run" clipPath={`url(#${clipFloor})`}>
          <path
            className="cabin-track-floor"
            d={TRACK_FLOOR_24}
            fill="none"
            stroke="currentColor"
            strokeOpacity={floorOpacity}
            strokeWidth={4.2}
            strokeLinecap="round"
          />
        </g>
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
      <path fill="currentColor" d={GABLE_24} />
      <path fill={roofColor} d={ROOF_24} />
      <path fill="currentColor" opacity={floorOpacity} d={FLOOR_24} />
    </svg>
  );
}

/** Brand lockup: Cabin mark beside the Cabin wordmark. */
export function CabinWordmark({
  size = 20,
  animated = false,
  className = "",
}: {
  size?: number;
  animated?: boolean;
  className?: string;
}) {
  return (
    <span className={`inline-flex items-center gap-2 text-fg ${className}`.trim()}>
      <CabinMark size={size} variant="color" animated={animated} className="shrink-0" />
      <span className="text-[14px] font-semibold tracking-[-0.02em]">Cabin</span>
    </span>
  );
}
