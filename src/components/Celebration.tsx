export function Celebration({ size = 56 }: { size?: number }) {
  return (
    <span className="success-check" style={{ width: size, height: size }} aria-hidden>
      <svg viewBox="0 0 52 52" width={size} height={size}>
        <circle className="success-check-circle" cx="26" cy="26" r="24" />
        <path className="success-check-mark" d="M15 27 l7.5 7.5 L37.5 19" />
      </svg>
    </span>
  );
}
