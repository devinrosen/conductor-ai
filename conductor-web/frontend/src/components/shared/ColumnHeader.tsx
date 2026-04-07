import { useState, useRef, useEffect } from "react";

export type SortDirection = "asc" | "desc" | null;

interface ColumnHeaderProps {
  label: string;
  columnKey: string;
  sortDirection: SortDirection;
  onSort: (columnKey: string, direction: SortDirection) => void;
  filterOptions?: string[];
  activeFilters?: Set<string>;
  onFilter?: (columnKey: string, values: Set<string>) => void;
  className?: string;
}

export function ColumnHeader({
  label,
  columnKey,
  sortDirection,
  onSort,
  filterOptions,
  activeFilters,
  onFilter,
  className = "",
}: ColumnHeaderProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  const hasFilter = filterOptions && onFilter;
  const filterActive = activeFilters && activeFilters.size > 0;

  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  function cycleSort() {
    const next: SortDirection =
      sortDirection === null ? "asc" : sortDirection === "asc" ? "desc" : null;
    onSort(columnKey, next);
  }

  function toggleFilter(value: string) {
    if (!onFilter || !activeFilters) return;
    const next = new Set(activeFilters);
    if (next.has(value)) next.delete(value);
    else next.add(value);
    onFilter(columnKey, next);
  }

  return (
    <th className={`px-4 py-2 ${className}`}>
      <div className="relative inline-flex items-center" ref={ref}>
        <button
          className="inline-flex items-center gap-0.5 uppercase text-xs font-medium text-gray-500 hover:text-gray-700"
          onClick={cycleSort}
        >
          {label}
          {sortDirection === "asc" && <span className="text-indigo-500 text-[10px] ml-0.5">&#9650;</span>}
          {sortDirection === "desc" && <span className="text-indigo-500 text-[10px] ml-0.5">&#9660;</span>}
        </button>
        {hasFilter && (
          <button
            onClick={() => setOpen(!open)}
            className={`ml-1 p-0.5 rounded ${filterActive ? "text-indigo-500" : "text-gray-300 hover:text-gray-400"}`}
          >
            <svg className="w-2.5 h-2.5" viewBox="0 0 16 16" fill="currentColor">
              <path d="M1 2h14l-5.5 6.5V14l-3-1.5V8.5z" />
            </svg>
          </button>
        )}
        {open && hasFilter && filterOptions && (
          <div className="absolute top-full left-0 mt-1 z-50 bg-white border border-gray-200 rounded-lg shadow-lg py-1 min-w-[140px] max-h-60 overflow-y-auto">
            {filterActive && (
              <button
                onClick={() => { onFilter(columnKey, new Set()); setOpen(false); }}
                className="w-full text-left px-3 py-1 text-[11px] text-gray-400 hover:text-gray-600 hover:bg-gray-50"
              >
                Clear
              </button>
            )}
            {filterOptions.map((opt) => (
              <label
                key={opt}
                className="flex items-center gap-2 px-3 py-1 text-[11px] hover:bg-gray-50 cursor-pointer"
              >
                <input
                  type="checkbox"
                  checked={activeFilters?.has(opt) ?? false}
                  onChange={() => toggleFilter(opt)}
                  className="rounded border-gray-300 text-indigo-600 focus:ring-indigo-500 w-3 h-3"
                />
                <span className="truncate">{opt}</span>
              </label>
            ))}
          </div>
        )}
      </div>
    </th>
  );
}
