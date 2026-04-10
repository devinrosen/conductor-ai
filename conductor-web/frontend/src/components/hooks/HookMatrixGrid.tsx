import { useRef, useEffect } from "react";
import type { HookSummary, HookEvent } from "../../api/types";
import { HookTypeIcon } from "./HookTypeIcon";
import { WildcardBadge } from "./WildcardBadge";
import { CellToggle, type CellMode } from "./CellToggle";

/** Returns true when `pattern` is a wildcard that covers `eventName`. */
function wildcardCovers(pattern: string, eventName: string): boolean {
  if (pattern === "*") return true;
  if (pattern.endsWith(".*")) {
    const prefix = pattern.slice(0, -2);
    return eventName.startsWith(prefix + ".");
  }
  return false;
}

/** Split comma-separated on-pattern into individual patterns, trimmed. */
function splitOn(on: string): string[] {
  return on
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * Derive the cell mode for a given (hook, event) pair.
 *
 * Checks both `event:root` and plain `event` sub-patterns, plus wildcard coverage.
 */
function cellMode(
  hook: HookSummary,
  draft: Map<number, string>,
  eventName: string,
): { on: string; mode: CellMode; wildcard: boolean } {
  const on = draft.get(hook.index) ?? hook.on;
  const parts = splitOn(on);

  // Explicit :root match
  if (parts.includes(`${eventName}:root`)) {
    return { on, mode: "root", wildcard: false };
  }
  // Explicit exact match
  if (parts.includes(eventName)) {
    return { on, mode: "any", wildcard: false };
  }
  // Wildcard coverage (read-only)
  if (parts.some((p) => wildcardCovers(p.replace(/:root$/, ""), eventName))) {
    return { on, mode: "off", wildcard: true };
  }
  return { on, mode: "off", wildcard: false };
}

interface HookMatrixGridProps {
  hooks: HookSummary[];
  events: HookEvent[];
  draft: Map<number, string>;
  inFlight: Set<number>;
  onCellChange: (hookIndex: number, eventName: string, mode: CellMode, currentOn: string) => void;
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
  onCellChange,
  onColumnAll,
}: HookMatrixGridProps) {
  if (hooks.length === 0) return null;

  return (
    <div className="overflow-x-auto">
      <table className="border-collapse text-sm w-full">
        <thead>
          <tr>
            {/* top-left corner cell */}
            <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 uppercase tracking-wider border-b border-gray-200 dark:border-gray-600 min-w-[160px]">
              Event
            </th>
            {hooks.map((hook) => {
              const on = draft.get(hook.index) ?? hook.on;
              const parts = splitOn(on);
              const isWildcard = parts.some((p) => p.replace(/:root$/, "").includes("*"));
              const allChecked = parts.includes("*");
              const isPartialWildcard = isWildcard && !allChecked;

              return (
                <th
                  key={hook.index}
                  className="px-3 py-2 text-center text-xs font-medium text-gray-500 border-b border-gray-200 dark:border-gray-600 min-w-[100px] max-w-[160px]"
                >
                  <div className="flex flex-col items-center gap-1.5">
                    <div
                      className="flex items-center gap-1.5"
                      title={hook.command ?? ""}
                    >
                      <HookTypeIcon kind={hook.kind} />
                      <span className="font-mono truncate max-w-[130px] inline-block">
                        {hook.label || "—"}
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
        <tbody className="divide-y divide-gray-100 dark:divide-gray-700">
          {events.map((event, rowIdx) => (
            <tr
              key={event.name}
              className={rowIdx % 2 === 0 ? "bg-white dark:bg-gray-800" : "bg-gray-50 dark:bg-gray-800/50"}
            >
              <td className="px-3 py-2 text-xs text-gray-700 dark:text-gray-300 whitespace-nowrap">
                <div>{event.label}</div>
                <div className="font-mono text-gray-400 dark:text-gray-500">{event.name}</div>
              </td>
              {hooks.map((hook) => {
                const cell = cellMode(hook, draft, event.name);
                return (
                  <td key={hook.index} className="px-2 py-2 text-center">
                    {cell.wildcard ? (
                      <div className="flex justify-center">
                        <WildcardBadge pattern={cell.on} />
                      </div>
                    ) : (
                      <CellToggle
                        mode={cell.mode}
                        isWorkflow={event.is_workflow}
                        disabled={inFlight.has(hook.index)}
                        onChange={(mode) =>
                          onCellChange(hook.index, event.name, mode, cell.on)
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
        <code className="bg-gray-100 dark:bg-gray-700 px-1 py-0.5 rounded">~/.conductor/config.toml</code>{" "}
        to use threshold-based events (cost spike, duration spike).
      </p>
    </div>
  );
}
