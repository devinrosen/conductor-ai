import { useState, useEffect, useCallback } from "react";
import { useParams, Link } from "react-router";
import { api } from "../api/client";
import type { WorkflowRun, WorkflowRunStep } from "../api/types";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { TrainProgress } from "../components/shared/TrainProgress";
import { TransitBreadcrumb } from "../components/shared/TransitBreadcrumb";
import { formatDuration, liveElapsedMs } from "../utils/agentStats";
import { StepDetailPanel } from "../components/workflows/StepDetailPanel";

export function WorkflowRunDetailPage() {
  const { repoId, worktreeId, runId } = useParams<{
    repoId: string;
    worktreeId: string;
    runId: string;
  }>();

  const [run, setRun] = useState<WorkflowRun | null>(null);
  const [steps, setSteps] = useState<WorkflowRunStep[]>([]);
  const [loading, setLoading] = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [gateModalOpen, setGateModalOpen] = useState(false);
  const [gateStep, setGateStep] = useState<WorkflowRunStep | null>(null);
  const [gateFeedback, setGateFeedback] = useState("");
  const [gateSubmitting, setGateSubmitting] = useState(false);
  const [gateError, setGateError] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [selectedStepId, setSelectedStepId] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    if (!runId) return;
    try {
      const [runData, stepsData] = await Promise.all([
        api.getWorkflowRun(runId),
        api.getWorkflowSteps(runId),
      ]);
      setRun(runData);
      setSteps(stepsData.slice().sort((a, b) => a.position - b.position));
      setFetchError(null);
    } catch (err) {
      setFetchError(err instanceof Error ? err.message : "Failed to load workflow run");
    } finally {
      setLoading(false);
    }
  }, [runId]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  // Poll when run is active
  useEffect(() => {
    if (!run) return;
    if (run.status !== "running" && run.status !== "waiting") return;
    const interval = setInterval(fetchData, 3000);
    return () => clearInterval(interval);
  }, [run, fetchData]);

  async function handleCancel() {
    if (!runId) return;
    setCancelling(true);
    setCancelError(null);
    try {
      await api.cancelWorkflow(runId);
      await fetchData();
    } catch (err) {
      setCancelError(err instanceof Error ? err.message : "Cancel failed — try again");
    } finally {
      setCancelling(false);
    }
  }

  async function handleApprove() {
    if (!runId) return;
    setGateSubmitting(true);
    setGateError(null);
    try {
      await api.approveGate(runId, gateFeedback || undefined);
      setGateModalOpen(false);
      setGateStep(null);
      setGateFeedback("");
      await fetchData();
    } catch (err) {
      setGateError(err instanceof Error ? err.message : "Failed to approve");
    } finally {
      setGateSubmitting(false);
    }
  }

  async function handleReject() {
    if (!runId) return;
    setGateSubmitting(true);
    setGateError(null);
    try {
      await api.rejectGate(runId);
      setGateModalOpen(false);
      setGateStep(null);
      setGateFeedback("");
      await fetchData();
    } catch (err) {
      setGateError(err instanceof Error ? err.message : "Failed to reject");
    } finally {
      setGateSubmitting(false);
    }
  }

  function openGateModal(step: WorkflowRunStep) {
    setGateStep(step);
    setGateFeedback("");
    setGateError(null);
    setGateModalOpen(true);
  }

  function handleStepKeyDown(e: React.KeyboardEvent, stepId: string) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      setSelectedStepId(selectedStepId === stepId ? null : stepId);
    }
  }

  if (loading) return <LoadingSpinner />;

  if (fetchError && !run) {
    return (
      <div className="text-center py-12 space-y-3">
        <p className="text-red-500 text-sm">{fetchError}</p>
        <button
          onClick={fetchData}
          className="px-4 py-2 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700"
        >
          Retry
        </button>
        <div>
          <Link
            to={`/repos/${repoId}/worktrees/${worktreeId}`}
            className="text-indigo-600 hover:underline text-sm"
          >
            Back to worktree
          </Link>
        </div>
      </div>
    );
  }

  if (!run) {
    return (
      <div className="text-center py-12">
        <p className="text-gray-500">Workflow run not found</p>
        <Link
          to={`/repos/${repoId}/worktrees/${worktreeId}`}
          className="text-indigo-600 hover:underline text-sm"
        >
          Back to worktree
        </Link>
      </div>
    );
  }

  const isActive = run.status === "running" || run.status === "waiting";

  return (
    <div className="space-y-6">
      <TransitBreadcrumb stops={[
        { label: "Home", href: "/" },
        { label: "Repo", href: `/repos/${repoId}` },
        { label: "Worktree", href: `/repos/${repoId}/worktrees/${worktreeId}` },
        { label: run.workflow_name, current: true },
      ]} />

      {/* Header */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        <div className="flex items-center gap-3 flex-wrap">
          <h2 className="text-xl font-bold text-gray-900">{run.workflow_name}</h2>
          <StatusBadge status={run.status} />
          {run.dry_run && (
            <span className="text-xs px-2 py-0.5 bg-yellow-100 text-yellow-700 rounded border border-yellow-200">
              dry-run
            </span>
          )}
          <Link
            to={`/repos/${repoId}/worktrees/${worktreeId}/workflows/defs/${encodeURIComponent(run.workflow_name)}`}
            className="text-xs text-indigo-500 hover:text-indigo-700 hover:underline"
          >
            View Definition
          </Link>
        </div>
        {isActive && (
          <button
            onClick={handleCancel}
            disabled={cancelling}
            className="px-3 py-2 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50 disabled:opacity-50 sm:self-auto"
          >
            {cancelling ? "Cancelling..." : "Cancel Run"}
          </button>
        )}
      </div>

      {cancelError && (
        <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">
          {cancelError}
        </div>
      )}

      <div className="text-sm text-gray-500">
        Started <TimeAgo date={run.started_at} />
        {run.ended_at && (
          <> · Ended <TimeAgo date={run.ended_at} /></>
        )}
        {(() => {
          const ms = run.ended_at
            ? new Date(run.ended_at).getTime() - new Date(run.started_at).getTime()
            : isActive ? liveElapsedMs(run.started_at) : null;
          return ms != null ? <> · <span className="font-mono tabular-nums">{formatDuration(ms)}</span></> : null;
        })()}
      </div>

      {/* Train progress overview */}
      {steps.length > 0 && (
        <TrainProgress
          steps={steps.map((s) => ({ name: s.step_name, status: s.status }))}
        />
      )}

      {/* Steps + Detail panel split */}
      <div className={`flex gap-4 ${selectedStepId ? "flex-col lg:flex-row" : ""}`}>
      <section className={selectedStepId ? "lg:w-1/2 lg:min-w-0" : "w-full"}>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Steps
        </h3>
        {steps.length === 0 ? (
          <p className="text-sm text-gray-500">No steps recorded yet.</p>
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <div className="divide-y divide-gray-100">
              {steps.map((step) => {
                const parsedMarkers: string[] = (() => {
                  if (!step.markers_out) return [];
                  try { return JSON.parse(step.markers_out); } catch { return []; }
                })();

                return (
                <div
                  key={step.id}
                  role="button"
                  tabIndex={0}
                  className={`px-4 py-3 cursor-pointer transition-colors outline-none focus-visible:ring-2 focus-visible:ring-indigo-500 focus-visible:ring-inset ${selectedStepId === step.id ? "bg-indigo-50" : "hover:bg-gray-50"}`}
                  onClick={() => setSelectedStepId(selectedStepId === step.id ? null : step.id)}
                  onKeyDown={(e) => handleStepKeyDown(e, step.id)}
                >
                  <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
                    <div className="flex items-center gap-3 flex-wrap">
                      <span className="text-gray-400 text-xs font-mono w-6 text-right shrink-0">
                        {step.position}
                      </span>
                      <span className="text-gray-900 text-sm font-medium">
                        {step.step_name}
                      </span>
                      <StatusBadge status={step.status} />
                      {step.role && (
                        <span className="text-xs px-1.5 py-0.5 bg-gray-100 text-gray-500 rounded">
                          {step.role}
                        </span>
                      )}
                      {step.iteration > 0 && (
                        <span className="text-xs text-gray-400">
                          iter {step.iteration}
                        </span>
                      )}
                      {step.retry_count > 0 && (
                        <span className="text-xs text-yellow-600">
                          {step.retry_count} retr{step.retry_count === 1 ? "y" : "ies"}
                        </span>
                      )}
                      {parsedMarkers.map((m) => (
                        <span key={m} className="text-[10px] px-1.5 py-0.5 bg-indigo-50 text-indigo-600 rounded-full font-medium">
                          {m}
                        </span>
                      ))}
                    </div>
                    <div className="flex items-center gap-3 ml-9 sm:ml-0">
                      {step.started_at && (
                        <span className="text-xs text-gray-400 font-mono tabular-nums">
                          {(() => {
                            const ms = step.ended_at
                              ? new Date(step.ended_at).getTime() - new Date(step.started_at).getTime()
                              : (step.status === "running" || step.status === "waiting") ? liveElapsedMs(step.started_at) : null;
                            return ms != null ? formatDuration(ms) : null;
                          })()}
                        </span>
                      )}
                      {step.gate_type && step.status === "waiting" && (
                        <button
                          onClick={(e) => { e.stopPropagation(); openGateModal(step); }}
                          className="px-3 py-1.5 text-xs rounded-md bg-indigo-600 text-white hover:bg-indigo-700 focus-visible:ring-2 focus-visible:ring-indigo-500 focus-visible:ring-offset-2"
                        >
                          Review Gate
                        </button>
                      )}
                    </div>
                  </div>

                  {/* Failed step result — always visible inline */}
                  {step.status === "failed" && step.result_text && (
                    <div className="ml-9 mt-2 px-3 py-2 text-xs bg-red-50 border border-red-200 rounded-md text-red-700 whitespace-pre-wrap font-mono">
                      {step.result_text}
                    </div>
                  )}

                  {/* Gate feedback — always visible inline */}
                  {step.gate_feedback && (
                    <div className="ml-9 mt-2 px-3 py-2 text-xs bg-amber-50 border border-amber-200 rounded-md text-amber-700">
                      <span className="font-medium">Gate feedback:</span> {step.gate_feedback}
                    </div>
                  )}
                </div>
                );
              })}
            </div>
          </div>
        )}

        {/* Result summary */}
        {run.result_summary && (
          <div className="mt-4">
            <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
              Result
            </h3>
            <div className="rounded-lg border border-gray-200 bg-white p-4">
              <p className="text-sm text-gray-700 whitespace-pre-wrap">
                {run.result_summary}
              </p>
            </div>
          </div>
        )}
      </section>

      {/* Step detail panel */}
      {selectedStepId && (() => {
        const step = steps.find((s) => s.id === selectedStepId);
        if (!step || !worktreeId) return null;
        return (
          <div className="lg:w-1/2 lg:min-w-0 rounded-lg border border-gray-200 overflow-hidden">
            <StepDetailPanel
              step={step}
              worktreeId={worktreeId}
              onClose={() => setSelectedStepId(null)}
            />
          </div>
        );
      })()}
      </div>

      {/* Gate Modal */}
      {gateModalOpen && gateStep && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
          <div className="bg-white rounded-lg shadow-lg max-w-lg w-full mx-4">
            <div className="px-6 py-4 border-b border-gray-200">
              <h3 className="text-lg font-semibold text-gray-900">
                Gate: {gateStep.step_name}
              </h3>
              <p className="text-xs text-gray-400 mt-1">
                Type: {gateStep.gate_type}
              </p>
            </div>

            <div className="px-6 py-4 space-y-4">
              {gateStep.gate_prompt && (
                <div>
                  <label className="block text-sm font-medium text-gray-700 mb-1">
                    Gate prompt
                  </label>
                  <div className="px-3 py-2 text-sm bg-gray-50 border border-gray-200 rounded-md text-gray-800 whitespace-pre-wrap">
                    {gateStep.gate_prompt}
                  </div>
                </div>
              )}

              <div>
                <label className="block text-sm font-medium text-gray-700 mb-1">
                  Feedback (optional)
                </label>
                <textarea
                  value={gateFeedback}
                  onChange={(e) => setGateFeedback(e.target.value)}
                  placeholder="Add optional feedback..."
                  rows={3}
                  className="w-full px-3 py-2 text-sm border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500 resize-none"
                />
              </div>

              {gateError && (
                <div className="px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200">
                  {gateError}
                </div>
              )}
            </div>

            <div className="px-6 py-4 border-t border-gray-200 flex justify-end gap-2">
              <button
                onClick={() => {
                  setGateModalOpen(false);
                  setGateStep(null);
                }}
                disabled={gateSubmitting}
                className="px-4 py-2 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-indigo-500 focus-visible:ring-offset-2"
              >
                Cancel
              </button>
              <button
                onClick={handleReject}
                disabled={gateSubmitting}
                className="px-4 py-2 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-red-500 focus-visible:ring-offset-2"
              >
                {gateSubmitting ? "Rejecting..." : "Reject"}
              </button>
              <button
                onClick={handleApprove}
                disabled={gateSubmitting}
                className="px-4 py-2 text-sm rounded-md bg-green-600 text-white hover:bg-green-700 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-green-500 focus-visible:ring-offset-2"
              >
                {gateSubmitting ? "Approving..." : "Approve"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
