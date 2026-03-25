import { useState, useMemo, useCallback } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { AgentRun, Ticket } from "../api/types";
import { WorktreeRow } from "../components/worktrees/WorktreeRow";
import { CreateWorktreeForm } from "../components/worktrees/CreateWorktreeForm";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import { IssueSourcesSection } from "../components/issue-sources/IssueSourcesSection";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { ModelPicker } from "../components/shared/ModelPicker";
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

  const [showClosedTickets, setShowClosedTickets] = useState(false);
  const [showCompletedWorktrees, setShowCompletedWorktrees] = useState(false);

  const {
    data: worktrees,
    loading: wtLoading,
    refetch: refetchWorktrees,
  } = useApi(() => api.listWorktrees(repoId!, showCompletedWorktrees), [repoId, showCompletedWorktrees]);

  const {
    data: tickets,
    loading: ticketsLoading,
    refetch: refetchTickets,
  } = useApi(() => api.listTickets(repoId!, showClosedTickets), [repoId, showClosedTickets]);

  const { data: latestRuns, refetch: refetchRuns } = useApi(
    () => api.latestRunsByWorktreeForRepo(repoId!),
    [repoId],
  );
  const { data: ticketTotals, refetch: refetchTotals } = useApi(
    () => api.ticketAgentTotalsForRepo(repoId!),
    [repoId],
  );

  const {
    data: issueSources,
    loading: sourcesLoading,
    refetch: refetchSources,
  } = useApi(() => api.listIssueSources(repoId!), [repoId]);

  const {
    data: repoAgentRuns,
    refetch: refetchRepoAgentRuns,
  } = useApi(() => api.listRepoAgentRuns(repoId!), [repoId]);

  const [repoAgentPrompt, setRepoAgentPrompt] = useState("");
  const [showAgentPrompt, setShowAgentPrompt] = useState(false);
  const [startingRepoAgent, setStartingRepoAgent] = useState(false);
  const [newRepoAgentSession, setNewRepoAgentSession] = useState(false);

  const activeRepoAgent: AgentRun | undefined = repoAgentRuns?.find(
    (r) => r.status === "running" || r.status === "waiting_for_feedback",
  );

  async function handleStartRepoAgent() {
    if (!repoAgentPrompt.trim()) return;
    setStartingRepoAgent(true);
    try {
      await api.startRepoAgent(repoId!, repoAgentPrompt.trim(), newRepoAgentSession);
      setRepoAgentPrompt("");
      setShowAgentPrompt(false);
      setNewRepoAgentSession(false);
      refetchRepoAgentRuns();
    } catch (err) {
      alert(err instanceof Error ? err.message : "Failed to start agent");
    } finally {
      setStartingRepoAgent(false);
    }
  }

  async function handleStopRepoAgent(runId: string) {
    try {
      await api.stopRepoAgent(repoId!, runId);
      refetchRepoAgentRuns();
    } catch (err) {
      alert(err instanceof Error ? err.message : "Failed to stop agent");
    }
  }

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
      repo_agent_started: (_ev: ConductorEventData) => {
        refetchRepoAgentRuns();
      },
      repo_agent_stopped: (_ev: ConductorEventData) => {
        refetchRepoAgentRuns();
      },
      issue_sources_changed: (ev: ConductorEventData) => {
        if (!ev.data || ev.data.repo_id === repoId) refetchSources();
      },
    };
    return map;
  }, [repoId, refetchWorktrees, refetchTickets, refetchRuns, refetchTotals, refetchSources, refetchRepoAgentRuns]);

  useConductorEvents(sseHandlers);

  const navigate = useNavigate();
  const [syncing, setSyncing] = useState(false);
  const [syncResult, setSyncResult] = useState<string | null>(null);
  const [togglingAgentIssues, setTogglingAgentIssues] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [unregisterRepoConfirm, setUnregisterRepoConfirm] = useState(false);
  const [selectedTicket, setSelectedTicket] = useState<Ticket | null>(null);
  const [createWtOpen, setCreateWtOpen] = useState(false);
  const [editingModel, setEditingModel] = useState(false);

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
    await api.unregisterRepo(repoId!);
    setUnregisterRepoConfirm(false);
    refreshRepos();
    window.location.href = "/";
  }

  async function handleModelChange(model: string | null) {
    try {
      await api.setRepoModel(repoId!, model);
      refreshRepos();
    } catch (err) {
      alert(err instanceof Error ? err.message : "Failed to save model");
    }
  }

  async function handleToggleAgentIssues() {
    if (!repo) return;
    setTogglingAgentIssues(true);
    try {
      await api.updateRepoSettings(repoId!, {
        allow_agent_issue_creation: !repo.allow_agent_issue_creation,
      });
      refreshRepos();
    } catch (err) {
      alert(err instanceof Error ? err.message : "Failed to update setting");
    } finally {
      setTogglingAgentIssues(false);
    }
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

  const noModalOpen = !selectedTicket && deleteTarget === null && !unregisterRepoConfirm;

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
        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
          <h2 className="text-xl font-bold text-gray-900">{repo.slug}</h2>
          <button
            onClick={() => setUnregisterRepoConfirm(true)}
            className="sm:self-auto px-3 py-2 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
          >
            Delete Repo
          </button>
        </div>
        <dl className="mt-2 grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-1 text-sm text-gray-600">
          <dt className="font-medium text-gray-500">Remote</dt>
          <dd className="truncate">{repo.remote_url}</dd>
          <dt className="font-medium text-gray-500">Local Path</dt>
          <dd className="truncate">{repo.local_path}</dd>
          <dt className="font-medium text-gray-500">Default Branch</dt>
          <dd>{repo.default_branch}</dd>
          <dt className="font-medium text-gray-500">Model</dt>
          <dd>
            {editingModel ? (
              <div className="mt-1">
                <ModelPicker
                  value={repo.model}
                  onChange={(m) => { handleModelChange(m); setEditingModel(false); }}
                  effectiveDefault={repo.model}
                  effectiveSource="repo"
                />
                <button
                  onClick={() => setEditingModel(false)}
                  className="mt-2 px-2 py-0.5 text-xs rounded border border-gray-300 text-gray-600 hover:bg-gray-50"
                >
                  Cancel
                </button>
              </div>
            ) : (
              <span className="flex items-center gap-2">
                <span className={repo.model ? "" : "text-gray-400"}>
                  {repo.model ?? "Not set"}
                </span>
                <button
                  onClick={() => setEditingModel(true)}
                  className="px-2 py-0.5 text-xs rounded border border-gray-300 text-gray-600 hover:bg-gray-50"
                >
                  Edit
                </button>
              </span>
            )}
          </dd>
          <dt className="font-medium text-gray-500">Agent Issue Creation</dt>
          <dd>
            <button
              onClick={handleToggleAgentIssues}
              disabled={togglingAgentIssues}
              className={`px-2 py-0.5 text-xs rounded border ${
                repo.allow_agent_issue_creation
                  ? "border-green-300 text-green-700 bg-green-50 hover:bg-green-100"
                  : "border-gray-300 text-gray-600 hover:bg-gray-50"
              } disabled:opacity-50`}
            >
              {repo.allow_agent_issue_creation ? "Enabled" : "Disabled"}
            </button>
          </dd>
        </dl>
      </div>

      {/* Repo Agent */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Repo Agent
            <span className="ml-2 text-xs font-normal normal-case text-gray-400">(read-only)</span>
          </h3>
          <div className="flex items-center gap-2">
            {activeRepoAgent && (
              <button
                onClick={() => handleStopRepoAgent(activeRepoAgent.id)}
                className="px-3 py-1.5 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
              >
                Stop Agent
              </button>
            )}
            <button
              onClick={() => setShowAgentPrompt(true)}
              className="px-3 py-1.5 text-sm rounded-md border border-indigo-300 text-indigo-700 bg-indigo-50 hover:bg-indigo-100"
            >
              Ask Agent
            </button>
          </div>
        </div>
        {activeRepoAgent && (
          <div className="mb-3 rounded-lg border border-green-200 bg-green-50 px-4 py-3 text-sm">
            <div className="flex items-center gap-2">
              <span className="inline-block h-2 w-2 rounded-full bg-green-500 animate-pulse" />
              <span className="font-medium text-green-800">Agent running</span>
              <span className="text-green-600 truncate">{activeRepoAgent.prompt.slice(0, 100)}</span>
            </div>
          </div>
        )}
        {repoAgentRuns && repoAgentRuns.length > 0 && !activeRepoAgent && (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">Prompt</th>
                  <th className="px-4 py-2">Status</th>
                  <th className="px-4 py-2">Cost</th>
                  <th className="px-4 py-2">Started</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {repoAgentRuns.slice(0, 5).map((run) => (
                  <tr key={run.id}>
                    <td className="px-4 py-2 truncate max-w-xs">{run.prompt.slice(0, 80)}</td>
                    <td className="px-4 py-2">
                      <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                        run.status === "completed" ? "bg-green-100 text-green-800" :
                        run.status === "failed" ? "bg-red-100 text-red-800" :
                        run.status === "cancelled" ? "bg-gray-100 text-gray-800" :
                        "bg-yellow-100 text-yellow-800"
                      }`}>
                        {run.status}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-gray-500">{run.cost_usd != null ? `$${run.cost_usd.toFixed(2)}` : "-"}</td>
                    <td className="px-4 py-2 text-gray-500">{new Date(run.started_at).toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {/* Agent Prompt Modal */}
      {showAgentPrompt && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-white rounded-lg shadow-xl w-full max-w-lg mx-4">
            <div className="px-6 py-4 border-b">
              <h3 className="text-lg font-semibold">Ask Repo Agent</h3>
              <p className="text-sm text-gray-500 mt-1">
                The agent runs in read-only mode and can explore code, answer questions, and triage issues.
              </p>
            </div>
            <div className="px-6 py-4">
              <textarea
                value={repoAgentPrompt}
                onChange={(e) => setRepoAgentPrompt(e.target.value)}
                placeholder="What would you like the agent to investigate?"
                className="w-full border border-gray-300 rounded-md px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500 min-h-[100px] resize-y"
                autoFocus
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    handleStartRepoAgent();
                  }
                }}
              />
              {repoAgentRuns?.some((r) => r.claude_session_id) && (
                <label className="flex items-center gap-2 mt-2 text-sm text-gray-600">
                  <input
                    type="checkbox"
                    checked={newRepoAgentSession}
                    onChange={(e) => setNewRepoAgentSession(e.target.checked)}
                    className="rounded border-gray-300"
                  />
                  New session (ignore prior context)
                </label>
              )}
            </div>
            <div className="px-6 py-3 border-t flex justify-end gap-2">
              <button
                onClick={() => { setShowAgentPrompt(false); setRepoAgentPrompt(""); setNewRepoAgentSession(false); }}
                className="px-4 py-2 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50"
              >
                Cancel
              </button>
              <button
                onClick={handleStartRepoAgent}
                disabled={startingRepoAgent || !repoAgentPrompt.trim()}
                className="px-4 py-2 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50"
              >
                {startingRepoAgent ? "Starting..." : "Start Agent"}
              </button>
            </div>
          </div>
        </div>
      )}

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
        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2 mb-3">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Worktrees
          </h3>
          <div className="flex flex-wrap items-center gap-3">
            <button
              onClick={() => setShowCompletedWorktrees((v) => !v)}
              className={`px-3 py-2 text-sm rounded-md border ${
                showCompletedWorktrees
                  ? "border-indigo-300 text-indigo-700 bg-indigo-50 hover:bg-indigo-100"
                  : "border-gray-300 text-gray-600 hover:bg-gray-50"
              }`}
            >
              {showCompletedWorktrees ? "Hiding active only" : "Show completed"}
            </button>
            <CreateWorktreeForm repoId={repoId!} onCreated={refetchWorktrees} open={createWtOpen} onOpenChange={setCreateWtOpen} />
          </div>
        </div>
        {wtLoading ? (
          <LoadingSpinner />
        ) : !worktrees || worktrees.length === 0 ? (
          <EmptyState message="No worktrees yet" />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden overflow-x-auto">
            <table className="w-full text-sm min-w-[520px]">
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
              onClick={() => setShowClosedTickets((v) => !v)}
              className={`px-3 py-1.5 text-sm rounded-md border ${
                showClosedTickets
                  ? "border-indigo-300 text-indigo-700 bg-indigo-50 hover:bg-indigo-100"
                  : "border-gray-300 text-gray-600 hover:bg-gray-50"
              }`}
            >
              {showClosedTickets ? "Hiding open only" : "Show closed"}
            </button>
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
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden overflow-x-auto">
            <table className="w-full text-sm min-w-[480px]">
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
        open={unregisterRepoConfirm}
        title="Delete Repo"
        message="Are you sure? This will unregister the repo from Conductor."
        onConfirm={handleDeleteRepo}
        onCancel={() => setUnregisterRepoConfirm(false)}
      />
    </div>
  );
}
