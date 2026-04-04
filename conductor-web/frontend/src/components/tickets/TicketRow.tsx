import type { Ticket, TicketAgentTotals, WorkflowRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { formatTicketTotalsFull } from "../../utils/agentStats";
import { parseLabels, labelTextColor } from "../../utils/ticketUtils";
import { formatWorkflowProgress } from "../../utils/workflowProgress";

interface TicketRowProps {
  ticket: Ticket;
  agentTotals?: TicketAgentTotals;
  repoSlug?: string;
  onClick: (ticket: Ticket) => void;
  selected?: boolean;
  index?: number;
  labelColorMap?: Record<string, string>;
  depth?: number;
  blocked?: boolean;
  unlocked?: boolean;
  workflowStatus?: "running" | "pending" | "waiting" | "failed" | "completed" | null;
  workflowRun?: WorkflowRun | null;
  onStartWorkflow?: (ticket: Ticket) => void;
  onResumeWorkflow?: (runId: string) => void;
  showPipeline?: boolean;
  hideStateAndLabels?: boolean;
  hasChildren?: boolean;
  collapsed?: boolean;
  onToggleCollapse?: (ticketId: string) => void;
  hasWorktree?: boolean;
}

const PIPELINE_DOT_COLORS: Record<string, string> = {
  ready: "bg-emerald-400",
  dispatched: "bg-blue-400",
  running: "bg-amber-400",
  completed: "bg-green-500",
  failed: "bg-red-500",
};

function PipelineIndicator({ rawJson }: { rawJson: string }) {
  try {
    const parsed = JSON.parse(rawJson);
    const status = parsed?.conductor?.status;
    if (!status) return null;
    const dot = PIPELINE_DOT_COLORS[status] ?? "bg-gray-400";
    return (
      <span className={`inline-block w-2.5 h-2.5 rounded-full ${dot}`} title={status} />
    );
  } catch {
    return null;
  }
}

export function TicketRow({
  ticket,
  agentTotals,
  repoSlug,
  onClick,
  selected,
  index,
  labelColorMap,
  depth = 0,
  blocked = false,
  unlocked = false,
  workflowStatus,
  workflowRun,
  onStartWorkflow,
  onResumeWorkflow,
  showPipeline = false,
  hideStateAndLabels = false,
  hasChildren = false,
  collapsed = false,
  onToggleCollapse,
  hasWorktree = false,
}: TicketRowProps) {
  const labels = parseLabels(ticket.labels);
  const isActive = workflowStatus === "running" || workflowStatus === "pending" || workflowStatus === "waiting";
  const progress = workflowRun ? formatWorkflowProgress(workflowRun) : null;
  const canStart =
    (!blocked || unlocked) &&
    !isActive &&
    !hasWorktree &&
    ticket.source_type === "vantage" &&
    ticket.state === "open" &&
    onStartWorkflow;

  return (
    <tr
      className={[
        blocked && !unlocked ? "opacity-50 cursor-default" : "cursor-pointer hover:bg-gray-50",
        selected ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200" : "",
      ].join(" ")}
      onClick={() => (!blocked || unlocked) && onClick(ticket)}
      data-list-index={index}
    >
      {repoSlug !== undefined && (
        <td className="px-3 py-1.5">
          <span className="inline-block px-1.5 py-0.5 text-[11px] font-mono rounded bg-gray-100 text-gray-600 truncate max-w-[100px]">
            {repoSlug}
          </span>
        </td>
      )}
      <td className="px-3 py-1.5 whitespace-nowrap">
        <span className="inline-flex items-center gap-1">
          {depth > 0 && (
            <span className="text-gray-400 text-[10px]" style={{ marginLeft: `${(depth - 1) * 0.75}rem` }}>&#8627;</span>
          )}
          {hasChildren && onToggleCollapse && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onToggleCollapse(ticket.source_id);
              }}
              className="text-gray-400 hover:text-gray-600 text-[10px] w-3 text-center"
            >
              {collapsed ? "▶" : "▼"}
            </button>
          )}
          {blocked && !unlocked && (
            <span title="Blocked — waiting on parent">
              <svg className="w-3 h-3 text-gray-400 shrink-0" viewBox="0 0 20 20" fill="currentColor">
                <path fillRule="evenodd" d="M10 1a4.5 4.5 0 00-4.5 4.5V9H5a2 2 0 00-2 2v6a2 2 0 002 2h10a2 2 0 002-2v-6a2 2 0 00-2-2h-.5V5.5A4.5 4.5 0 0010 1zm3 8V5.5a3 3 0 10-6 0V9h6z" clipRule="evenodd" />
              </svg>
            </span>
          )}
          {blocked && unlocked && (
            <span title="Unlocked — parent PR approved">
              <svg className="w-3 h-3 text-emerald-500 shrink-0" viewBox="0 0 20 20" fill="currentColor">
                <path d="M10 1a4.5 4.5 0 00-4.5 4.5V9H5a2 2 0 00-2 2v6a2 2 0 002 2h10a2 2 0 002-2v-6a2 2 0 00-2-2h-.5V5.5a3 3 0 016 0v.5a.75.75 0 001.5 0v-.5A4.5 4.5 0 0010 1z" />
              </svg>
            </span>
          )}
          <span className={depth > 0 ? "text-indigo-400" : "text-indigo-600"}>{ticket.source_id}</span>
        </span>
      </td>
      <td className="px-3 py-1.5 text-gray-900">{ticket.title}</td>
      {!hideStateAndLabels && (
        <td className="px-3 py-1.5">
          <StatusBadge status={ticket.state} />
        </td>
      )}
      {!hideStateAndLabels && (
        <td className="px-3 py-1.5">
          <div className="flex flex-wrap gap-1">
            {labels.map((l) => {
              const bg = labelColorMap?.[l];
              return bg ? (
                <span
                  key={l}
                  className="px-1.5 py-0.5 text-xs rounded"
                  style={{ backgroundColor: bg, color: labelTextColor(bg) }}
                >
                  {l}
                </span>
              ) : (
                <span
                  key={l}
                  className="px-1.5 py-0.5 text-xs rounded bg-gray-100 text-gray-600"
                >
                  {l}
                </span>
              );
            })}
          </div>
        </td>
      )}
      <td className="px-3 py-1.5 text-xs text-gray-500">
        {ticket.assignee ?? "-"}
      </td>
      {showPipeline && (
        <td className="px-3 py-1.5 text-center">
          <PipelineIndicator rawJson={ticket.raw_json} />
        </td>
      )}
      <td className="px-3 py-1.5 text-xs whitespace-nowrap">
        {workflowStatus === "failed" ? (
          <span className="inline-flex items-center gap-1.5 text-red-600">
            <svg className="w-3.5 h-3.5 shrink-0" viewBox="0 0 20 20" fill="currentColor">
              <path fillRule="evenodd" d="M18 10a8 8 0 11-16 0 8 8 0 0116 0zm-8-5a.75.75 0 01.75.75v4.5a.75.75 0 01-1.5 0v-4.5A.75.75 0 0110 5zm0 10a1 1 0 100-2 1 1 0 000 2z" clipRule="evenodd" />
            </svg>
            <span>
              Failed{progress && <span className="text-red-400"> &middot; {progress}</span>}
              {workflowRun?.result_summary && (
                <span className="block text-[10px] text-red-400 mt-0.5 whitespace-normal max-w-[200px]" title={workflowRun.result_summary}>
                  {workflowRun.result_summary.length > 60 ? workflowRun.result_summary.slice(0, 60) + "\u2026" : workflowRun.result_summary}
                </span>
              )}
            </span>
            {onResumeWorkflow && workflowRun && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onResumeWorkflow(workflowRun.id);
                }}
                className="ml-1 px-1.5 py-0.5 text-[10px] rounded bg-red-100 text-red-700 hover:bg-red-200 active:scale-95 transition-transform shrink-0"
                title="Resume from failed step"
              >
                Resume
              </button>
            )}
          </span>
        ) : isActive ? (
          <span className="inline-flex items-center gap-1.5 text-amber-600">
            <span className="relative flex h-2 w-2">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-amber-400 opacity-75" />
              <span className="relative inline-flex rounded-full h-2 w-2 bg-amber-500" />
            </span>
            <span>
              Running{progress && <span className="text-amber-400"> &middot; {progress}</span>}
            </span>
          </span>
        ) : canStart ? (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onStartWorkflow!(ticket);
            }}
            className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded bg-indigo-600 text-white hover:bg-indigo-700 active:scale-95 transition-transform"
          >
            <svg className="w-3 h-3" viewBox="0 0 20 20" fill="currentColor">
              <path d="M6.3 2.84A1.5 1.5 0 004 4.11v11.78a1.5 1.5 0 002.3 1.27l9.344-5.891a1.5 1.5 0 000-2.538L6.3 2.841z" />
            </svg>
            Start
          </button>
        ) : agentTotals ? (
          <span className="text-purple-600">{formatTicketTotalsFull(agentTotals)}</span>
        ) : null}
      </td>
    </tr>
  );
}
