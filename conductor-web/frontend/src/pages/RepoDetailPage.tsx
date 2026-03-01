import { useState, useMemo, useCallback } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { Ticket } from "../api/types";
import { WorktreeRow } from "../components/worktrees/WorktreeRow";
import { CreateWorktreeForm } from "../components/worktrees/CreateWorktreeForm";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import { IssueSourcesSection } from "../components/issue-sources/IssueSourcesSection";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";
import { useHotkeys } from "../hooks/useHotkeys";
import { useListNav } from "../hooks/useListNav";

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

  const { data: latestRuns, refetch: refetchRuns } = useApi(
    () => api.latestRunsByWorktree(),
    [],
  );
  const { data: ticketTotals, refetch: refetchTotals } = useApi(
    () => api.ticketAgentTotals(),
    [],
  );

  const {
    data: issueSources,
    loading: sourcesLoading,
    refetch: refetchSources,
  } = useApi(() => api.listIssueSources(repoId!), [repoId]);

  const sseHandlers = useMemo(() => {
    const handleWorktreeChange = (ev: ConductorEventData) => {
      if (!ev.data || ev.data.repo_id === repoId) refetchWorktrees();
    };
    const handleTicketsChange = (ev: ConductorEventData) => {
      if (!ev.data || ev.data.repo_id === repoId) refetchTickets();
    };
    const handleAgentChange = (_ev: ConductorEventData) => {
      refetchRuns();
      refetchTotals();
    };
    const map: Partial<
      Record<ConductorEventType, (data: ConductorEventData) => void>
    > = {
      worktree_created: handleWorktreeChange,
      worktree_deleted: handleWorktreeChange,
      tickets_synced: handleTicketsChange,
      agent_started: handleAgentChange,
      agent_stopped: handleAgentChange,
      issue_sources_changed: (ev: ConductorEventData) => {
        if (!ev.data || ev.data.repo_id === repoId) refetchSources();
      },
    };
    return map;
  }, [repoId, refetchWorktrees, refetchTickets, refetchRuns, refetchTotals, refetchSources]);

  useConductorEvents(sseHandlers);

  const navigate = useNavigate();
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteRepoConfirm, setDeleteRepoConfirm] = useState(false);
  const [selectedTicket, setSelectedTicket] = useState<Ticket | null>(null);
  const [createWtOpen, setCreateWtOpen] = useState(false);

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

  const wtCount = worktrees?.length ?? 0;
  const { selectedIndex, moveDown, moveUp, reset } = useListNav(wtCount);

  const openSelectedWt = useCallback(() => {
    const wt = worktrees?.[selectedIndex];
    if (wt) navigate(`/repos/${repoId}/worktrees/${wt.id}`);
  }, [worktrees, selectedIndex, navigate, repoId]);

  const openCreateWt = useCallback(() => setCreateWtOpen(true), []);

  const deleteSelectedWt = useCallback(() => {
    const wt = worktrees?.[selectedIndex];
    if (wt) setDeleteTarget(wt.id);
  }, [worktrees, selectedIndex]);

  const handleEscape = useCallback(() => {
    if (selectedTicket) {
      setSelectedTicket(null);
    } else if (selectedIndex >= 0) {
      reset();
    }
  }, [selectedTicket, selectedIndex, reset]);

  const noModalOpen = !selectedTicket && deleteTarget === null && !deleteRepoConfirm;

  useHotkeys([
    { key: "j", handler: moveDown, description: "Next worktree", enabled: noModalOpen },
    { key: "k", handler: moveUp, description: "Previous worktree", enabled: noModalOpen },
    { key: "Enter", handler: openSelectedWt, description: "Open selected", enabled: selectedIndex >= 0 && noModalOpen },
    { key: "c", handler: openCreateWt, description: "Create worktree", enabled: noModalOpen },
    { key: "d", handler: deleteSelectedWt, description: "Delete selected worktree", enabled: selectedIndex >= 0 && noModalOpen },
    { key: "Escape", handler: handleEscape, description: "Close / deselect" },
  ]);

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

      {/* Issue Sources */}
      <IssueSourcesSection
        repoId={repoId!}
        remoteUrl={repo.remote_url}
        sources={issueSources ?? []}
        loading={sourcesLoading}
        onChanged={refetchSources}
      />

      {/* Worktrees */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Worktrees
          </h3>
          <CreateWorktreeForm repoId={repoId!} onCreated={refetchWorktrees} open={createWtOpen} onOpenChange={setCreateWtOpen} />
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
                {worktrees.map((wt, index) => (
                  <WorktreeRow
                    key={wt.id}
                    worktree={wt}
                    latestRun={latestRuns?.[wt.id]}
                    onDelete={setDeleteTarget}
                    selected={index === selectedIndex}
                    index={index}
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
                    onClick={setSelectedTicket}
                  />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* Dialogs */}
      {selectedTicket && (
        <TicketDetailModal
          ticket={selectedTicket}
          onClose={() => setSelectedTicket(null)}
        />
      )}
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
