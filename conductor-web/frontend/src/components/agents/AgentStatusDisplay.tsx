import type { AgentRun } from "../../api/types";
import { formatTokens, isActiveRun, statusColors, statusLabels } from "../../utils/agentStats";
import { StatusPulseBadge } from "../shared/StatusPulseBadge";
import { TimeAgo } from "../shared/TimeAgo";
import { ChildRunsList } from "./ChildRunsList";

function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remaining = seconds % 60;
  return `${minutes}m ${remaining}s`;
}

interface AgentStatusDisplayProps {
  run: AgentRun;
  runs: AgentRun[];
  childRuns?: AgentRun[];
}

export function AgentStatusDisplay({
  run,
  runs,
  childRuns,
}: AgentStatusDisplayProps) {
  const color = statusColors[run.status] ?? "bg-gray-100 text-gray-600";
  const hasChildren = childRuns && childRuns.length > 0;

  // Aggregate totals across all completed runs (top-level only)
  const completedRuns = runs.filter((r) => r.status === "completed");
  const totalInputTokens = completedRuns.reduce((s, r) => s + (r.input_tokens ?? 0), 0);
  const totalOutputTokens = completedRuns.reduce((s, r) => s + (r.output_tokens ?? 0), 0);
  const totalTurns = completedRuns.reduce((s, r) => s + (r.num_turns ?? 0), 0);
  const totalDurationMs = completedRuns.reduce(
    (s, r) => s + (r.duration_ms ?? 0),
    0,
  );

  // Include in-progress run's stats in the total
  const isActive = isActiveRun(run);
  const displayInputTokens = isActive
    ? totalInputTokens + (run.input_tokens ?? 0)
    : totalInputTokens;
  const displayOutputTokens = isActive
    ? totalOutputTokens + (run.output_tokens ?? 0)
    : totalOutputTokens;
  const displayTurns = isActive
    ? totalTurns + (run.num_turns ?? 0)
    : totalTurns;
  const displayDuration = isActive
    ? totalDurationMs + (run.duration_ms ?? 0)
    : totalDurationMs;

  // If the latest run has child runs, compute aggregated (parent + children) totals
  const childInputTokens = hasChildren
    ? childRuns.reduce((s, r) => s + (r.input_tokens ?? 0), 0)
    : 0;
  const childOutputTokens = hasChildren
    ? childRuns.reduce((s, r) => s + (r.output_tokens ?? 0), 0)
    : 0;
  const childTurns = hasChildren
    ? childRuns.reduce((s, r) => s + (r.num_turns ?? 0), 0)
    : 0;
  const childDurationMs = hasChildren
    ? childRuns.reduce((s, r) => s + (r.duration_ms ?? 0), 0)
    : 0;

  const treeInputTokens = displayInputTokens + childInputTokens;
  const treeOutputTokens = displayOutputTokens + childOutputTokens;
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
            {statusLabels[run.status] ?? run.status}
          </span>
          <StatusPulseBadge status={run.status} />
          {hasChildren && (
            <span className="inline-block px-2 py-0.5 text-xs font-medium rounded-full bg-indigo-100 text-indigo-700">
              {childRuns.length} child{childRuns.length !== 1 ? "ren" : ""}
            </span>
          )}
        </div>
      </div>

      <dl className="grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-2 text-sm">
        {(treeInputTokens > 0 || treeOutputTokens > 0) && (
          <div>
            <dt className="text-gray-500">
              Tokens{hasChildren ? " (tree)" : ""}
            </dt>
            <dd className="font-medium text-gray-900">
              {formatTokens(treeInputTokens, treeOutputTokens)}
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
          <div>
            <dt className="text-gray-500">Session</dt>
            <dd
              className="font-mono text-xs text-gray-700 cursor-help"
              title={run.claude_session_id}
            >
              {run.claude_session_id.slice(0, 8)}&hellip;
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
