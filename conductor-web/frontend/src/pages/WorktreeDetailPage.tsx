import { useState, useEffect } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { AgentRun } from "../api/types";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import {
  agentStatusColor,
  formatCost,
  formatDuration,
  formatRunStats,
  liveElapsedMs,
} from "../utils/agentStats";

export function WorktreeDetailPage() {
  const { repoId, worktreeId } = useParams<{
    repoId: string;
    worktreeId: string;
  }>();
  const navigate = useNavigate();

  const { data: worktrees, loading } = useApi(
    () => api.listWorktrees(repoId!),
    [repoId],
  );

  const { data: tickets } = useApi(
    () => api.listTickets(repoId!),
    [repoId],
  );

  const { data: agentRuns } = useApi(
    () => api.listAgentRuns(worktreeId!),
    [worktreeId],
  );

  const [deleteConfirm, setDeleteConfirm] = useState(false);

  const worktree = worktrees?.find((w) => w.id === worktreeId);
  const linkedTicket = worktree?.ticket_id
    ? tickets?.find((t) => t.id === worktree.ticket_id)
    : null;

  const latestRun = agentRuns && agentRuns.length > 0 ? agentRuns[0] : null;

  async function handleDelete() {
    await api.deleteWorktree(worktreeId!);
    navigate(`/repos/${repoId}`);
  }

  if (loading) return <LoadingSpinner />;

  if (!worktree) {
    return (
      <div className="text-center py-12">
        <p className="text-gray-500">Worktree not found</p>
        <Link
          to={`/repos/${repoId}`}
          className="text-indigo-600 hover:underline text-sm"
        >
          Back to repo
        </Link>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <Link
          to={`/repos/${repoId}`}
          className="text-sm text-indigo-600 hover:underline"
        >
          Back to repo
        </Link>
      </div>

      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-xl font-bold text-gray-900">
            {worktree.branch}
          </h2>
          <p className="text-sm text-gray-500 mt-1">{worktree.slug}</p>
        </div>
        <button
          onClick={() => setDeleteConfirm(true)}
          className="px-3 py-1.5 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
        >
          Delete Worktree
        </button>
      </div>

      <div className="rounded-lg border border-gray-200 bg-white p-4">
        <dl className="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 text-sm">
          <div>
            <dt className="font-medium text-gray-500">Status</dt>
            <dd className="mt-1">
              <StatusBadge status={worktree.status} />
            </dd>
          </div>
          <div>
            <dt className="font-medium text-gray-500">Branch</dt>
            <dd className="mt-1 text-gray-900">{worktree.branch}</dd>
          </div>
          <div>
            <dt className="font-medium text-gray-500">Path</dt>
            <dd className="mt-1 text-gray-900 truncate">{worktree.path}</dd>
          </div>
          <div>
            <dt className="font-medium text-gray-500">Created</dt>
            <dd className="mt-1 text-gray-900">
              <TimeAgo date={worktree.created_at} />
            </dd>
          </div>
          {worktree.completed_at && (
            <div>
              <dt className="font-medium text-gray-500">Completed</dt>
              <dd className="mt-1 text-gray-900">
                <TimeAgo date={worktree.completed_at} />
              </dd>
            </div>
          )}
        </dl>
      </div>

      {/* Agent Status */}
      {latestRun && (
        <AgentStatusSection latestRun={latestRun} runs={agentRuns!} />
      )}

      {linkedTicket && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Linked Ticket
          </h3>
          <div className="rounded-lg border border-gray-200 bg-white p-4">
            <div className="flex items-center gap-2">
              <a
                href={linkedTicket.url}
                target="_blank"
                rel="noopener noreferrer"
                className="text-indigo-600 hover:underline font-medium"
              >
                {linkedTicket.source_id}
              </a>
              <StatusBadge status={linkedTicket.state} />
            </div>
            <p className="mt-1 text-sm text-gray-900">{linkedTicket.title}</p>
            {linkedTicket.assignee && (
              <p className="mt-1 text-xs text-gray-500">
                Assigned to {linkedTicket.assignee}
              </p>
            )}
          </div>
        </section>
      )}

      {/* Agent Run History */}
      {agentRuns && agentRuns.length > 1 && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Agent Runs ({agentRuns.length})
          </h3>
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">Status</th>
                  <th className="px-4 py-2">Cost</th>
                  <th className="px-4 py-2">Turns</th>
                  <th className="px-4 py-2">Duration</th>
                  <th className="px-4 py-2">Started</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {agentRuns.map((run) => (
                  <AgentRunRow key={run.id} run={run} />
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

      <ConfirmDialog
        open={deleteConfirm}
        title="Delete Worktree"
        message="Are you sure? This will remove the worktree and its git branch."
        onConfirm={handleDelete}
        onCancel={() => setDeleteConfirm(false)}
      />
    </div>
  );
}

function AgentStatusSection({
  latestRun,
  runs,
}: {
  latestRun: AgentRun;
  runs: AgentRun[];
}) {
  const [, setTick] = useState(0);

  // Tick every second to update live elapsed time when agent is running
  useEffect(() => {
    if (latestRun.status !== "running") return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [latestRun.status]);

  // Aggregate totals from completed runs
  const completedRuns = runs.filter((r) => r.status === "completed");
  const totalCost = completedRuns.reduce((s, r) => s + (r.cost_usd ?? 0), 0);
  const totalTurns = completedRuns.reduce(
    (s, r) => s + (r.num_turns ?? 0),
    0,
  );
  const totalDurationMs = completedRuns.reduce(
    (s, r) => s + (r.duration_ms ?? 0),
    0,
  );

  let durationMs: number;
  let displayTurns: number;
  if (latestRun.status === "running") {
    durationMs = totalDurationMs + liveElapsedMs(latestRun.started_at);
    displayTurns = totalTurns + (latestRun.num_turns ?? 0);
  } else {
    durationMs = totalDurationMs;
    displayTurns = totalTurns;
  }

  const runsLabel = runs.length > 1 ? ` (${runs.length} runs)` : "";

  const statsText =
    latestRun.status === "failed"
      ? latestRun.result_text
        ? latestRun.result_text.slice(0, 80)
        : ""
      : latestRun.status === "cancelled"
        ? ""
        : formatRunStats(
            { ...latestRun, cost_usd: totalCost, num_turns: displayTurns },
            durationMs,
          ) + runsLabel;

  return (
    <section>
      <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
        Agent
      </h3>
      <div className="rounded-lg border border-gray-200 bg-white p-4">
        <div className="flex items-center gap-3 text-sm">
          <span className="text-gray-500">Agent:</span>
          <span
            className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${agentStatusColor(latestRun.status)}`}
          >
            {latestRun.status}
          </span>
          {statsText && <span className="text-gray-500">{statsText}</span>}
        </div>
        {latestRun.claude_session_id && latestRun.status !== "running" && (
          <p className="mt-2 text-xs text-gray-400">
            session: {latestRun.claude_session_id.slice(0, 13)}
          </p>
        )}
      </div>
    </section>
  );
}

function AgentRunRow({ run }: { run: AgentRun }) {
  return (
    <tr>
      <td className="px-4 py-2">
        <span
          className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${agentStatusColor(run.status)}`}
        >
          {run.status}
        </span>
      </td>
      <td className="px-4 py-2 text-gray-600">
        {run.cost_usd != null ? formatCost(run.cost_usd) : "-"}
      </td>
      <td className="px-4 py-2 text-gray-600">{run.num_turns ?? "-"}</td>
      <td className="px-4 py-2 text-gray-600">
        {run.duration_ms != null ? formatDuration(run.duration_ms) : "-"}
      </td>
      <td className="px-4 py-2 text-gray-500">
        <TimeAgo date={run.started_at} />
      </td>
    </tr>
  );
}
