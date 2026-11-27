import { IconAlertTriangle, IconCheck, IconInfoCircle, IconX } from "@tabler/icons-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { stackHeight, stackLayout } from "../lib/toastStack";
import { uiActions, useUi, type Toast } from "../state/ui";

function ToneIcon({ tone, size = 13 }: { tone: Toast["tone"]; size?: number }) {
  if (tone === "loading") {
    return (
      <svg className="toast-spinner" width={size + 3} height={size + 3} viewBox="0 0 16 16" aria-hidden>
        <circle cx="8" cy="8" r="6" />
      </svg>
    );
  }
  const Icon = tone === "success" ? IconCheck : tone === "error" ? IconAlertTriangle : IconInfoCircle;
  return <Icon size={size} stroke={2.4} />;
}

function ToastCard({ toast, extraCount, onHeight }: { toast: Toast; extraCount: number; onHeight: (height: number) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    onHeight(element.offsetHeight);
    const observer = new ResizeObserver(() => onHeight(element.offsetHeight));
    observer.observe(element);
    return () => observer.disconnect();
  }, [onHeight]);

  return (
    <div
      ref={ref}
      className={`toast ${toast.tone}${toast.leaving ? " leaving" : ""}`}
      role={toast.tone === "error" ? "alert" : "status"}
    >
      {toast.image ? (
        <span className="toast-thumb">
          <img src={toast.image} alt="" />
          <span className="toast-badge" key={toast.tone} aria-hidden><ToneIcon tone={toast.tone} size={9} /></span>
        </span>
      ) : (
        <span className="toast-icon" key={toast.tone} aria-hidden><ToneIcon tone={toast.tone} /></span>
      )}
      <span className="toast-copy">
        <span className="toast-text" key={`m${toast.revision}`}>{toast.message}</span>
        {toast.detail && <span className="toast-detail" key={`d${toast.revision}`}>{toast.detail}</span>}
      </span>
      {extraCount > 0 && (
        <span className="toast-more" aria-hidden>+{extraCount}</span>
      )}
      <button type="button" className="toast-close" aria-label="Dismiss" onClick={() => uiActions.dismissToast(toast.id)}>
        <IconX size={12} stroke={2.4} />
      </button>
      {toast.duration > 0 && !toast.leaving && (
        <span
          key={`t${toast.revision}`}
          className="toast-timer"
          style={{ animationDuration: `${toast.duration}ms` }}
          aria-hidden
        />
      )}
    </div>
  );
}

/**
 * Notices at the top centre, styled as a Dynamic Island deck: the newest in
 * front, older ones tucked behind it. Hovering fans them out and holds their
 * timers. A toast can morph in place (e.g. "Attaching…" → "Attached").
 */
export function Toasts() {
  const toasts = useUi((ui) => ui.toasts);
  const paused = useUi((ui) => ui.toastsPaused);
  const [heights, setHeights] = useState<Record<number, number>>({});
  const measure = useRef(new Map<number, (height: number) => void>());
  const newestFirst = toasts.slice().reverse();
  const stacked = newestFirst.length > 1;
  const expanded = paused && stacked;
  const sizes = newestFirst.map((toast) => heights[toast.id] ?? 0);
  const slots = stackLayout(sizes, expanded);

  // Forget the sizes of toasts that are gone.
  useEffect(() => {
    const live = new Set(toasts.map((toast) => toast.id));
    for (const id of measure.current.keys()) if (!live.has(id)) measure.current.delete(id);
    setHeights((current) => {
      const kept = Object.fromEntries(Object.entries(current).filter(([id]) => live.has(Number(id))));
      return Object.keys(kept).length === Object.keys(current).length ? current : kept;
    });
  }, [toasts]);

  const onHeight = (id: number) => {
    let callback = measure.current.get(id);
    if (!callback) {
      callback = (height: number) => setHeights((current) => (current[id] === height ? current : { ...current, [id]: height }));
      measure.current.set(id, callback);
    }
    return callback;
  };

  return (
    <div
      className={`toasts${stacked ? " stacked" : ""}${expanded ? " expanded" : ""}${paused ? " paused" : ""}`}
      style={{ height: stackHeight(sizes, expanded) }}
      aria-live="polite"
      onMouseEnter={() => uiActions.pauseToasts(true)}
      onMouseLeave={() => uiActions.pauseToasts(false)}
    >
      {newestFirst.map((toast, index) => {
        const slot = slots[index];
        return (
          <div
            key={toast.id}
            className="toast-slot"
            style={{
              transform: `translateY(${slot.y}px) scale(${slot.scale})`,
              opacity: toast.leaving ? 0 : slot.opacity,
              zIndex: slot.z,
              pointerEvents: toast.leaving || slot.opacity === 0 ? "none" : undefined,
            }}
            aria-hidden={!expanded && index > 0 ? true : undefined}
          >
            <ToastCard
              toast={toast}
              extraCount={!expanded && index === 0 ? newestFirst.length - 1 : 0}
              onHeight={onHeight(toast.id)}
            />
          </div>
        );
      })}
    </div>
  );
}
