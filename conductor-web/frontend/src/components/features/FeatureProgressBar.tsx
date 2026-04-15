interface FeatureProgressBarProps {
  merged: number;
  total: number;
}

export function FeatureProgressBar({ merged, total }: FeatureProgressBarProps) {
  const pct = total > 0 ? Math.round((merged / total) * 100) : 0;

  return (
    <div className="flex items-center gap-2 min-w-[120px]">
      <div className="flex-1 h-1.5 bg-gray-200 rounded-full overflow-hidden">
        <div
          className="h-full bg-indigo-500 rounded-full transition-all"
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="text-xs text-gray-500 whitespace-nowrap">
        {merged}/{total}
      </span>
    </div>
  );
}
