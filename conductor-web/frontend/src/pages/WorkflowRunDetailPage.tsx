import { useState, useEffect, useCallback } from "react";
import { useParams, Link } from "react-router";
import { api } from "../api/client";
import type { WorkflowRun, WorkflowRunStep } from "../api/types";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { TrainProgress } from "../components/shared/TrainProgress";
import { TransitBreadcrumb } from "../components/shared/TransitBreadcrumb";
import { formatDuration, liveElapsedMs } from "../utils/agentStats";
import { StepDetailPanel } from "../components/workflows/StepDetailPanel";
import { StatusBadge } from "../components/shared/StatusBadge";

function StepStatusIcon({ status }: { status: string }) {
  if (status === "completed") {
    return (
      <svg className="w-4 h-4 text-green-500 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.75.75 0 0 1 1.06-1.06L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z" />
      </svg>
    );
  }
  if (status === "failed" || status === "cancelled") {
    return (
      <svg className="w-4 h-4 text-red-500 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M3.72 3.72a.75.75 0 0 1 1.06 0L8 6.94l3.22-3.22a.75.75 0 1 1 1.06 1.06L9.06 8l3.22 3.22a.75.75 0 1 1-1.06 1.06L8 9.06l-3.22 3.22a.75.75 0 0 1-1.06-1.06L6.94 8 3.72 4.78a.75.75 0 0 1 0-1.06Z" />
      </svg>
    );
  }
  if (status === "running" || status === "waiting") {
    return (
      <span className="relative flex w-4 h-4 shrink-0">
        <span className="absolute inset-0.5 rounded-full bg-amber-400/30 animate-ping" style={{ animationDuration: "2s" }} />
        <span className="relative inline-flex w-4 h-4 items-center justify-center">
          <span className="w-2 h-2 rounded-full bg-amber-500" />
        </span>
      </span>
    );
  }
  if (status === "skipped") {
    return (
      <svg className="w-4 h-4 text-gray-400 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M1.5 4.5a.75.75 0 0 1 1.28-.53l4.72 4.72 4.72-4.72a.75.75 0 1 1 1.06 1.06l-5.25 5.25a.75.75 0 0 1-1.06 0L1.72 5.03a.75.75 0 0 1-.22-.53Z" />
      </svg>
    );
  }
  // pending
  return <span className="w-4 h-4 rounded-full border-2 border-gray-300 shrink-0" />;
}

interface GateOption {
  value: string;
  label: string;
}

