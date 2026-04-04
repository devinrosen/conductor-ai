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

/** Elapsed timer — updates every 30s for active runs, static for completed/failed. */
function LiveTimer({ startedAt, endedAt, estimatedMs }: { startedAt: string; endedAt?: string | null; estimatedMs?: number | null }) {
  const [, setTick] = useState(0);
  const isLive = !endedAt;
  useEffect(() => {
    if (!isLive) return;
    const id = setInterval(() => setTick((t) => t + 1), 30_000);
    return () => clearInterval(id);
  }, [isLive]);

  const elapsed = (endedAt ? new Date(endedAt).getTime() : Date.now()) - new Date(startedAt).getTime();
  // Round to nearest minute for cleaner display
  const elapsedStr = elapsed < 60_000 ? "<1m" : formatDuration(Math.floor(elapsed / 60_000) * 60_000);

  return (
    <div className="text-[11px] font-mono tabular-nums">
      <span className="text-gray-400">{elapsedStr}</span>
      {estimatedMs && estimatedMs > 0 ? (
        <span className="text-gray-600 block">est. {formatDuration(estimatedMs)}</span>
      ) : null}
    </div>
  );
}

export function WorktreeRow({
  worktree,
  workflowRun,
  onDelete,
  onResume,
  selected,
  index,
  ticketSourceId,
  isMerged = false,
}: {
  worktree: Worktree;
  workflowRun?: WorkflowRun | null;
  onDelete: (id: string) => void;
  onResume?: (runId: string) => void;
  selected?: boolean;
  index?: number;
  ticketSourceId?: string | null;
  isMerged?: boolean;
}) {
  const isRunning = workflowRun?.status === "running" || workflowRun?.status === "pending";
  const isWaiting = workflowRun?.status === "waiting";
  const isFailed = workflowRun?.status === "failed";
  const isCompleted = workflowRun?.status === "completed";
  const isActive = isRunning || isWaiting;
  const hasWorkflow = isActive || isFailed || isCompleted;

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

  const nameColor = "text-gray-200";

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
        {isMerged ? (
          <span className="inline-flex items-center gap-1.5 text-xs text-purple-400">
            <svg className="w-3.5 h-3.5 shrink-0" viewBox="0 0 20 20" fill="currentColor">
              <path fillRule="evenodd" d="M15.98 1.804a1 1 0 00-1.96 0l-.24 1.192a1 1 0 01-.784.785l-1.192.238a1 1 0 000 1.962l1.192.238a1 1 0 01.785.785l.238 1.192a1 1 0 001.962 0l.238-1.192a1 1 0 01.785-.785l1.192-.238a1 1 0 000-1.962l-1.192-.238a1 1 0 01-.785-.785l-.238-1.192zM6.949 5.684a1 1 0 00-1.898 0l-.683 2.051a1 1 0 01-.633.633l-2.051.683a1 1 0 000 1.898l2.051.684a1 1 0 01.633.632l.683 2.051a1 1 0 001.898 0l.683-2.051a1 1 0 01.633-.633l2.051-.683a1 1 0 000-1.898l-2.051-.683a1 1 0 01-.633-.633L6.95 5.684zM13.949 13.684a1 1 0 00-1.898 0l-.184.551a1 1 0 01-.632.633l-.551.183a1 1 0 000 1.898l.551.183a1 1 0 01.633.633l.183.551a1 1 0 001.898 0l.184-.551a1 1 0 01.632-.633l.551-.183a1 1 0 000-1.898l-.551-.184a1 1 0 01-.633-.632l-.183-.551z" clipRule="evenodd" />
            </svg>
            Merged
          </span>
        ) : isCompleted ? (
          <span className="inline-flex items-center gap-1.5 text-xs text-green-500">
            <svg className="w-3.5 h-3.5 shrink-0" viewBox="0 0 20 20" fill="currentColor">
              <path fillRule="evenodd" d="M10 18a8 8 0 100-16 8 8 0 000 16zm3.857-9.809a.75.75 0 00-1.214-.882l-3.483 4.79-1.88-1.88a.75.75 0 10-1.06 1.061l2.5 2.5a.75.75 0 001.137-.089l4-5.5z" clipRule="evenodd" />
            </svg>
            PR Approved
          </span>
        ) : hasWorkflow ? (
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
            </div>
          </div>
        ) : null}
      </td>
      {/* Duration */}
      <td className="px-3 py-2 align-top">
        {hasWorkflow && workflowRun!.started_at && (
          <LiveTimer
            startedAt={workflowRun!.started_at}
            endedAt={workflowRun!.ended_at}
            estimatedMs={workflowRun!.estimated_duration_ms}
          />
        )}
      </td>
      {/* Resume */}
      <td className="pl-2 py-2 w-6">
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
      </td>
      {/* Delete */}
      <td className="pl-1 pr-4 py-2 w-6">
        <Tooltip content={isMerged ? "Clean up merged worktree" : "Delete worktree"}>
          <button
            onClick={() => onDelete(worktree.id)}
            className={isMerged ? "text-green-500 hover:text-green-400" : "text-gray-400 hover:text-red-600"}
            aria-label={isMerged ? "Clean up merged worktree" : "Delete worktree"}
          >
            <svg className="w-4 h-4" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
              <path fillRule="evenodd" d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z" clipRule="evenodd" />
            </svg>
          </button>
        </Tooltip>
      </td>
    </tr>
  );
}
