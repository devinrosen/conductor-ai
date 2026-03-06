import type { AgentRun, TicketAgentTotals } from "../api/types";

/** CSS class mapping for agent run status badges. */
export const statusColors: Record<string, string> = {
  running: "bg-yellow-100 text-yellow-700",
  waiting_for_feedback: "bg-purple-100 text-purple-700",
  completed: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
  cancelled: "bg-gray-100 text-gray-600",
};

/** Returns true if the run is currently active (running or waiting for feedback). */
export function isActiveRun(run: { status: string }): boolean {
  return run.status === "running" || run.status === "waiting_for_feedback";
}

/** Protocol marker that agents emit to request human feedback. */
export const FEEDBACK_MARKER = "[NEEDS_FEEDBACK]";

/** Human-readable labels for statuses that differ from the raw key. */
export const statusLabels: Record<string, string> = {
  waiting_for_feedback: "waiting for feedback",
};

/** Format cost as $X.XXXX (4 decimal places). */
export function formatCost(cost: number): string {
  return `$${cost.toFixed(4)}`;
}

/** Format cost compactly as $X.XX (2 decimal places) for inline views. */
export function formatCostCompact(cost: number): string {
  return `$${cost.toFixed(2)}`;
}

/** Format duration from milliseconds to human-readable. */
export function formatDuration(ms: number): string {
  const totalSecs = ms / 1000;
  const mins = Math.floor(totalSecs / 60);
  const secs = Math.floor(totalSecs % 60);
  if (mins > 0) {
    return `${mins}m${String(secs).padStart(2, "0")}s`;
  }
  return `${totalSecs.toFixed(1)}s`;
}

/** Compute live elapsed duration for a running agent. */
export function liveElapsedMs(startedAt: string): number {
  const start = new Date(startedAt).getTime();
  return Math.max(0, Date.now() - start);
}

/** Status color classes for agent run statuses. */
export function agentStatusColor(status: string): string {
  return statusColors[status] ?? "bg-gray-100 text-gray-600";
}

/** Build a compact stats string for ticket agent totals: "$X.XX Xt". */
export function formatTicketTotalsCompact(totals: TicketAgentTotals): string {
  return `${formatCostCompact(totals.total_cost)} ${totals.total_turns}t`;
}

/** Build a full stats string: "$X.XX  Xt  XmXXs". */
export function formatTicketTotalsFull(totals: TicketAgentTotals): string {
  return `${formatCostCompact(totals.total_cost)}  ${totals.total_turns}t  ${formatDuration(totals.total_duration_ms)}`;
}

/** Build stats string for an agent run status line. */
export function formatRunStats(run: AgentRun, durationMs: number): string {
  const cost = run.cost_usd ?? 0;
  const turns = run.num_turns ?? 0;
  const dur = formatDuration(durationMs);
  if (cost > 0) {
    return `${formatCost(cost)}, ${turns} turns, ${dur}`;
  }
  return `${turns} turns, ${dur}`;
}
