import type { WorkflowRun } from "../api/types";
import { formatDuration } from "./agentStats";

/** Format step progress like "Step 3/7" or just the step name if totals are missing. */
export function formatStepProgress(run: WorkflowRun): string | null {
  if (!run.current_step) return null;
  if (run.total_steps) {
    return `Step ${run.current_step}/${run.total_steps}`;
  }
  return run.current_step_name ?? null;
}

/** Format iteration info like "iter 2/10" for do-while loops. */
export function formatIteration(run: WorkflowRun): string | null {
  if (run.current_iteration == null || run.current_iteration === 0) return null;
  const iter = run.current_iteration;
  if (run.max_iterations) {
    return `iter ${iter}/${run.max_iterations}`;
  }
  return `iter ${iter}`;
}

/** Build a compact progress string combining step + iteration + step name. */
export function formatWorkflowProgress(run: WorkflowRun): string | null {
  const parts: string[] = [];
  const step = formatStepProgress(run);
  if (step) parts.push(step);
  const iter = formatIteration(run);
  if (iter) parts.push(iter);
  if (run.current_step_name) parts.push(run.current_step_name);
  if (run.estimated_remaining_ms != null && run.estimated_remaining_ms > 0) {
    parts.push(`~${formatDuration(run.estimated_remaining_ms)} left`);
  }
  return parts.length > 0 ? parts.join(" \u00b7 ") : null;
}
