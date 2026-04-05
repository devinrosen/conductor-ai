import { useEffect, useRef } from "react";

/**
 * Traps keyboard focus within a dialog element while it is open.
 *
 * Handles:
 * - Storing and restoring the previously-focused element
 * - Focusing the dialog on open
 * - Trapping Tab / Shift+Tab within focusable children
 * - Calling `onClose` on Escape
 */
export function useFocusTrap(
  dialogRef: React.RefObject<HTMLElement | null>,
  open: boolean,
  onClose?: () => void,
): void {
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;

    // Store the element that had focus before the dialog opened
    previousFocusRef.current = document.activeElement as HTMLElement;

    // Focus the dialog itself
    dialogRef.current?.focus();

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        onClose?.();
        return;
      }
      // Trap focus within the dialog
      if (e.key === "Tab") {
        const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
        );
        if (!focusable || focusable.length === 0) return;
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }

    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      // Restore focus on close
      previousFocusRef.current?.focus();
    };
  }, [open, onClose, dialogRef]);
}
