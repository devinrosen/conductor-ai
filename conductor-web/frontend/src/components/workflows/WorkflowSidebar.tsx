import { useState, useEffect, useCallback } from "react";
import { Link } from "react-router";
import { api } from "../../api/client";
import type { WorkflowDefSummary, WorkflowRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";
import { RunWorkflowModal } from "./RunWorkflowModal";
import { getErrorMessage } from "../../utils/errorHandling";

interface WorkflowSidebarProps {
  repoId: string;
  worktreeId: string;
  ticketId?: string;
}

export function WorkflowSidebar({ repoId, worktreeId, ticketId }: WorkflowSidebarProps) {
  const [defs, setDefs] = useState<WorkflowDefSummary[]>([]);
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [runModalDef, setRunModalDef] = useState<WorkflowDefSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const [defsData, runsData] = await Promise.all([
        api.listWorkflowDefs(worktreeId),
        api.listWorkflowRuns(worktreeId),
      ]);
      setDefs(defsData);
      setRuns(runsData);
      setError(null);
    } catch (e) {
      const message = getErrorMessage(e, "Failed to load workflow data");
      setError(message);
      console.error("Failed to fetch workflow data:", e);
    } finally {
      setLoading(false);
    }
  }, [worktreeId]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  // Only poll when there are active workflow runs
  useEffect(() => {
    const hasActiveRuns = runs.some((r) => r.status === "running" || r.status === "waiting");
    if (!hasActiveRuns) return;

    const interval = setInterval(fetchData, 5000);
    return () => clearInterval(interval);
  }, [runs, fetchData]);

  const handleCancel = async (runId: string) => {
    try {
      await api.cancelWorkflow(runId);
      fetchData();
    } catch (e) {
      const message = getErrorMessage(e, "Failed to cancel workflow");
      setError(message);
      console.error("Failed to cancel workflow:", e);
    }
  };

  if (loading) {
    return <div className="p-3 text-gray-500 text-sm">Loading...</div>;
  }

  if (error) {
    return (
      <div className="p-3 text-red-500 text-sm">
        <p>{error}</p>
        <button
          onClick={() => { setError(null); fetchData(); }}
          className="mt-1 text-xs text-red-400 hover:text-red-300 underline"
        >
          Retry
        </button>
      </div>
    );
  }

  const runningCount = runs.filter((r) => r.status === "running" || r.status === "waiting").length;

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* Available workflows — compact list */}
      <div className="px-3 pt-3 pb-2">
        <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-500 mb-2">
          Workflows
          {defs.length > 0 && <span className="ml-1 text-gray-600">({defs.length})</span>}
        </h4>
        {defs.length === 0 ? (
          <p className="text-xs text-gray-600">No workflows found</p>
        ) : (
          <div className="space-y-1">
            {defs.map((def) => (
              <div key={def.name} className="flex items-center justify-between gap-2 py-1">
                <span className="text-sm text-gray-300 truncate" title={def.description || def.name}>
                  {def.name}
                </span>
                <button
                  onClick={() => setRunModalDef(def)}
                  className="shrink-0 px-2 py-0.5 text-xs bg-cyan-700 hover:bg-cyan-600 text-white rounded active:scale-95 transition-transform"
                >
                  Run
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="mx-3 border-t border-gray-700 my-2" />

      {/* Recent runs */}
      <div className="px-3 pb-3 flex-1 overflow-y-auto min-h-0">
        <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-500 mb-2">
          Recent Runs
          {runningCount > 0 && (
            <span className="ml-1.5 inline-flex items-center px-1.5 py-0.5 text-[10px] font-bold bg-cyan-800 text-cyan-200 rounded-full">
              {runningCount} active
            </span>
          )}
        </h4>
        {runs.length === 0 ? (
          <p className="text-xs text-gray-600">No runs yet</p>
        ) : (
          <div className="space-y-1.5">
            {runs.slice(0, 15).map((run) => (
              <div key={run.id} className="rounded border border-gray-700 bg-gray-800 px-2.5 py-1.5">
                <div className="flex items-center justify-between gap-1.5">
                  <Link
                    to={`/repos/${repoId}/worktrees/${worktreeId}/workflows/runs/${run.id}`}
                    className="text-sm text-gray-300 hover:text-gray-100 truncate"
                    title={run.workflow_name}
                  >
                    {run.workflow_name}
                  </Link>
                  <StatusBadge status={run.status} />
                </div>
                <div className="flex items-center justify-between mt-0.5">
                  <span className="text-[10px] text-gray-600">
                    <TimeAgo date={run.started_at} />
                  </span>
                  {(run.status === "running" || run.status === "waiting") && (
                    <button
                      onClick={() => handleCancel(run.id)}
                      className="text-[10px] text-red-500 hover:text-red-400"
                    >
                      Cancel
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Run modal */}
      {runModalDef && (
        <RunWorkflowModal
          def={runModalDef}
          worktreeId={worktreeId}
          ticketId={ticketId}
          onClose={() => setRunModalDef(null)}
          onStarted={() => {
            setRunModalDef(null);
            fetchData();
          }}
        />
      )}
    </div>
  );
}
