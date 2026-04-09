interface WildcardBadgeProps {
  pattern: string;
}

export function WildcardBadge({ pattern }: WildcardBadgeProps) {
  return (
    <span
      className="inline-flex items-center px-1 py-0.5 rounded text-xs font-mono bg-amber-100 text-amber-700 border border-amber-200"
      title={`Covered by wildcard pattern: ${pattern}`}
    >
      {pattern}
    </span>
  );
}
