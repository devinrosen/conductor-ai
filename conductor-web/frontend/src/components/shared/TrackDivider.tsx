/**
 * Track-style section divider with station dots.
 *
 * Renders: ──●──────────────●──
 */
export function TrackDivider({ className = "" }: { className?: string }) {
  return (
    <div className={`flex items-center gap-0 ${className}`} aria-hidden>
      <div className="w-3 h-px bg-gray-300" />
      <div className="w-1.5 h-1.5 rounded-full bg-gray-400 shrink-0" />
      <div className="flex-1 h-px bg-gray-300" />
      <div className="w-1.5 h-1.5 rounded-full bg-gray-400 shrink-0" />
      <div className="w-3 h-px bg-gray-300" />
    </div>
  );
}
