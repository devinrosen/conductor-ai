import type { AgentRun } from "../../api/types";
import { formatTokens, statusColors, statusLabels } from "../../utils/agentStats";
import { StatusPulseBadge } from "../shared/StatusPulseBadge";
import { TimeAgo } from "../shared/TimeAgo";

function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remaining = seconds % 60;
  return `${minutes}m ${remaining}s`;
}


/**
 * Extract a clean step label from an orchestrator child prompt.
 */
function extractStepLabel(prompt: string): string | null {
  const match = prompt.match(
    /^You are executing step (\d+) of (\d+) in a multi-step plan\./,
  );
  if (!match) return null;
  const [, stepNum, total] = match;

  const assignmentIdx = prompt.indexOf("## Your Assignment");
  if (assignmentIdx !== -1) {
    const afterHeader = prompt.slice(assignmentIdx);
    const nlIdx = afterHeader.indexOf("\n");
    if (nlIdx !== -1) {
      const desc = afterHeader
        .slice(nlIdx + 1)
        .trim()
        .split("\n")
        .filter((l) => !l.startsWith("Focus only on this step"))
        .join(" ")
        .trim();
      if (desc) {
        const truncated = desc.length > 80 ? desc.slice(0, 80) + "..." : desc;
        return `Step ${stepNum}/${total}: ${truncated}`;
      }
    }
  }

  return `Step ${stepNum}/${total}`;
}

interface ChildRunsListProps {
  children: AgentRun[];
}

export function ChildRunsList({ children }: ChildRunsListProps) {
  if (children.length === 0) return null;

  return (
    <div className="mt-3 border-t border-gray-100 pt-3">
      <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-2">
        Child Runs ({children.length})
      </h4>
      <div className="space-y-2">
        {children.map((child) => {
          const color =
            statusColors[child.status] ?? "bg-gray-100 text-gray-600";
          return (
            <div
              key={child.id}
              className="flex items-center gap-3 rounded-md border border-gray-100 bg-gray-50 px-3 py-2 text-sm"
            >
              <span
                className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${color}`}
              >
                {statusLabels[child.status] ?? child.status}
              </span>
              <StatusPulseBadge status={child.status} />
              <span className="text-gray-700 truncate flex-1" title={child.prompt}>
                {extractStepLabel(child.prompt) ??
                  (child.prompt.length > 80
                    ? child.prompt.slice(0, 80) + "..."
                    : child.prompt)}
              </span>
              <span className="shrink-0 text-xs text-gray-500 tabular-nums space-x-2">
                {(child.input_tokens != null || child.output_tokens != null) && (
                  <span>{formatTokens(child.input_tokens ?? 0, child.output_tokens ?? 0)}</span>
                )}
                {child.num_turns != null && child.num_turns > 0 && (
                  <span>{child.num_turns}t</span>
                )}
                {child.duration_ms != null && child.duration_ms > 0 && (
                  <span>{formatDuration(child.duration_ms)}</span>
                )}
              </span>
              <span className="shrink-0 text-xs text-gray-400">
                <TimeAgo date={child.started_at} />
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
