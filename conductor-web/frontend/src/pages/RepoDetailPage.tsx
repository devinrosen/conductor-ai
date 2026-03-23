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
  const [ticketSearch, setTicketSearch] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sortCol, setSortCol] = useState<string | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");
  const [stateFilter, setStateFilter] = useState<Set<string>>(new Set());
  const [labelFilter, setLabelFilter] = useState<Set<string>>(new Set());
  const [assigneeFilter, setAssigneeFilter] = useState<Set<string>>(new Set());
  const [openFilterCol, setOpenFilterCol] = useState<string | null>(null);

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

  // Unique values for column filter dropdowns
  const uniqueStates = useMemo(() => {
    if (!tickets) return [];
    return [...new Set(tickets.map((t) => t.state))].sort();
  }, [tickets]);

  const uniqueLabels = useMemo(() => {
    if (!tickets) return [];
    const all = new Set<string>();
    for (const t of tickets) {
      if (t.labels) t.labels.split(",").forEach((l) => { const trimmed = l.trim(); if (trimmed) all.add(trimmed); });
    }
    return [...all].sort();
  }, [tickets]);

  const uniqueAssignees = useMemo(() => {
    if (!tickets) return [];
    return [...new Set(tickets.map((t) => t.assignee).filter(Boolean) as string[])].sort();
  }, [tickets]);

  // Filter + sort tickets
  const filteredTickets = useMemo(() => {
    if (!tickets) return [];
    let result = tickets;

    // Text search
    const q = ticketSearch.toLowerCase().trim();
    if (q) {
      result = result.filter((t) =>
        t.title.toLowerCase().includes(q) ||
        t.source_id.toLowerCase().includes(q) ||
        (t.labels && t.labels.toLowerCase().includes(q)) ||
        (t.assignee && t.assignee.toLowerCase().includes(q))
      );
    }

    // Column filters
    if (stateFilter.size > 0) {
      result = result.filter((t) => stateFilter.has(t.state));
    }
    if (labelFilter.size > 0) {
      result = result.filter((t) => {
        if (!t.labels) return false;
        const tLabels = t.labels.split(",").map((l) => l.trim());
        return tLabels.some((l) => labelFilter.has(l));
      });
    }
    if (assigneeFilter.size > 0) {
      result = result.filter((t) => t.assignee && assigneeFilter.has(t.assignee));
    }

    // Sort
    if (sortCol) {
      result = [...result].sort((a, b) => {
        let va = "", vb = "";
        switch (sortCol) {
          case "#": va = a.source_id; vb = b.source_id; break;
          case "title": va = a.title; vb = b.title; break;
          case "state": va = a.state; vb = b.state; break;
          case "labels": va = a.labels ?? ""; vb = b.labels ?? ""; break;
          case "assignee": va = a.assignee ?? ""; vb = b.assignee ?? ""; break;
        }
        const cmp = va.localeCompare(vb);
        return sortDir === "asc" ? cmp : -cmp;
      });
    }

    return result;
  }, [tickets, ticketSearch, stateFilter, labelFilter, assigneeFilter, sortCol, sortDir]);

  const toggleSort = useCallback((col: string) => {
    if (sortCol === col) {
      setSortDir((d) => d === "asc" ? "desc" : "asc");
    } else {
      setSortCol(col);
      setSortDir("asc");
    }
  }, [sortCol]);

  const toggleFilterValue = useCallback((setter: React.Dispatch<React.SetStateAction<Set<string>>>, value: string) => {
    setter((prev) => {
      const next = new Set(prev);
      if (next.has(value)) next.delete(value); else next.add(value);
      return next;
    });
  }, []);

  const activeFilterCount = stateFilter.size + labelFilter.size + assigneeFilter.size + (ticketSearch ? 1 : 0);

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
    <div className="flex flex-col h-[calc(100vh-4rem)] overflow-hidden gap-3">
      {/* Compact header: slug + branch + settings toggle */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3 min-w-0">
          <h2 className="text-lg font-bold text-gray-900 truncate">{repo.slug}</h2>
          <span className="text-xs font-mono text-gray-500 shrink-0">{repo.default_branch}</span>
          {repo.model && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-gray-100 text-gray-600 font-mono shrink-0">
              {repo.model}
            </span>
          )}
        </div>
        <button
          onClick={() => setSettingsOpen((v) => !v)}
          className="px-2.5 py-1.5 text-sm rounded-md border border-gray-300 text-gray-600 hover:bg-gray-100 shrink-0"
          title="Repo settings"
        >
          {settingsOpen ? "Close Settings" : "\u2699 Settings"}
        </button>
      </div>

      {/* Collapsible settings panel */}
      {settingsOpen && (
        <div className="rounded-lg border border-gray-200 bg-white p-4 space-y-5">
          {/* Repo info (read-only) */}
          <div>
            <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-2">
              Repository Info
            </h4>
            <dl className="grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-1 text-sm text-gray-600">
              <dt className="font-medium text-gray-500">Remote</dt>
              <dd className="truncate">{repo.remote_url}</dd>
              <dt className="font-medium text-gray-500">Local Path</dt>
              <dd className="truncate">{repo.local_path}</dd>
              <dt className="font-medium text-gray-500">Default Branch</dt>
              <dd>{repo.default_branch}</dd>
            </dl>
          </div>

          {/* Configuration (editable) */}
          <div>
            <h4 className="text-xs font-semibold uppercase tracking-wider text-gray-400 mb-2">
              Configuration
            </h4>
            <div className="space-y-3">
              <div className="flex items-center justify-between">
                <span className="text-sm text-gray-600">Model</span>
                {editingModel ? (
                  <div className="flex items-center gap-2">
                    <ModelPicker
                      value={repo.model}
                      onChange={(m) => { handleModelChange(m); setEditingModel(false); }}
                      effectiveDefault={repo.model}
                      effectiveSource="repo"
                    />
                    <button
                      onClick={() => setEditingModel(false)}
                      className="px-2 py-0.5 text-xs rounded border border-gray-300 text-gray-600 hover:bg-gray-50"
                    >
                      Cancel
                    </button>
                  </div>
                ) : (
                  <button
                    onClick={() => setEditingModel(true)}
                    className="text-sm text-gray-700 hover:text-gray-900"
                  >
                    {repo.model ?? <span className="text-gray-400">Not set</span>} &middot; <span className="text-indigo-600">Edit</span>
                  </button>
                )}
              </div>
              <div className="flex items-center justify-between">
                <span className="text-sm text-gray-600">Agent Issue Creation</span>
                <button
                  onClick={handleToggleAgentIssues}
                  disabled={togglingAgentIssues}
                  className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                    repo.allow_agent_issue_creation ? "bg-green-500" : "bg-gray-300"
                  } disabled:opacity-50`}
                >
                  <span
                    className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                      repo.allow_agent_issue_creation ? "translate-x-4.5" : "translate-x-0.5"
                    }`}
                  />
                </button>
              </div>
            </div>
          </div>

          {/* Issue Sources */}
          <IssueSourcesSection
            repoId={repoId!}
            remoteUrl={repo.remote_url}
            sources={issueSources ?? []}
            loading={sourcesLoading}
            onChanged={refetchSources}
          />

          {/* Danger Zone */}
          <div className="pt-3 border-t border-gray-200">
            <h4 className="text-xs font-semibold uppercase tracking-wider text-red-400 mb-2">
              Danger Zone
            </h4>
            <button
              onClick={() => setUnregisterRepoConfirm(true)}
              className="px-3 py-1.5 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
            >
              Delete Repo
            </button>
          </div>
        </div>
      )}

      {/* Content area — splits remaining space between worktrees and tickets */}
      <div className="flex-1 flex flex-col gap-3 min-h-0 overflow-hidden">

      {/* Worktrees */}
      <section className="flex flex-col min-h-0">
        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2 mb-2">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Worktrees {worktrees ? `(${worktrees.length})` : ""}
          </h3>
          <div className="flex flex-wrap items-center gap-2">
            <button
              onClick={() => setShowCompletedWorktrees((v) => !v)}
              className={`px-3 py-1.5 text-sm rounded-md border ${
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
          <EmptyState message="No platforms active. Create a worktree to lay some track." />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden overflow-y-auto overflow-x-auto flex-1 min-h-0 max-h-[30vh]">
            <table className="w-full text-sm min-w-[520px]">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase sticky top-0 z-10">
                <tr>
                  <th className="px-3 py-1.5">Branch</th>
                  <th className="px-3 py-1.5">Status</th>
                  <th className="px-3 py-1.5">Agent</th>
                  <th className="px-3 py-1.5">Path</th>
                  <th className="px-3 py-1.5">Created</th>
                  <th className="px-3 py-1.5"></th>
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
      <section className="flex flex-col flex-1 min-h-0">
        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2 mb-2">
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400">
            Tickets {tickets ? `(${filteredTickets.length}${activeFilterCount > 0 ? ` of ${tickets.length}` : ""})` : ""}
          </h3>
          <div className="flex items-center gap-2">
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

        {/* Search bar */}
        {tickets && tickets.length > 0 && (
          <div className="mb-2 flex items-center gap-2">
            <input
              type="text"
              value={ticketSearch}
              onChange={(e) => setTicketSearch(e.target.value)}
              placeholder="Search tickets..."
              className="flex-1 px-3 py-1.5 text-sm rounded-md border border-gray-200 bg-white placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-indigo-500"
            />
            {activeFilterCount > 0 && (
              <button
                onClick={() => { setTicketSearch(""); setStateFilter(new Set()); setLabelFilter(new Set()); setAssigneeFilter(new Set()); }}
                className="px-2 py-1.5 text-xs rounded-md text-gray-400 hover:text-gray-600"
              >
                Clear all filters
              </button>
            )}
          </div>
        )}

        {ticketsLoading ? (
          <LoadingSpinner />
        ) : !tickets || tickets.length === 0 ? (
          <EmptyState message="No tickets issued. Sync your issues to start the journey." />
        ) : filteredTickets.length === 0 ? (
          <EmptyState message="No tickets match your filter." />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden overflow-y-auto flex-1 min-h-0">
            <table className="w-full text-sm min-w-[480px]">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase sticky top-0 z-10">
                <tr>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("#")} className="hover:text-gray-800 flex items-center gap-1">
                      # {sortCol === "#" && <span>{sortDir === "asc" ? "\u25B2" : "\u25BC"}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("title")} className="hover:text-gray-800 flex items-center gap-1">
                      Title {sortCol === "title" && <span>{sortDir === "asc" ? "\u25B2" : "\u25BC"}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">
                    <div className="flex items-center gap-1">
                      <button onClick={() => toggleSort("state")} className="hover:text-gray-800 flex items-center gap-1">
                        State {sortCol === "state" && <span>{sortDir === "asc" ? "\u25B2" : "\u25BC"}</span>}
                      </button>
                      <div className="relative">
                        <button
                          onClick={() => setOpenFilterCol(openFilterCol === "state" ? null : "state")}
                          className={`text-[10px] px-1 rounded ${stateFilter.size > 0 ? "text-indigo-500" : "text-gray-400 hover:text-gray-600"}`}
                        >
                          {stateFilter.size > 0 ? `(${stateFilter.size})` : "\u25BE"}
                        </button>
                        {openFilterCol === "state" && (
                          <div className="absolute left-0 top-6 w-36 p-2 rounded-lg border border-gray-200 bg-white shadow-lg z-30 space-y-1">
                            {uniqueStates.map((v) => (
                              <label key={v} className="flex items-center gap-2 text-xs text-gray-700 cursor-pointer hover:bg-gray-50 px-1 py-0.5 rounded">
                                <input type="checkbox" checked={stateFilter.has(v)} onChange={() => toggleFilterValue(setStateFilter, v)} className="rounded" />
                                {v}
                              </label>
                            ))}
                            {stateFilter.size > 0 && (
                              <button onClick={() => setStateFilter(new Set())} className="text-[10px] text-gray-400 hover:text-gray-600 w-full text-left px-1">Clear</button>
                            )}
                          </div>
                        )}
                      </div>
                    </div>
                  </th>
                  <th className="px-3 py-1.5">
                    <div className="flex items-center gap-1">
                      <button onClick={() => toggleSort("labels")} className="hover:text-gray-800 flex items-center gap-1">
                        Labels {sortCol === "labels" && <span>{sortDir === "asc" ? "\u25B2" : "\u25BC"}</span>}
                      </button>
                      <div className="relative">
                        <button
                          onClick={() => setOpenFilterCol(openFilterCol === "labels" ? null : "labels")}
                          className={`text-[10px] px-1 rounded ${labelFilter.size > 0 ? "text-indigo-500" : "text-gray-400 hover:text-gray-600"}`}
                        >
                          {labelFilter.size > 0 ? `(${labelFilter.size})` : "\u25BE"}
                        </button>
                        {openFilterCol === "labels" && (
                          <div className="absolute left-0 top-6 w-44 max-h-48 overflow-y-auto p-2 rounded-lg border border-gray-200 bg-white shadow-lg z-30 space-y-1">
                            {uniqueLabels.map((v) => (
                              <label key={v} className="flex items-center gap-2 text-xs text-gray-700 cursor-pointer hover:bg-gray-50 px-1 py-0.5 rounded">
                                <input type="checkbox" checked={labelFilter.has(v)} onChange={() => toggleFilterValue(setLabelFilter, v)} className="rounded" />
                                {v}
                              </label>
                            ))}
                            {labelFilter.size > 0 && (
                              <button onClick={() => setLabelFilter(new Set())} className="text-[10px] text-gray-400 hover:text-gray-600 w-full text-left px-1">Clear</button>
                            )}
                          </div>
                        )}
                      </div>
                    </div>
                  </th>
                  <th className="px-3 py-1.5">
                    <div className="flex items-center gap-1">
                      <button onClick={() => toggleSort("assignee")} className="hover:text-gray-800 flex items-center gap-1">
                        Assignee {sortCol === "assignee" && <span>{sortDir === "asc" ? "\u25B2" : "\u25BC"}</span>}
                      </button>
                      <div className="relative">
                        <button
                          onClick={() => setOpenFilterCol(openFilterCol === "assignee" ? null : "assignee")}
                          className={`text-[10px] px-1 rounded ${assigneeFilter.size > 0 ? "text-indigo-500" : "text-gray-400 hover:text-gray-600"}`}
                        >
                          {assigneeFilter.size > 0 ? `(${assigneeFilter.size})` : "\u25BE"}
                        </button>
                        {openFilterCol === "assignee" && (
                          <div className="absolute right-0 top-6 w-40 max-h-48 overflow-y-auto p-2 rounded-lg border border-gray-200 bg-white shadow-lg z-30 space-y-1">
                            {uniqueAssignees.map((v) => (
                              <label key={v} className="flex items-center gap-2 text-xs text-gray-700 cursor-pointer hover:bg-gray-50 px-1 py-0.5 rounded">
                                <input type="checkbox" checked={assigneeFilter.has(v)} onChange={() => toggleFilterValue(setAssigneeFilter, v)} className="rounded" />
                                {v}
                              </label>
                            ))}
                            {assigneeFilter.size > 0 && (
                              <button onClick={() => setAssigneeFilter(new Set())} className="text-[10px] text-gray-400 hover:text-gray-600 w-full text-left px-1">Clear</button>
                            )}
                          </div>
                        )}
                      </div>
                    </div>
                  </th>
                  <th className="px-3 py-1.5">Agent</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {filteredTickets.map((t) => (
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

      </div>{/* end content area */}

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
