import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";

/**
 * Onboarding highlight that draws attention to a specific UI element
 * when the user arrives from the Getting Started guide.
 *
 * Renders a pulsing glow ring around the child element with a label badge.
 * Auto-dismisses after 8 seconds or on click.
 */

export function OnboardingHint({
  target,
  label,
  children,
}: {
  target: string;
  label?: string;
  children: React.ReactNode;
}) {
  const [searchParams, setSearchParams] = useSearchParams();
  const isHighlighted = searchParams.get("highlight") === target;
  const [visible, setVisible] = useState(isHighlighted);

  useEffect(() => {
    if (!isHighlighted) { setVisible(false); return; }
    setVisible(true);
    const timer = setTimeout(() => {
      setVisible(false);
      const next = new URLSearchParams(searchParams);
      next.delete("highlight");
      setSearchParams(next, { replace: true });
    }, 8000);
    return () => clearTimeout(timer);
  }, [isHighlighted, searchParams, setSearchParams]);

  function dismiss() {
    setVisible(false);
    const next = new URLSearchParams(searchParams);
    next.delete("highlight");
    setSearchParams(next, { replace: true });
  }

  if (!visible) return <>{children}</>;

  return (
    <div className="relative inline-block" onClick={dismiss}>
      <div className="absolute -inset-1.5 rounded-lg border-2 border-amber-400 animate-pulse pointer-events-none z-10" />
      {label && (
        <span className="absolute -top-6 left-0 text-[11px] font-semibold text-amber-400 bg-amber-400/10 px-2 py-0.5 rounded-full whitespace-nowrap z-10">
          {label}
        </span>
      )}
      {children}
    </div>
  );
}

/**
 * Hook to check if a highlight param is active.
 */
export function useOnboardingHighlight(target: string): boolean {
  const [searchParams] = useSearchParams();
  return searchParams.get("highlight") === target;
}
