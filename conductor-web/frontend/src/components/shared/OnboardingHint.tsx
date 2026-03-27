import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";

/**
 * Bouncing arrow indicator that draws attention to a specific UI element
 * when the user arrives from the Getting Started guide.
 *
 * Usage:
 *   <OnboardingHint target="create-worktree">
 *     <button>Create Worktree</button>
 *   </OnboardingHint>
 *
 * Shows a bouncing arrow + label when ?highlight=create-worktree is in the URL.
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
      // Clean up the URL param
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

  return (
    <div className="relative">
      {children}
      {visible && (
        <div
          className="absolute -top-8 left-1/2 -translate-x-1/2 flex flex-col items-center z-20 pointer-events-none"
          onClick={dismiss}
        >
          {label && (
            <span className="text-[10px] font-semibold text-indigo-500 bg-indigo-50 px-2 py-0.5 rounded-full mb-0.5 whitespace-nowrap pointer-events-auto cursor-pointer">
              {label}
            </span>
          )}
          <span className="text-indigo-500 text-lg animate-bounce pointer-events-auto cursor-pointer">↓</span>
        </div>
      )}
    </div>
  );
}

/**
 * Hook to check if a highlight param is active.
 * Useful when you need to auto-open a section (like settings).
 */
export function useOnboardingHighlight(target: string): boolean {
  const [searchParams] = useSearchParams();
  return searchParams.get("highlight") === target;
}