function parseGateOptions(json: string | null): GateOption[] {
  if (!json) return [];
  try {
    const arr = JSON.parse(json);
    if (Array.isArray(arr)) return arr as GateOption[];
  } catch {
    // ignore
  }
  return [];
}

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
  const [gateSelections, setGateSelections] = useState<Set<string>>(new Set());
  const [gateSubmitting, setGateSubmitting] = useState(false);
  const [gateError, setGateError] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [selectedStepId, setSelectedStepId] = useState<string | null>(null);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);
  // Map from step name (e.g. "workflow:review-pr") to that child workflow's steps
  const [childStepsByName, setChildStepsByName] = useState<Map<string, WorkflowRunStep[]>>(new Map());
  const [expandedChildren, setExpandedChildren] = useState<Set<string>>(new Set());

  const fetchData = useCallback(async () => {
    if (!runId) return;
    try {
      const [runData, stepsData] = await Promise.all([
        api.getWorkflowRun(runId),
        api.getWorkflowSteps(runId),
      ]);
      setRun(runData);
      const sorted = stepsData.slice().sort((a, b) => a.position - b.position);
      setSteps(sorted);
      setFetchError(null);

      // Fetch child workflow runs and their steps
      const childRuns = await api.getChildWorkflowRuns(runId).catch(() => [] as WorkflowRun[]);
      if (childRuns.length > 0) {
        const childStepResults = await Promise.all(
          childRuns.map((cr) => api.getWorkflowSteps(cr.id).catch(() => [] as WorkflowRunStep[])),
        );
        const map = new Map<string, WorkflowRunStep[]>();
        for (let i = 0; i < childRuns.length; i++) {
          // Key by "workflow:<name>" to match the parent step's step_name
          const key = `workflow:${childRuns[i].workflow_name}`;
          map.set(key, childStepResults[i].slice().sort((a, b) => a.position - b.position));
        }
        setChildStepsByName(map);
      }
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
      const gateOptions = parseGateOptions(gateStep?.gate_options ?? null);
      const selections =
        gateOptions.length > 0 ? Array.from(gateSelections) : undefined;
      await api.approveGate(runId, gateFeedback || undefined, selections);
      setGateModalOpen(false);
      setGateStep(null);
      setGateFeedback("");
      setStatusMessage("Gate approved — workflow resuming");
      setTimeout(() => setStatusMessage(null), 4000);
      setGateSelections(new Set());
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
      setGateSelections(new Set());
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
    setGateSelections(new Set());
    setGateError(null);
    setGateModalOpen(true);
  }

  function toggleChildExpand(childRunId: string) {
    setExpandedChildren((prev) => {
      const next = new Set(prev);
      if (next.has(childRunId)) next.delete(childRunId); else next.add(childRunId);
      return next;
    });
  }

  function toggleSelection(value: string) {
    setGateSelections((prev) => {
      const next = new Set(prev);
      if (next.has(value)) next.delete(value);
      else next.add(value);
      return next;
    });
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
  const gateOptions = parseGateOptions(gateStep?.gate_options ?? null);

  return (
    <div className="space-y-6">
      <TransitBreadcrumb stops={[
        { label: "Home", href: "/" },
        { label: run.target_label?.split("/")[0] ?? "Repo", href: `/repos/${repoId}` },
        { label: run.target_label?.split("/").slice(1).join("/") ?? "Worktree", href: `/repos/${repoId}/worktrees/${worktreeId}` },
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
      {statusMessage && (
        <div className="rounded-md bg-green-50 border border-green-200 px-4 py-3 text-sm text-green-700">
          {statusMessage}
        </div>
      )}

      <div className="text-sm text-gray-500">
        Started <TimeAgo date={run.started_at} />
        {(() => {
          const ms = run.ended_at
            ? new Date(run.ended_at).getTime() - new Date(run.started_at).getTime()
            : isActive ? liveElapsedMs(run.started_at) : null;
          return ms != null ? <> · <span className="font-mono tabular-nums">{formatDuration(ms)}</span></> : null;
        })()}
      </div>

      {/* Train progress overview — only worth showing for 4+ steps */}
      {steps.length >= 4 && (
        <TrainProgress
          steps={steps.map((s) => ({ name: s.step_name.replace(/^workflow:/, ""), status: s.status }))}
        />
      )}

      {/* Steps + Detail panel split */}
      <div className="flex gap-4 flex-col lg:flex-row">
      <section className={selectedStepId ? "lg:w-1/2 lg:min-w-0" : "w-full"}>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Steps
        </h3>
        {steps.length === 0 ? (
          <p className="text-sm text-gray-500">No steps recorded yet.</p>
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <div className="divide-y divide-gray-100">
              {(() => {
                // Hide role badge if every step has the same role
                const roles = new Set(steps.map((s) => s.role).filter(Boolean));
                const showRole = roles.size > 1;
                return steps.map((step) => {
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
                    <div className="flex items-center gap-2 flex-wrap">
                      <StepStatusIcon status={step.status} />
                      <span className="text-sm">
                        {step.step_name.replace(/^workflow:/, "")}
                      </span>
                      {showRole && step.role && (
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
                    <div className="ml-6 mt-2 px-3 py-2 text-xs bg-amber-50 border border-amber-200 rounded-md text-amber-700">
                      <span className="font-medium">Gate feedback:</span> {step.gate_feedback}
                    </div>
                  )}

                  {/* Child workflow steps — expandable */}
                  {childStepsByName.has(step.step_name) && (() => {
                    const children = childStepsByName.get(step.step_name)!;
                    if (children.length === 0) return null;
                    const isOpen = expandedChildren.has(step.step_name);
                    const childRoles = new Set(children.map((s) => s.role).filter(Boolean));
                    const showChildRole = childRoles.size > 1;
                    return (
                      <div className="ml-6 mt-2">
                        <button
                          onClick={(e) => { e.stopPropagation(); toggleChildExpand(step.step_name); }}
                          className="flex items-center gap-1.5 text-xs text-gray-500 hover:text-gray-700 py-0.5"
                        >
                          <svg
                            className={`w-3 h-3 transition-transform duration-150 ${isOpen ? "rotate-90" : ""}`}
                            viewBox="0 0 16 16" fill="currentColor"
                          >
                            <path d="M6.22 4.22a.75.75 0 0 1 1.06 0l3.25 3.25a.75.75 0 0 1 0 1.06l-3.25 3.25a.75.75 0 0 1-1.06-1.06L8.94 8 6.22 5.28a.75.75 0 0 1 0-1.06Z" />
                          </svg>
                          <span>{children.length} steps</span>
                        </button>
                        {isOpen && (
                          <div className="mt-1 space-y-0.5 border-l border-gray-600/30 ml-1.5 pl-3">
                            {children.map((child) => {
                              const childMs = child.ended_at && child.started_at
                                ? new Date(child.ended_at).getTime() - new Date(child.started_at).getTime()
                                : (child.status === "running" || child.status === "waiting") && child.started_at
                                  ? liveElapsedMs(child.started_at) : null;
                              return (
                                <div key={child.id} className="flex items-center justify-between gap-2 py-0.5">
                                  <div className="flex items-center gap-2 min-w-0">
                                    <StepStatusIcon status={child.status} />
                                    <span className="text-xs text-gray-700">{child.step_name.replace(/^workflow:/, "")}</span>
                                    {showChildRole && child.role && (
                                      <span className="text-[10px] px-1 py-0.5 bg-gray-100 text-gray-500 rounded">{child.role}</span>
                                    )}
                                  </div>
                                  {childMs != null && (
                                    <span className="text-xs text-gray-400 font-mono tabular-nums shrink-0">{formatDuration(childMs)}</span>
                                  )}
                                </div>
                              );
                            })}
                          </div>
                        )}
                      </div>
                    );
                  })()}
                </div>
                );
              });
              })()}
            </div>
          </div>
        )}

        {/* Result summary — only show for failed runs where the error is useful */}
        {run.result_summary && run.status === "failed" && (
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
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
          onClick={() => { if (!gateSubmitting) { setGateModalOpen(false); setGateStep(null); } }}
        >
          <div
            role="dialog"
            aria-modal="true"
            aria-label={`Gate: ${gateStep.step_name}`}
            className="bg-white rounded-lg shadow-lg max-w-lg w-full mx-4"
            onClick={(e) => e.stopPropagation()}
          >
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

              {gateOptions.length > 0 ? (
                <div>
                  <label className="block text-sm font-medium text-gray-700 mb-2">
                    Select items to act on
                  </label>
                  <div className="space-y-2 max-h-48 overflow-y-auto border border-gray-200 rounded-md p-2">
                    {gateOptions.map((opt) => (
                      <label
                        key={opt.value}
                        className="flex items-center gap-2 cursor-pointer hover:bg-gray-50 px-2 py-1 rounded"
                      >
                        <input
                          type="checkbox"
                          checked={gateSelections.has(opt.value)}
                          onChange={() => toggleSelection(opt.value)}
                          className="rounded border-gray-300 text-indigo-600 focus:ring-indigo-500"
                        />
                        <span className="text-sm text-gray-800">{opt.label}</span>
                      </label>
                    ))}
                  </div>
                  <p className="text-xs text-gray-400 mt-1">
                    {gateSelections.size} of {gateOptions.length} selected — submit with no selections to skip
                  </p>
                </div>
              ) : (
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
              )}

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
              {gateOptions.length === 0 && (
                <button
                  onClick={handleReject}
                  disabled={gateSubmitting}
                  className="px-4 py-2 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-red-500 focus-visible:ring-offset-2"
                >
                  {gateSubmitting ? "Rejecting..." : "Reject"}
                </button>
              )}
              <button
                onClick={handleApprove}
                disabled={gateSubmitting}
                className="px-4 py-2 text-sm rounded-md bg-green-600 text-white hover:bg-green-700 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-green-500 focus-visible:ring-offset-2"
              >
                {gateSubmitting
                  ? "Submitting..."
                  : gateOptions.length > 0
                  ? `Submit (${gateSelections.size} selected)`
                  : "Approve"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
