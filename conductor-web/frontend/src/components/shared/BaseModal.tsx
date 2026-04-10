import { useRef, useId } from "react";
import type { ReactNode } from "react";
import { useFocusTrap } from "../../hooks/useFocusTrap";

interface BaseModalProps {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  className?: string;
  preventCloseOnBackdrop?: boolean;
  titleId?: string;
}

export function BaseModal({
  open,
  onClose,
  children,
  className = "bg-white rounded-lg shadow-lg max-w-lg w-full mx-4 outline-none modal-panel",
  preventCloseOnBackdrop = false,
  titleId,
}: BaseModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const generatedTitleId = useId();
  const actualTitleId = titleId || generatedTitleId;

  useFocusTrap(dialogRef, open, onClose);

  if (!open) return null;

  const handleBackdropClick = () => {
    if (!preventCloseOnBackdrop) {
      onClose();
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4 modal-backdrop"
      onClick={handleBackdropClick}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={actualTitleId}
        tabIndex={-1}
        className={className}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

