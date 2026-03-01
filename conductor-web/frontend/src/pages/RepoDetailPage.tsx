import { useState } from "react";
import { useParams, Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { WorktreeRow } from "../components/worktrees/WorktreeRow";
import { CreateWorktreeForm } from "../components/worktrees/CreateWorktreeForm";
import { TicketRow } from "../components/tickets/TicketRow";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";

export function RepoDetailPage() {
  const { repoId } = useParams<{ repoId: string }>();
  const { repos, refreshRepos } = useRepos();
  const repo = repos.find((r) => r.id === repoId);

  const {
    data: worktrees,
    loading: wtLoading,
    refetch: refetchWorktrees,
  } = useApi(() => api.listWorktrees(repoId!), [repoId]);

  const {
    data: tickets,
    loading: ticketsLoading,
    refetch: refetchTickets,
  } = useApi(() => api.listTickets(repoId!), [repoId]);

  const { data: latestRuns } = useApi(() => api.latestRunsByWorktree(), []);
  const { data: ticketTotals } = useApi(() => api.ticketAgentTotals(), []);

  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteRepoConfirm, setDeleteRepoConfirm] = useState(false);

  async function handleSyncTickets() {
    setSyncing(true);
    setSyncResult(null);
    try {
      const result = await api.syncTickets(repoId!);
      setSyncResult(`Synced ${result.synced}, closed ${result.closed}`);
      refetchTickets();
    } catch (err) {
      setSyncResult(
        err instanceof Error ? err.message : "Sync failed",
      );
    } finally {
      setSyncing(false);
    }
  }

  async function handleDeleteWorktree() {
    if (!deleteTarget) return;
    await api.deleteWorktree(deleteTarget);
    setDeleteTarget(null);
    refetchWorktrees();
  }

  async function handleDeleteRepo() {
    await api.deleteRepo(repoId!);
    setDeleteRepoConfirm(false);
    refreshRepos();
    window.location.href = "/";
  }

  if (!repo) {
    return (
      <div className="text-center py-12">
        <p className="text-gray-500">Repo not found</p>
        <Link to="/" className="text-indigo-600 hover:underline text-sm">
          Back to dashboard
        </Link>
      </div>
    );
  }

  return (
    <div className="space-y-8">
      {/* Header */}
      <div>
        <div className="flex items-center justify-between">
          <h2 className="text-xl font-bold text-gray-900">{repo.slug}</h2>
          <button
            onClick={() => setDeleteRepoConfirm(true)}
            className="px-3 py-1.5 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
          >
            Delete Repo
          </button>
        </div>
        <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm text-gray-600">
          <dt className="font-medium text-gray-500">Remote</dt>
          <dd className="truncate">{repo.remote_url}</dd>
          <dt className="font-medium text-gray-500">Local Path</dt>
          <dd className="truncate">{repo.local_path}</dd>
          <dt className="font-medium text-gray-500">Default Branch</dt>
          <dd>{repo.default_branch}</dd>
        </dl>
      </div>

      {/* Worktrees */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Worktrees
          </h3>
          <CreateWorktreeForm repoId={repoId!} onCreated={refetchWorktrees} />
        </div>
        {wtLoading ? (
          <LoadingSpinner />
        ) : !worktrees || worktrees.length === 0 ? (
          <EmptyState message="No worktrees yet" />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">Branch</th>
                  <th className="px-4 py-2">Status</th>
                  <th className="px-4 py-2">Agent</th>
                  <th className="px-4 py-2">Path</th>
                  <th className="px-4 py-2">Created</th>
                  <th className="px-4 py-2"></th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {worktrees.map((wt) => (
                  <WorktreeRow
                    key={wt.id}
                    worktree={wt}
                    latestRun={latestRuns?.[wt.id]}
                    onDelete={setDeleteTarget}
                  />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* Tickets */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Tickets
          </h3>
          <div className="flex items-center gap-3">
            {syncResult && (
              <span className="text-xs text-gray-500">{syncResult}</span>
            )}
            <button
              onClick={handleSyncTickets}
              disabled={syncing}
              className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50"
            >
              {syncing ? "Syncing..." : "Sync Tickets"}
            </button>
          </div>
        </div>
        {ticketsLoading ? (
          <LoadingSpinner />
        ) : !tickets || tickets.length === 0 ? (
          <EmptyState message="No tickets synced yet" />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">#</th>
                  <th className="px-4 py-2">Title</th>
                  <th className="px-4 py-2">State</th>
                  <th className="px-4 py-2">Labels</th>
                  <th className="px-4 py-2">Assignee</th>
                  <th className="px-4 py-2">Agent</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {tickets.map((t) => (
                  <TicketRow
                    key={t.id}
                    ticket={t}
                    agentTotals={ticketTotals?.[t.id]}
                  />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* Dialogs */}
      <ConfirmDialog
        open={deleteTarget !== null}
        title="Delete Worktree"
        message="Are you sure? This will remove the worktree and its git branch."
        onConfirm={handleDeleteWorktree}
        onCancel={() => setDeleteTarget(null)}
      />
      <ConfirmDialog
        open={deleteRepoConfirm}
        title="Delete Repo"
        message="Are you sure? This will unregister the repo from Conductor."
        onConfirm={handleDeleteRepo}
        onCancel={() => setDeleteRepoConfirm(false)}
      />
    </div>
  );
}
