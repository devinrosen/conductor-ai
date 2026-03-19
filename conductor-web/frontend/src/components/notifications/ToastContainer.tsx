import type { Toast } from "../../hooks/useToast";

const severityStyles: Record<string, string> = {
  info: "bg-blue-50 border-blue-200 text-blue-800",
  warning: "bg-yellow-50 border-yellow-200 text-yellow-800",
  action_required: "bg-red-50 border-red-200 text-red-800",
};

interface ToastContainerProps {
  toasts: Toast[];
  onDismiss: (id: string) => void;
}

export function ToastContainer({ toasts, onDismiss }: ToastContainerProps) {
  if (toasts.length === 0) return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className={`border rounded-lg px-4 py-3 shadow-md animate-in slide-in-from-right ${
            severityStyles[toast.severity] || severityStyles.info
          }`}
        >
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0">
              <p className="text-sm font-semibold truncate">{toast.title}</p>
              <p className="text-xs mt-0.5 truncate opacity-80">{toast.body}</p>
            </div>
            <button
              onClick={() => onDismiss(toast.id)}
              className="text-current opacity-50 hover:opacity-100 shrink-0"
              aria-label="Dismiss"
            >
              ✕
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
