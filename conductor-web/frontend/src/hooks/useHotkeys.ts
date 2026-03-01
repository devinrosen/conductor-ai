import { useEffect, useRef } from "react";

export interface HotkeyDef {
  key: string;
  handler: () => void;
  description: string;
  enabled?: boolean;
}

export function useHotkeys(hotkeys: HotkeyDef[]) {
  const hotkeysRef = useRef(hotkeys);
  hotkeysRef.current = hotkeys;
  const pendingRef = useRef<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      const el = e.target as HTMLElement;
      const tag = el?.tagName;
      const isInput =
        tag === "INPUT" ||
        tag === "TEXTAREA" ||
        tag === "SELECT" ||
        el?.isContentEditable;

      // Always allow Escape, even in inputs
      if (isInput && e.key !== "Escape") return;

      // Don't fire on modifier combos (allow Shift for ? etc.)
      if (e.ctrlKey || e.metaKey || e.altKey) return;

      const current = hotkeysRef.current;

      // Check for sequence match first (e.g., "g d")
      if (pendingRef.current) {
        const seq = pendingRef.current + " " + e.key;
        pendingRef.current = null;
        clearTimeout(timerRef.current);
        const match = current.find(
          (h) => h.key === seq && (h.enabled ?? true),
        );
        if (match) {
          e.preventDefault();
          match.handler();
          return;
        }
        // No sequence match — fall through to check as standalone key
      }

      // Check if this key starts a sequence
      const hasSequence = current.some(
        (h) => h.key.startsWith(e.key + " ") && (h.enabled ?? true),
      );
      if (hasSequence) {
        pendingRef.current = e.key;
        timerRef.current = setTimeout(() => {
          pendingRef.current = null;
        }, 500);
        return;
      }

      // Single key match
      const match = current.find(
        (h) => h.key === e.key && (h.enabled ?? true),
      );
      if (match) {
        e.preventDefault();
        match.handler();
      }
    }

    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      clearTimeout(timerRef.current);
    };
  }, []);
}
