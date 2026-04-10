import type { WorkflowRun } from '../api/types';

/**
 * Gets the display name for a workflow run, preferring workflow_title over workflow_name.
 */
export function getWorkflowDisplayName(run: WorkflowRun): string {
  return run.workflow_title ?? run.workflow_name;
}