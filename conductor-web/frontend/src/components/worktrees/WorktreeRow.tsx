import { Link } from "react-router";
import type { Worktree, WorkflowRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";
import { formatWorkflowProgress } from "../../utils/workflowProgress";

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

  const progress = workflowRun ? formatWorkflowProgress(workflowRun) : null;

  // Truncated failure reason from result_summary
  const failReason = isFailed && workflowRun?.result_summary
    ? workflowRun.result_summary.length > 80
      ? workflowRun.result_summary.slice(0, 80) + "\u2026"
      : workflowRun.result_summary
    : null;

  // Find the current active step name (fallback when progress is unavailable)
  const activeStep = workflowRun?.active_steps?.find(
    (s) => s.status === "running" || s.status === "waiting",
  );

  return (
    <tr
      className={selected ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200" : ""}
      data-list-index={index}
    >
      <td className="px-4 py-2">
        <Link
          to={`/repos/${worktree.repo_id}/worktrees/${worktree.id}`}
          className="text-indigo-600 hover:underline"
        >
          {worktree.branch}
        </Link>
      </td>
      <td className="px-4 py-2">
        {ticketSourceId ? (
          <span className="text-xs font-mono text-indigo-500">{ticketSourceId}</span>
        ) : (
          <span className="text-xs text-gray-400">-</span>
        )}
      </td>
      <td className="px-4 py-2">
        <StatusBadge status={worktree.status} />
      </td>
      <td className="px-4 py-2">
        {isRunning ? (
          <span className="inline-flex items-center gap-1.5 text-xs">
            <span className="relative flex h-2 w-2 shrink-0">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-amber-400 opacity-75" />
              <span className="relative inline-flex rounded-full h-2 w-2 bg-amber-500" />
            </span>
            <span className="text-amber-600">
              {workflowRun!.workflow_name}
              {progress
                ? <span className="text-gray-400"> &middot; {progress}</span>
                : activeStep && <span className="text-gray-400"> &middot; {activeStep.step_name}</span>
              }
            </span>
          </span>
        ) : isWaiting ? (
          <span className="inline-flex items-center gap-1.5 text-xs">
            <span className="inline-flex h-2 w-2 rounded-full bg-blue-400 shrink-0" />
            <span className="text-blue-600">
              {workflowRun!.workflow_name}
              {progress
                ? <span className="text-gray-400"> &middot; {progress}</span>
                : activeStep && <span className="text-gray-400"> &middot; {activeStep.step_name}</span>
              }
            </span>
          </span>
        ) : isFailed ? (
          <span className="inline-flex items-center gap-1.5 text-xs text-red-600">
            <svg className="w-3.5 h-3.5 shrink-0" viewBox="0 0 20 20" fill="currentColor">
              <path fillRule="evenodd" d="M18 10a8 8 0 11-16 0 8 8 0 0116 0zm-8-5a.75.75 0 01.75.75v4.5a.75.75 0 01-1.5 0v-4.5A.75.75 0 0110 5zm0 10a1 1 0 100-2 1 1 0 000 2z" clipRule="evenodd" />
            </svg>
            <span>
              {workflowRun!.workflow_name}
              {progress && <span className="text-red-400"> &middot; {progress}</span>}
              {failReason && (
                <span className="block text-[10px] text-red-400 mt-0.5" title={workflowRun!.result_summary ?? undefined}>
                  {failReason}
                </span>
              )}
            </span>
            {onResume && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  e.preventDefault();
                  onResume(workflowRun!.id);
                }}
                className="ml-1 px-1.5 py-0.5 text-[10px] rounded bg-red-100 text-red-700 hover:bg-red-200 active:scale-95 transition-transform"
                title="Resume from failed step"
              >
                Resume
              </button>
            )
          </span>
        ) : null}
      </td>
      <td className="px-4 py-2 text-gray-500">
        <TimeAgo date={worktree.created_at} />
      </td>
      <td className="px-4 py-2">
        <button
          onClick={() => onDelete(worktree.id)}
          className="text-xs text-gray-400 hover:text-red-600"
        >
          <svg className="w-4 h-4" viewBox="0 0 20 20" fill="currentColor">
            <path fillRule="evenodd" d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z" clipRule="evenodd" />
          </svg>
        </button>
      </td>
    </tr>
  );
}
