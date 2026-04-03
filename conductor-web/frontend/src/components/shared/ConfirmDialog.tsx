import { useEffect, useRef } from "react";
import { BaseModal, useModalTitleId } from "./BaseModal";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  onConfirm: () => void;
  onCancel: () => void;
  loading?: boolean;
}

export function ConfirmDialog({
  open,
  title,
  message,
  onConfirm,
  onCancel,
  loading = false,
}: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const titleId = useModalTitleId();

  const handleClose = () => {
    if (!loading) onCancel();
  };

  // Focus the Cancel button (safe default for destructive dialogs)
  useEffect(() => {
    if (!open) return;
    requestAnimationFrame(() => cancelRef.current?.focus());
  }, [open]);

  return (
    <BaseModal
      open={open}
      onClose={handleClose}
      className="bg-white rounded-lg shadow-lg p-6 max-w-sm w-full mx-4 outline-none modal-panel"
      preventCloseOnBackdrop={loading}
    >
      <h3 id={titleId} className="text-lg font-semibold text-gray-900">{title}</h3>
      <p className="mt-2 text-sm text-gray-600">{message}</p>
      <div className="mt-4 flex justify-end gap-2">
        <button
          ref={cancelRef}
          onClick={onCancel}
          disabled={loading}
          className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 active:scale-95 transition-transform disabled:opacity-50"
        >
          Cancel
        </button>
        <button
          onClick={onConfirm}
          disabled={loading}
          className="px-3 py-1.5 text-sm rounded-md bg-red-600 text-white hover:bg-red-700 hover:brightness-110 active:scale-95 transition-transform disabled:opacity-50"
        >
          {loading ? "Deleting..." : "Confirm"}
        </button>
      </div>
    </BaseModal>
  );
}
