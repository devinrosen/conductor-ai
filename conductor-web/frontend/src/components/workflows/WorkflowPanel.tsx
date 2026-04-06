import { useState, useEffect, useCallback } from "react";
import { Link } from "react-router";
import { api } from "../../api/client";
import type { WorkflowDefSummary, WorkflowRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";
import { RunWorkflowModal } from "./RunWorkflowModal";

function GroupSection({
  title,
  defs,
  onRun,
}: {
  title: string;
  defs: WorkflowDefSummary[];
  onRun: (def: WorkflowDefSummary) => void;
}) {
  return (
    <div>
      <div className="flex items-center gap-2 mb-2">
        <h4 className="text-sm font-semibold text-gray-400 uppercase tracking-wide">{title}</h4>
        <div className="flex-1 h-px bg-gray-700" />
      </div>
      <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
        {defs.map((def) => (
          <WorkflowCard key={def.name} def={def} onRun={onRun} />
        ))}
      </div>
    </div>
  );
}

function WorkflowCard({
  def,
  onRun,
}: {
  def: WorkflowDefSummary;
  onRun: (def: WorkflowDefSummary) => void;
}) {
  return (
    <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
      <div className="flex items-center justify-between mb-2">
        <h4 className="font-medium text-gray-200">{def.title ?? def.name}</h4>
        <span className="text-xs px-2 py-0.5 bg-gray-700 rounded text-gray-400">
          {def.trigger}
        </span>
      </div>
      <p className="text-sm text-gray-400 mb-3">{def.description || "No description"}</p>
      <div className="flex items-center justify-between">
        <span className="text-xs text-gray-500">
          {def.node_count} step{def.node_count !== 1 ? "s" : ""}
          {def.inputs.length > 0 &&
            ` · ${def.inputs.length} input${def.inputs.length !== 1 ? "s" : ""}`}
        </span>
        <button
          onClick={() => onRun(def)}
          className="px-3 py-1 text-sm bg-cyan-600 hover:bg-cyan-500 text-white rounded"
        >
          Run
        </button>
      </div>
    </div>
  );
}

interface WorkflowPanelProps {
  repoId: string;
  worktreeId: string;
  ticketId?: string;
}

export function WorkflowPanel({ repoId, worktreeId, ticketId }: WorkflowPanelProps) {
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
        ) : (() => {
          const hasGroups = defs.some((d) => d.group !== null);
          if (!hasGroups) {
            return (
              <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                {defs.map((def) => (
                  <WorkflowCard key={def.name} def={def} onRun={setRunModalDef} />
                ))}
              </div>
            );
          }
          const groupMap = new Map<string, WorkflowDefSummary[]>();
          const ungrouped: WorkflowDefSummary[] = [];
          for (const def of defs) {
            if (def.group === null) {
              ungrouped.push(def);
            } else {
              const existing = groupMap.get(def.group);
              if (existing) {
                existing.push(def);
              } else {
                groupMap.set(def.group, [def]);
              }
            }
          }
          const sortedGroups = Array.from(groupMap.entries()).sort(([a], [b]) =>
            a.localeCompare(b)
          );
          return (
            <div className="space-y-5">
              {sortedGroups.map(([groupName, groupDefs]) => (
                <GroupSection
                  key={groupName}
                  title={groupName}
                  defs={groupDefs}
                  onRun={setRunModalDef}
                />
              ))}
              {ungrouped.length > 0 && (
                <GroupSection title="Other" defs={ungrouped} onRun={setRunModalDef} />
              )}
            </div>
          );
        })()}
      </div>

      {/* Run Modal */}
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
