interface HookTypeIconProps {
  kind: "shell" | "http";
  className?: string;
}

export function HookTypeIcon({ kind, className = "" }: HookTypeIconProps) {
  if (kind === "http") {
    return (
      <span
        className={`inline-flex items-center justify-center text-blue-500 ${className}`}
        title="HTTP hook"
        aria-label="HTTP hook"
      >
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
          <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
        </svg>
      </span>
    );
  }

  return (
    <span
      className={`inline-flex items-center justify-center text-gray-600 ${className}`}
      title="Shell hook"
      aria-label="Shell hook"
    >
      <svg
        width="14"
        height="14"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <polyline points="4 17 10 11 4 5" />
        <line x1="12" y1="19" x2="20" y2="19" />
      </svg>
    </span>
  );
}
