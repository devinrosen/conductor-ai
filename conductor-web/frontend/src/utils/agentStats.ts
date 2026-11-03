import type { AgentRun, TicketAgentTotals } from "../api/types";

/** CSS class mapping for agent run status badges. */
export const statusColors: Record<string, string> = {
  running: "bg-yellow-100 text-yellow-700",
  waiting_for_feedback: "bg-purple-100 text-purple-700",
  completed: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
  cancelled: "bg-gray-100 text-gray-600",
  idle: "bg-sky-100 text-sky-700",
};

/** Statuses of an active run. Mirrors `AgentRun::is_active()` in conductor-core. */
const ACTIVE_STATUSES: ReadonlySet<string> = new Set(["running", "waiting_for_feedback", "idle"]);

/** Returns true if the run is currently active (running, waiting for feedback, or an idle interactive session). */
export function isActiveRun(run: { status: string }): boolean {
  return ACTIVE_STATUSES.has(run.status);
}

/** Returns true if the run is a developer's own Claude Code session (RFC 021). */
export function isInteractiveRun(run: { runtime?: string | null }): boolean {
  return run.runtime === "interactive";
}

/** Returns true if conductor may stop the run. Interactive sessions are never stopped by conductor, whatever their status. */
export function isStoppableRun(run: { status: string; runtime?: string | null }): boolean {
  return (
    !isInteractiveRun(run) && (run.status === "running" || run.status === "waiting_for_feedback")
  );
}

/** Format an ISO timestamp as local HH:MM, or null if missing/invalid. */
export function formatIdleSince(ts?: string | null): string | null {
  if (!ts) return null;
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return null;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
}

/** " since HH:MM" for an idle run, or "" when the timestamp is missing/invalid. */
export function idleSinceLabel(run: { idle_since?: string | null }): string {
  const t = formatIdleSince(run.idle_since);
  return t ? ` since ${t}` : "";
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

/** Format a token count as X.Xk for values ≥ 1000, or plain integer otherwise. */
function fmtTokensK(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(Math.round(n));
}

/** Format input/output token counts as "→ 5.8k ⊙ 342 →". */
export function formatTokens(input: number, output: number): string {
  return `→ ${fmtTokensK(input)} ⊙ ${fmtTokensK(output)} →`;
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

/** Build a compact stats string for ticket agent totals: "5.8k↓ 342↑ Xt". */
export function formatTicketTotalsCompact(totals: TicketAgentTotals): string {
  return `${formatTokens(totals.total_input_tokens, totals.total_output_tokens)} ${totals.total_turns}t`;
}

/** Build a full stats string: "5.8k↓ 342↑  Xt  XmXXs". */
export function formatTicketTotalsFull(totals: TicketAgentTotals): string {
  return `${formatTokens(totals.total_input_tokens, totals.total_output_tokens)}  ${totals.total_turns}t  ${formatDuration(totals.total_duration_ms)}`;
}

/** Build stats string for an agent run status line. */
export function formatRunStats(run: AgentRun, durationMs: number): string {
  const inputTokens = run.input_tokens ?? 0;
  const outputTokens = run.output_tokens ?? 0;
  const turns = run.num_turns ?? 0;
  const dur = formatDuration(durationMs);
  if (inputTokens > 0 || outputTokens > 0) {
    return `${formatTokens(inputTokens, outputTokens)}, ${turns} turns, ${dur}`;
  }
  return `${turns} turns, ${dur}`;
}
