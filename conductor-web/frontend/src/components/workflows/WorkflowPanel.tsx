import { useState, useEffect, useCallback } from "react";
import { Link } from "react-router";
import { api } from "../../api/client";
import type { WorkflowDefSummary, WorkflowRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";

interface WorkflowPanelProps {
  repoId: string;
  worktreeId: string;
}

export function WorkflowPanel({ repoId, worktreeId }: WorkflowPanelProps) {
  const [defs, setDefs] = useState<WorkflowDefSummary[]>([]);
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [runModalDef, setRunModalDef] = useState<WorkflowDefSummary | null>(null);
  const [loading, setLoading] = useState(true);

  const fetchData = useCallback(async () => {
    try {
      const [defsData, runsData] = await Promise.all([
        api.listWorkflowDefs(worktreeId),
        api.listWorkflowRuns(worktreeId),
      ]);
      setDefs(defsData);
      setRuns(runsData);
    } catch {
      // silently fail
    } finally {
      setLoading(false);
    }
  }, [worktreeId]);

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 5000);
    return () => clearInterval(interval);
  }, [fetchData]);

  const handleCancel = async (runId: string) => {
    try {
      await api.cancelWorkflow(runId);
      fetchData();
    } catch {
      // ignore
    }
  };

  if (loading) {
    return <div className="p-4 text-gray-400">Loading workflows...</div>;
  }

  return (
    <div className="space-y-6">
      {/* Available Workflows */}
      <div>
        <h3 className="text-lg font-semibold text-gray-200 mb-3">Available Workflows</h3>
        {defs.length === 0 ? (
          <p className="text-gray-500 text-sm">
            No workflow definitions found. Add .wf files to .conductor/workflows/
          </p>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            {defs.map((def) => (
              <div
                key={def.name}
                className="bg-gray-800 rounded-lg p-4 border border-gray-700"
              >
                <div className="flex items-center justify-between mb-2">
                  <h4 className="font-medium text-gray-200">{def.name}</h4>
                  <span className="text-xs px-2 py-0.5 bg-gray-700 rounded text-gray-400">
                    {def.trigger}
                  </span>
                </div>
                <p className="text-sm text-gray-400 mb-3">
                  {def.description || "No description"}
                </p>
                <div className="flex items-center justify-between">
                  <span className="text-xs text-gray-500">
                    {def.node_count} step{def.node_count !== 1 ? "s" : ""}
                    {def.inputs.length > 0 && ` · ${def.inputs.length} input${def.inputs.length !== 1 ? "s" : ""}`}
                  </span>
                  <button
                    onClick={() => setRunModalDef(def)}
                    className="px-3 py-1 text-sm bg-cyan-600 hover:bg-cyan-500 text-white rounded"
                  >
                    Run
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Run Modal */}
      {runModalDef && (
        <RunWorkflowModal
          def={runModalDef}
          worktreeId={worktreeId}
          onClose={() => setRunModalDef(null)}
          onStarted={() => {
            setRunModalDef(null);
            fetchData();
          }}
        />
      )}

      {/* Recent Runs */}
      <div>
        <h3 className="text-lg font-semibold text-gray-200 mb-3">Recent Runs</h3>
        {runs.length === 0 ? (
          <p className="text-gray-500 text-sm">No workflow runs yet.</p>
        ) : (
          <div className="space-y-2">
            {runs.map((run) => (
              <div key={run.id} className="bg-gray-800 rounded-lg border border-gray-700">
                <div className="flex items-center justify-between p-3">
                  <Link
                    to={`/repos/${repoId}/worktrees/${worktreeId}/workflows/runs/${run.id}`}
                    className="flex items-center gap-3 hover:opacity-80 min-w-0"
                  >
                    <span className="font-medium text-gray-200 truncate">{run.workflow_name}</span>
                    <StatusBadge status={run.status} />
                    {run.dry_run && (
                      <span className="text-xs px-1.5 py-0.5 bg-yellow-900 text-yellow-300 rounded shrink-0">
                        dry-run
                      </span>
                    )}
                  </Link>
                  <div className="flex items-center gap-3 shrink-0 ml-3">
                    <TimeAgo date={run.started_at} />
                    {(run.status === "running" || run.status === "waiting") && (
                      <button
                        onClick={() => handleCancel(run.id)}
                        className="px-2 py-0.5 text-xs bg-red-700 hover:bg-red-600 text-white rounded"
                      >
                        Cancel
                      </button>
                    )}
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function RunWorkflowModal({
  def,
  worktreeId,
  onClose,
  onStarted,
}: {
  def: WorkflowDefSummary;
  worktreeId: string;
  onClose: () => void;
  onStarted: () => void;
}) {
  const [model, setModel] = useState("");
  const [dryRun, setDryRun] = useState(false);
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async () => {
    setSubmitting(true);
    setError(null);
    try {
      await api.runWorkflow(worktreeId, {
        name: def.name,
        model: model || undefined,
        dry_run: dryRun || undefined,
        inputs: Object.keys(inputs).length > 0 ? inputs : undefined,
      });
      onStarted();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Failed to start workflow");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
      <div className="bg-gray-800 rounded-lg p-6 w-full max-w-md border border-gray-600">
        <h3 className="text-lg font-semibold text-gray-200 mb-4">
          Run: {def.name}
        </h3>

        <div className="space-y-4">
          <div>
            <label className="block text-sm text-gray-400 mb-1">Model override</label>
            <input
              type="text"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="Default model"
              className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded text-gray-200 text-sm"
            />
          </div>

          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="dry-run"
              checked={dryRun}
              onChange={(e) => setDryRun(e.target.checked)}
              className="rounded"
            />
            <label htmlFor="dry-run" className="text-sm text-gray-400">
              Dry run
            </label>
          </div>

          {def.inputs.map((input) => (
            <div key={input.name}>
              <label className="block text-sm text-gray-400 mb-1">
                {input.name}
                {input.required && <span className="text-red-400 ml-1">*</span>}
              </label>
              <input
                type="text"
                value={inputs[input.name] || ""}
                onChange={(e) =>
                  setInputs((prev) => ({ ...prev, [input.name]: e.target.value }))
                }
                className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded text-gray-200 text-sm"
              />
            </div>
          ))}

          {error && (
            <p className="text-red-400 text-sm">{error}</p>
          )}
        </div>

        <div className="flex justify-end gap-2 mt-6">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm text-gray-400 hover:text-gray-200"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting}
            className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-500 text-white rounded disabled:opacity-50"
          >
            {submitting ? "Starting..." : "Start Workflow"}
          </button>
        </div>
      </div>
    </div>
  );
}
