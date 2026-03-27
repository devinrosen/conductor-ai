import type { AgentRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";

interface RepoAgentRunCardProps {
  run: AgentRun;
}

export function RepoAgentRunCard({ run }: RepoAgentRunCardProps) {
  return (
    <div className="rounded-lg border border-gray-200 bg-white p-3">
      <p className="text-sm text-gray-900 line-clamp-3">{run.prompt}</p>
      <div className="mt-2 flex items-center gap-3">
        <StatusBadge status={run.status} />
        <span className="text-xs text-gray-500">
          {run.cost_usd != null ? `$${run.cost_usd.toFixed(2)}` : "-"}
        </span>
        <span className="text-xs text-gray-500">
          {new Date(run.started_at).toLocaleString()}
        </span>
      </div>
    </div>
  );
}
