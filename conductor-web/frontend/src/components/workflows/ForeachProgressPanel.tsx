import { useState, useEffect } from "react";
import { Link } from "react-router";
import { api } from "../../api/client";
import type { WorkflowRunStep, FanOutItem } from "../../api/types";

interface ForeachProgressPanelProps {
  step: WorkflowRunStep;
  runId: string;
  repoId: string;
  worktreeId: string;
  isRunActive: boolean;
}

function ItemStatusIcon({ status }: { status: FanOutItem["status"] }) {
  if (status === "completed") {
    return (
      <svg className="w-3.5 h-3.5 text-green-500 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.75.75 0 0 1 1.06-1.06L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z" />
      </svg>
    );
  }
  if (status === "failed") {
    return (
      <svg className="w-3.5 h-3.5 text-red-500 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M3.72 3.72a.75.75 0 0 1 1.06 0L8 6.94l3.22-3.22a.75.75 0 1 1 1.06 1.06L9.06 8l3.22 3.22a.75.75 0 1 1-1.06 1.06L8 9.06l-3.22 3.22a.75.75 0 0 1-1.06-1.06L6.94 8 3.72 4.78a.75.75 0 0 1 0-1.06Z" />
      </svg>
    );
  }
  if (status === "running") {
    return (
      <span className="relative flex w-3.5 h-3.5 shrink-0">
        <span className="absolute inset-0.5 rounded-full bg-amber-400/30 animate-ping" style={{ animationDuration: "2s" }} />
        <span className="relative inline-flex w-3.5 h-3.5 items-center justify-center">
          <span className="w-2 h-2 rounded-full bg-amber-500" />
        </span>
      </span>
    );
  }
  if (status === "skipped") {
    return (
      <svg className="w-3.5 h-3.5 text-gray-400 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M1.5 4.5a.75.75 0 0 1 1.28-.53l4.72 4.72 4.72-4.72a.75.75 0 1 1 1.06 1.06l-5.25 5.25a.75.75 0 0 1-1.06 0L1.72 5.03a.75.75 0 0 1-.22-.53Z" />
      </svg>
    );
  }
  // pending
  return <span className="w-3.5 h-3.5 rounded-full border-2 border-gray-300 shrink-0" />;
}

function ItemStatusBadge({ status }: { status: FanOutItem["status"] }) {
  const colors: Record<FanOutItem["status"], string> = {
    completed: "bg-green-50 text-green-700",
    failed: "bg-red-50 text-red-700",
    running: "bg-amber-50 text-amber-700",
    skipped: "bg-gray-100 text-gray-500",
    pending: "bg-gray-50 text-gray-400",
  };
  return (
    <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${colors[status]}`}>
      {status}
    </span>
  );
}

export function ForeachProgressPanel({
  step,
  runId,
  repoId,
  worktreeId,
  isRunActive,
}: ForeachProgressPanelProps) {
  const [expanded, setExpanded] = useState(false);
  const [items, setItems] = useState<FanOutItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);

  const total = step.fan_out_total ?? 0;
  const completed = step.fan_out_completed ?? 0;
  const failed = step.fan_out_failed ?? 0;
  const skipped = step.fan_out_skipped ?? 0;
  const running = Math.max(0, total - completed - failed - skipped);
  const pct = total > 0 ? Math.round((completed / total) * 100) : 0;

  // Fetch (and re-fetch while active) when the panel is expanded or progress counters change.
  const refetchKey = isRunActive ? step.fan_out_completed : 0;
  useEffect(() => {
    if (!expanded) return;

    let cancelled = false;

    async function fetchItems() {
      setLoading(true);
      setFetchError(null);
      try {
        const data = await api.getFanOutItems(runId, step.id);
        if (!cancelled) setItems(data);
      } catch (err) {
        if (!cancelled) setFetchError(err instanceof Error ? err.message : "Failed to load items");
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    fetchItems();
    return () => { cancelled = true; };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expanded, refetchKey, runId, step.id]);

  return (
    <div
      className="ml-6 mt-2 rounded-md border border-indigo-100 bg-indigo-50/40"
      onClick={(e) => e.stopPropagation()}
    >
      {/* Summary header */}
      <div className="px-3 py-2 space-y-1.5">
        <div className="flex items-center justify-between gap-2">
          <span className="text-xs text-indigo-700 font-medium">foreach</span>
          <span className="text-xs text-gray-500 font-mono tabular-nums">
            {completed}/{total}
            {running > 0 && <> · <span className="text-amber-600">{running} running</span></>}
            {failed > 0 && <> · <span className="text-red-600">{failed} failed</span></>}
            {skipped > 0 && <> · <span className="text-gray-400">{skipped} skipped</span></>}
          </span>
        </div>
        <div className="h-1.5 bg-indigo-100 rounded-full overflow-hidden">
          <div
            className="h-full bg-indigo-500 rounded-full transition-all duration-500"
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>

      {/* Expand/collapse toggle */}
      <button
        onClick={() => setExpanded((v) => !v)}
        className="flex items-center gap-1.5 px-3 pb-2 text-xs text-gray-500 hover:text-gray-700"
      >
        <svg
          className={`w-3 h-3 transition-transform duration-150 ${expanded ? "rotate-90" : ""}`}
          viewBox="0 0 16 16"
          fill="currentColor"
        >
          <path d="M6.22 4.22a.75.75 0 0 1 1.06 0l3.25 3.25a.75.75 0 0 1 0 1.06l-3.25 3.25a.75.75 0 0 1-1.06-1.06L8.94 8 6.22 5.28a.75.75 0 0 1 0-1.06Z" />
        </svg>
        {expanded ? "Hide items" : `Show ${total} items`}
      </button>

      {/* Per-item table */}
      {expanded && (
        <div className="border-t border-indigo-100">
          {loading ? (
            <p className="px-3 py-2 text-xs text-gray-400">Loading...</p>
          ) : fetchError ? (
            <p className="px-3 py-2 text-xs text-red-500">{fetchError}</p>
          ) : items.length === 0 ? (
            <p className="px-3 py-2 text-xs text-gray-400">No items yet.</p>
          ) : (
            <div className="divide-y divide-indigo-100 max-h-64 overflow-y-auto">
              {items.map((item) => (
                <div key={item.id} className="flex items-center justify-between gap-2 px-3 py-1.5">
                  <div className="flex items-center gap-2 min-w-0">
                    <ItemStatusIcon status={item.status} />
                    <span className="text-xs text-gray-700 truncate">{item.item_ref}</span>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <ItemStatusBadge status={item.status} />
                    {item.child_run_id && (
                      <Link
                        to={`/repos/${repoId}/worktrees/${worktreeId}/workflows/runs/${item.child_run_id}`}
                        className="text-[10px] text-indigo-500 hover:text-indigo-700 hover:underline"
                        onClick={(e) => e.stopPropagation()}
                      >
                        view run
                      </Link>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
