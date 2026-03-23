/**
 * Station name plate section header.
 *
 * Styled after railway station signage — uppercase text with a thin
 * brass underline accent. Replaces plain section headers.
 */
export function StationHeader({
  children,
  count,
}: {
  children: React.ReactNode;
  count?: number;
}) {
  return (
    <div className="mb-2">
      <div className="flex items-center gap-2">
        <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-400">
          {children}
          {count !== undefined && (
            <span className="ml-1 font-mono">({count})</span>
          )}
        </h3>
      </div>
      <div
        className="mt-1 h-px"
        style={{
          background: "linear-gradient(to right, #CD853F 0%, #CD853F 30%, transparent 100%)",
        }}
      />
    </div>
  );
}
