import type { AgentRun } from "../../api/types";
import { TimeAgo } from "../shared/TimeAgo";
import { ChildRunsList } from "./ChildRunsList";

const statusColors: Record<string, string> = {
  running: "bg-yellow-100 text-yellow-700",
  completed: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
  cancelled: "bg-gray-100 text-gray-600",
};

function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remaining = seconds % 60;
  return `${minutes}m ${remaining}s`;
}

function formatCost(usd: number): string {
  return `$${usd.toFixed(4)}`;
}

interface AgentStatusDisplayProps {
  run: AgentRun;
  runs: AgentRun[];
  childRuns?: AgentRun[];
  onLaunch: () => void;
  onOrchestrate?: () => void;
  onStop: () => void;
}

export function AgentStatusDisplay({
  run,
  runs,
  childRuns,
  onLaunch,
  onOrchestrate,
  onStop,
}: AgentStatusDisplayProps) {
  const color = statusColors[run.status] ?? "bg-gray-100 text-gray-600";
  const hasChildren = childRuns && childRuns.length > 0;

  // Aggregate totals across all completed runs (top-level only)
  const completedRuns = runs.filter((r) => r.status === "completed");
  const totalCost = completedRuns.reduce((s, r) => s + (r.cost_usd ?? 0), 0);
  const totalTurns = completedRuns.reduce((s, r) => s + (r.num_turns ?? 0), 0);
  const totalDurationMs = completedRuns.reduce(
    (s, r) => s + (r.duration_ms ?? 0),
    0,
  );

  // Include in-progress run's turns in the total
  const displayCost =
    run.status === "running" ? totalCost + (run.cost_usd ?? 0) : totalCost;
  const displayTurns =
    run.status === "running" ? totalTurns + (run.num_turns ?? 0) : totalTurns;
  const displayDuration =
    run.status === "running"
      ? totalDurationMs + (run.duration_ms ?? 0)
      : totalDurationMs;

  // If the latest run has child runs, compute aggregated (parent + children) totals
  const childCost = hasChildren
    ? childRuns.reduce((s, r) => s + (r.cost_usd ?? 0), 0)
    : 0;
  const childTurns = hasChildren
    ? childRuns.reduce((s, r) => s + (r.num_turns ?? 0), 0)
    : 0;
  const childDurationMs = hasChildren
    ? childRuns.reduce((s, r) => s + (r.duration_ms ?? 0), 0)
    : 0;

  const treeCost = displayCost + childCost;
  const treeTurns = displayTurns + childTurns;
  const treeDuration = displayDuration + childDurationMs;

  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4">
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Agent
          </h3>
          <span
            className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${color}`}
          >
            {run.status}
          </span>
          {run.status === "running" && (
            <span className="inline-block w-2 h-2 rounded-full bg-yellow-400 animate-pulse" />
          )}
          {hasChildren && (
            <span className="inline-block px-2 py-0.5 text-xs font-medium rounded-full bg-indigo-100 text-indigo-700">
              {childRuns.length} child{childRuns.length !== 1 ? "ren" : ""}
            </span>
          )}
        </div>
        <div className="flex gap-2">
          {run.status === "running" ? (
            <button
              onClick={onStop}
              className="px-3 py-1.5 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
            >
              Stop Agent
            </button>
          ) : (
            <>
              <button
                onClick={onLaunch}
                className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700"
              >
                {run.claude_session_id ? "Launch / Resume" : "Launch Agent"}
              </button>
              {onOrchestrate && (
                <button
                  onClick={onOrchestrate}
                  className="px-3 py-1.5 text-sm rounded-md border border-indigo-300 text-indigo-700 hover:bg-indigo-50"
                >
                  Orchestrate
                </button>
              )}
            </>
          )}
        </div>
      </div>

      <dl className="grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-2 text-sm">
        {treeCost > 0 && (
          <div>
            <dt className="text-gray-500">
              Cost{hasChildren ? " (tree)" : ""}
            </dt>
            <dd className="font-medium text-gray-900">
              {formatCost(treeCost)}
            </dd>
          </div>
        )}
        {treeTurns > 0 && (
          <div>
            <dt className="text-gray-500">
              Turns{hasChildren ? " (tree)" : ""}
            </dt>
            <dd className="font-medium text-gray-900">{treeTurns}</dd>
          </div>
        )}
        {treeDuration > 0 && (
          <div>
            <dt className="text-gray-500">
              Duration{hasChildren ? " (tree)" : ""}
            </dt>
            <dd className="font-medium text-gray-900">
              {formatDuration(treeDuration)}
            </dd>
          </div>
        )}
        <div>
          <dt className="text-gray-500">Runs</dt>
          <dd className="font-medium text-gray-900">{runs.length}</dd>
        </div>
        <div>
          <dt className="text-gray-500">Started</dt>
          <dd className="font-medium text-gray-900">
            <TimeAgo date={run.started_at} />
          </dd>
        </div>
        {run.ended_at && (
          <div>
            <dt className="text-gray-500">Ended</dt>
            <dd className="font-medium text-gray-900">
              <TimeAgo date={run.ended_at} />
            </dd>
          </div>
        )}
        {run.claude_session_id && (
          <div className="col-span-2">
            <dt className="text-gray-500">Session</dt>
            <dd className="font-mono text-xs text-gray-700 truncate">
              {run.claude_session_id}
            </dd>
          </div>
        )}
      </dl>

      {run.status === "failed" && run.result_text && (
        <div className="mt-3 rounded-md bg-red-50 p-3 text-sm text-red-700">
          {run.result_text}
        </div>
      )}

      {hasChildren && <ChildRunsList children={childRuns} />}
    </div>
  );
}
