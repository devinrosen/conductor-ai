import { useRef, useEffect } from "react";
import type { HookSummary, HookEvent } from "../../api/types";
import { HookTypeIcon } from "./HookTypeIcon";
import { WildcardBadge } from "./WildcardBadge";

/** Returns true when `pattern` is a wildcard that covers `eventName`. */
function wildcardCovers(pattern: string, eventName: string): boolean {
  if (pattern === "*") return true;
  if (pattern.endsWith(".*")) {
    const prefix = pattern.slice(0, -2);
    return eventName.startsWith(prefix + ".");
  }
  return false;
}

interface CellState {
  /** Hook's current on-pattern after applying draft overrides */
  on: string;
  /** Exact match for this event */
  checked: boolean;
  /** Covered by a wildcard (read-only display) */
  wildcard: boolean;
}

function cellState(hook: HookSummary, draft: Map<number, string>, eventName: string): CellState {
  const on = draft.get(hook.index) ?? hook.on;
  const checked = on === eventName;
  const wildcard = !checked && wildcardCovers(on, eventName);
  return { on, checked, wildcard };
}

interface HookMatrixGridProps {
  hooks: HookSummary[];
  events: HookEvent[];
  draft: Map<number, string>;
  inFlight: Set<number>;
  onCellToggle: (hookIndex: number, eventName: string, currentOn: string) => void;
  onColumnAll: (hookIndex: number, currentOn: string) => void;
}

/** Checkbox that supports indeterminate state */
function TriCheckbox({
  checked,
  indeterminate,
  disabled,
  onChange,
  title,
}: {
  checked: boolean;
  indeterminate?: boolean;
  disabled?: boolean;
  onChange: () => void;
  title?: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate ?? false;
  }, [indeterminate]);

  return (
    <input
      ref={ref}
      type="checkbox"
      checked={checked}
      disabled={disabled}
      onChange={onChange}
      title={title}
      className="h-4 w-4 rounded border-gray-300 text-blue-600 cursor-pointer disabled:cursor-not-allowed disabled:opacity-60"
    />
  );
}

export function HookMatrixGrid({
  hooks,
  events,
  draft,
  inFlight,
  onCellToggle,
  onColumnAll,
}: HookMatrixGridProps) {
  if (hooks.length === 0) return null;

  return (
    <div className="overflow-x-auto">
      <table className="border-collapse text-sm w-full">
        <thead>
          <tr>
            {/* top-left corner cell */}
            <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider border-b border-gray-200 min-w-[160px]">
              Event
            </th>
            {hooks.map((hook) => {
              const on = draft.get(hook.index) ?? hook.on;
              const isWildcard = on.includes("*");
              const allChecked = on === "*";
              const isPartialWildcard = isWildcard && !allChecked;

              return (
                <th
                  key={hook.index}
                  className="px-2 py-2 text-center text-xs font-medium text-gray-500 border-b border-gray-200 max-w-[120px]"
                >
                  <div className="flex flex-col items-center gap-1">
                    <div className="flex items-center gap-1">
                      <HookTypeIcon kind={hook.kind} />
                      <span
                        className="font-mono truncate max-w-[80px] inline-block"
                        title={hook.command ?? ""}
                      >
                        {hook.command ?? "—"}
                      </span>
                    </div>
                    <TriCheckbox
                      checked={allChecked}
                      indeterminate={isPartialWildcard}
                      disabled={inFlight.has(hook.index)}
                      onChange={() => onColumnAll(hook.index, on)}
                      title={
                        allChecked
                          ? "Remove wildcard (fires on all events)"
                          : "Set to wildcard * (fires on all events)"
                      }
                    />
                  </div>
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-100">
          {events.map((event, rowIdx) => (
            <tr key={event.name} className={rowIdx % 2 === 0 ? "bg-white" : "bg-gray-50"}>
              <td className="px-3 py-2 text-xs text-gray-700 whitespace-nowrap">
                <div>{event.label}</div>
                <div className="font-mono text-gray-400">{event.name}</div>
              </td>
              {hooks.map((hook) => {
                const cell = cellState(hook, draft, event.name);
                return (
                  <td key={hook.index} className="px-2 py-2 text-center">
                    {cell.wildcard ? (
                      <div className="flex justify-center">
                        <WildcardBadge pattern={cell.on} />
                      </div>
                    ) : (
                      <TriCheckbox
                        checked={cell.checked}
                        disabled={inFlight.has(hook.index)}
                        onChange={() => onCellToggle(hook.index, event.name, cell.on)}
                        title={
                          cell.checked
                            ? `Unset — hook currently fires on "${cell.on}"`
                            : `Set hook to fire on "${event.name}"`
                        }
                      />
                    )}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>

      <p className="mt-2 text-xs text-gray-400">
        Wildcard badges indicate the hook fires on all matching events via a glob pattern.
        Edit the pattern directly in{" "}
        <code className="bg-gray-100 px-1 py-0.5 rounded">~/.conductor/config.toml</code>{" "}
        to use threshold-based events (cost spike, duration spike).
      </p>
    </div>
  );
}
