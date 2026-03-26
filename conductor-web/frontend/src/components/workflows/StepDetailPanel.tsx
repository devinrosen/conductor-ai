import type { WorkflowRunStep } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { formatDuration, liveElapsedMs } from "../../utils/agentStats";
import { AgentEventFeed } from "./AgentEventFeed";

interface StepDetailPanelProps {
  step: WorkflowRunStep;
  worktreeId: string;
  onClose: () => void;
}

export function StepDetailPanel({ step, worktreeId, onClose }: StepDetailPanelProps) {
  const stepDurationMs = (() => {
    if (!step.started_at) return null;
    if (step.ended_at) return new Date(step.ended_at).getTime() - new Date(step.started_at).getTime();
    if (step.status === "running" || step.status === "waiting") return liveElapsedMs(step.started_at);
    return null;
  })();

  const parsedMarkers: string[] = (() => {
    if (!step.markers_out) return [];
    try { return JSON.parse(step.markers_out); } catch { return []; }
  })();

  const isStepActive = step.status === "running" || step.status === "waiting";

  return (
    <div className="border-l border-gray-200 bg-white overflow-y-auto h-full">
      {/* Header */}
      <div className="sticky top-0 bg-white border-b border-gray-200 px-4 py-3 flex items-center justify-between">
        <div className="flex items-center gap-2 min-w-0">
          <span className="text-sm font-semibold text-gray-900 truncate">{step.step_name}</span>
          <StatusBadge status={step.status} />
        </div>
        <button
          onClick={onClose}
          aria-label="Close panel"
          className="text-gray-400 hover:text-gray-600 text-sm shrink-0 ml-2 focus-visible:ring-2 focus-visible:ring-indigo-500 rounded"
        >
          &times;
        </button>
      </div>

      <div className="px-4 py-3 space-y-4">
        {/* Metadata */}
        <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
          <span className="text-gray-400">Position</span>
          <span className="text-gray-700">{step.position}</span>
          {step.role && (
            <>
              <span className="text-gray-400">Role</span>
              <span className="text-gray-700">{step.role}</span>
            </>
          )}
          {step.iteration > 0 && (
            <>
              <span className="text-gray-400">Iteration</span>
              <span className="text-gray-700">{step.iteration}</span>
            </>
          )}
          {step.retry_count > 0 && (
            <>
              <span className="text-gray-400">Retries</span>
              <span className="text-yellow-600">{step.retry_count}</span>
            </>
          )}
          {stepDurationMs != null && (
            <>
              <span className="text-gray-400">Duration</span>
              <span className="text-gray-700 font-mono tabular-nums">{formatDuration(stepDurationMs)}</span>
            </>
          )}
        </div>

        {/* Markers */}
        {parsedMarkers.length > 0 && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 mb-1">Markers</h4>
            <div className="flex flex-wrap gap-1">
              {parsedMarkers.map((m) => (
                <span key={m} className="text-[10px] px-1.5 py-0.5 bg-indigo-50 text-indigo-600 rounded-full font-medium">
                  {m}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Result text */}
        {step.result_text && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 mb-1">Result</h4>
            <pre className={`text-xs px-3 py-2 rounded-md border whitespace-pre-wrap font-mono overflow-x-auto ${
              step.status === "failed"
                ? "bg-red-50 border-red-200 text-red-700"
                : "bg-gray-50 border-gray-200 text-gray-700"
            }`}>
              {step.result_text}
            </pre>
          </div>
        )}

        {/* Gate info */}
        {step.gate_type && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 mb-1">Gate</h4>
            <div className="text-xs space-y-1">
              <div><span className="text-gray-400">Type:</span> <span className="text-gray-700">{step.gate_type}</span></div>
              {step.gate_prompt && (
                <div className="px-3 py-2 bg-gray-50 border border-gray-200 rounded-md text-gray-700 whitespace-pre-wrap">
                  {step.gate_prompt}
                </div>
              )}
              {step.gate_approved_by && (
                <div><span className="text-gray-400">Approved by:</span> <span className="text-gray-700">{step.gate_approved_by}</span></div>
              )}
              {step.gate_feedback && (
                <div className="px-3 py-2 bg-amber-50 border border-amber-200 rounded-md text-amber-700">
                  <span className="font-medium">Feedback:</span> {step.gate_feedback}
                </div>
              )}
            </div>
          </div>
        )}

        {/* Context output */}
        {step.context_out && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 mb-1">Context Output</h4>
            <pre className="text-xs px-3 py-2 bg-gray-50 border border-gray-200 rounded-md text-gray-600 whitespace-pre-wrap font-mono overflow-x-auto">
              {step.context_out}
            </pre>
          </div>
        )}

        {/* Agent events */}
        {step.child_run_id && (
          <div>
            <h4 className="text-xs font-medium text-gray-500 mb-1">Agent Activity</h4>
            <AgentEventFeed
              worktreeId={worktreeId}
              agentRunId={step.child_run_id}
              active={isStepActive}
            />
          </div>
        )}
      </div>
    </div>
  );
}
