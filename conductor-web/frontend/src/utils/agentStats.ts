import type { AgentRun, TicketAgentTotals } from "../api/types";

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
  switch (status) {
    case "running":
      return "bg-yellow-100 text-yellow-700";
    case "completed":
      return "bg-green-100 text-green-700";
    case "failed":
      return "bg-red-100 text-red-700";
    case "cancelled":
      return "bg-gray-100 text-gray-500";
    default:
      return "bg-gray-100 text-gray-600";
  }
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
