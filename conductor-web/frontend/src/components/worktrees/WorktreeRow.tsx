import { useState, useEffect } from "react";
import { Link } from "react-router";
import type { Worktree, WorkflowRun } from "../../api/types";
import { TimeAgo } from "../shared/TimeAgo";
import { Tooltip } from "../shared/Tooltip";
import { formatDuration } from "../../utils/agentStats";
import { formatIteration } from "../../utils/workflowProgress";

/** Small segmented progress bar showing completed/current/remaining steps. */
function StepBar({ current, total, failed }: { current: number; total: number; failed?: boolean }) {
  const segments = [];
  for (let i = 1; i <= total; i++) {
    let color: string;
    if (i < current) color = "bg-green-500";
    else if (i === current) color = failed ? "bg-red-500" : "bg-amber-400";
    else color = "bg-gray-600";
    segments.push(
      <span key={i} className={`h-1 flex-1 rounded-full ${color}`} />,
    );
  }
  return (
    <Tooltip content={`Step ${current}/${total}`}>
      <span className="flex gap-px w-20">{segments}</span>
    </Tooltip>
  );
}

/** Live elapsed timer that re-renders every second. */
function LiveTimer({ startedAt, estimatedMs }: { startedAt: string; estimatedMs?: number | null }) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, []);

  const elapsed = Date.now() - new Date(startedAt).getTime();
  const elapsedStr = formatDuration(elapsed);

  if (estimatedMs && estimatedMs > 0) {
    const remaining = Math.max(0, estimatedMs - elapsed);
    return (
      <span className="text-[10px] text-gray-500 font-mono tabular-nums">
        {elapsedStr} / ~{formatDuration(estimatedMs)}
        {remaining > 0 && <span className="text-gray-600"> ({formatDuration(remaining)} left)</span>}
      </span>
    );
  }

  return <span className="text-[10px] text-gray-500 font-mono tabular-nums">{elapsedStr}</span>;
}

export function WorktreeRow({
  worktree,
  workflowRun,
  onDelete,
  onResume,
  selected,
  index,
  ticketSourceId,
}: {
  worktree: Worktree;
  workflowRun?: WorkflowRun | null;
  onDelete: (id: string) => void;
  onResume?: (runId: string) => void;
  selected?: boolean;
  index?: number;
  ticketSourceId?: string | null;
}) {
  const isRunning = workflowRun?.status === "running" || workflowRun?.status === "pending";
  const isWaiting = workflowRun?.status === "waiting";
  const isFailed = workflowRun?.status === "failed";
  const isActive = isRunning || isWaiting;
  const hasWorkflow = isActive || isFailed;

  // Get the deepest active substep name from active_steps
  const activeStep = workflowRun?.active_steps?.find(
    (s) => s.status === "running" || s.status === "waiting",
  );
  const substepName = activeStep?.step_name;
  // For display: strip "workflow:" prefix, show leaf step name
  const displaySubstep = substepName && !substepName.startsWith("workflow:")
    ? substepName
    : workflowRun?.current_step_name && !workflowRun.current_step_name.startsWith("workflow:")
      ? workflowRun.current_step_name
      : null;

  const iter = workflowRun ? formatIteration(workflowRun) : null;

  // Status indicator element
  const statusDot = isRunning ? (
    <span className="relative flex h-2 w-2 shrink-0">
      <span className="motion-safe:animate-ping absolute inline-flex h-full w-full rounded-full bg-amber-400 opacity-75" />
      <span className="relative inline-flex rounded-full h-2 w-2 bg-amber-500" />
    </span>
  ) : isWaiting ? (
    <span className="inline-flex h-2 w-2 rounded-full bg-blue-400 shrink-0" />
  ) : isFailed ? (
    <svg className="w-3.5 h-3.5 shrink-0 text-red-500" viewBox="0 0 20 20" fill="currentColor">
      <path fillRule="evenodd" d="M18 10a8 8 0 11-16 0 8 8 0 0116 0zm-8-5a.75.75 0 01.75.75v4.5a.75.75 0 01-1.5 0v-4.5A.75.75 0 0110 5zm0 10a1 1 0 100-2 1 1 0 000 2z" clipRule="evenodd" />
    </svg>
  ) : null;

  const nameColor = "text-gray-300";

  return (
    <tr
      className={selected ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200" : ""}
      data-list-index={index}
    >
      {/* Branch + Created */}
      <td className="px-4 py-2">
        <Link
          to={`/repos/${worktree.repo_id}/worktrees/${worktree.id}`}
          className="text-indigo-600 hover:underline block"
        >
          {worktree.branch}
        </Link>
        <span className="text-[11px] text-gray-500">
          created <TimeAgo date={worktree.created_at} short /> ago
        </span>
      </td>
      {/* Ticket */}
      <td className="px-4 py-2">
        {ticketSourceId ? (
          <span className="text-xs font-mono text-indigo-500">{ticketSourceId}</span>
        ) : (
          <span className="text-xs text-gray-400">-</span>
        )}
      </td>
      {/* Workflow */}
      <td className="px-4 py-2">
        {hasWorkflow ? (
          <div className="flex items-start gap-1.5">
            <span className="mt-0.5">{statusDot}</span>
            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <span className={`text-xs ${nameColor}`}>{workflowRun!.workflow_name}</span>
                {iter && <span className="text-[10px] text-gray-500">{iter}</span>}
              </div>
              {workflowRun!.current_step != null && workflowRun!.total_steps != null && (
                <StepBar
                  current={workflowRun!.current_step}
                  total={workflowRun!.total_steps}
                  failed={isFailed}
                />
              )}
              {displaySubstep && (
                <span className="text-[10px] text-gray-500 block truncate max-w-[180px]">{displaySubstep}</span>
              )}
              {isActive && workflowRun!.started_at && (
                <LiveTimer
                  startedAt={workflowRun!.started_at}
                  estimatedMs={workflowRun!.estimated_duration_ms}
                />
              )}
            </div>
          </div>
        ) : null}
      </td>
      {/* Actions */}
      <td className="px-4 py-2 align-top">
        <div className="flex flex-col items-center gap-1">
          {isFailed && onResume && (
            <Tooltip content="Resume from failed step">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  e.preventDefault();
                  onResume(workflowRun!.id);
                }}
                className="text-amber-600 hover:text-amber-800"
                aria-label="Resume from failed step"
              >
                <svg className="w-4 h-4" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                  <path fillRule="evenodd" d="M15.312 11.424a5.5 5.5 0 01-9.201 2.466l-.312-.311h2.433a.75.75 0 000-1.5H4.598a.75.75 0 00-.75.75v3.634a.75.75 0 001.5 0v-2.033l.312.311a7 7 0 0011.712-3.138.75.75 0 00-1.449-.39zm-10.624-2.85a5.5 5.5 0 019.201-2.465l.312.31H11.77a.75.75 0 000 1.5h3.634a.75.75 0 00.75-.75V3.534a.75.75 0 00-1.5 0v2.033l-.311-.311A7 7 0 002.63 8.394a.75.75 0 001.449.39z" clipRule="evenodd" />
                </svg>
              </button>
            </Tooltip>
          )}
          <Tooltip content="Delete worktree">
            <button
              onClick={() => onDelete(worktree.id)}
              className="text-gray-400 hover:text-red-600"
              aria-label="Delete worktree"
            >
              <svg className="w-4 h-4" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                <path fillRule="evenodd" d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z" clipRule="evenodd" />
              </svg>
            </button>
          </Tooltip>
        </div>
      </td>
    </tr>
  );
}
