import type { AgentRun } from "../../api/types";

interface RepoAgentRunCardProps {
  run: AgentRun;
}

export function RepoAgentRunCard({ run }: RepoAgentRunCardProps) {
  return (
    <div className="rounded-lg border border-gray-200 bg-white p-3">
      <p className="text-sm text-gray-900 line-clamp-3">{run.prompt}</p>
      <div className="mt-2 flex items-center gap-3">
        <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
          run.status === "completed" ? "bg-green-100 text-green-800" :
          run.status === "failed" ? "bg-red-100 text-red-800" :
          run.status === "cancelled" ? "bg-gray-100 text-gray-800" :
          "bg-yellow-100 text-yellow-800"
        }`}>
          {run.status}
        </span>
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
