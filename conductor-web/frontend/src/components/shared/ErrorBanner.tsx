import { railwayError } from "../../utils/railwayErrors";

interface ErrorBannerProps {
  error: string | null;
  onRetry?: () => void;
  onDismiss?: () => void;
}

export function ErrorBanner({ error, onRetry, onDismiss }: ErrorBannerProps) {
  if (!error) return null;
  return (
    <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700 flex items-center justify-between gap-3">
      <span>{railwayError(error)}</span>
      <span className="flex items-center gap-2 shrink-0">
        {onRetry && (
          <button
            onClick={onRetry}
            className="text-xs font-medium text-red-700 hover:text-red-900 underline"
          >
            Try again
          </button>
        )}
        {onDismiss && (
          <button
            onClick={onDismiss}
            className="text-red-400 hover:text-red-600"
            aria-label="Dismiss error"
          >
            &times;
          </button>
        )}
      </span>
    </div>
  );
}
