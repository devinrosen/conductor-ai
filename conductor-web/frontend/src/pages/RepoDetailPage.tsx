import { useState, useMemo, useCallback } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { AgentRun, Ticket, WorkflowRun } from "../api/types";
import { WorktreeRow } from "../components/worktrees/WorktreeRow";
import { CreateWorktreeForm } from "../components/worktrees/CreateWorktreeForm";
import { TicketRow } from "../components/tickets/TicketRow";
import { TicketCard } from "../components/tickets/TicketCard";
import { RepoAgentRunCard } from "../components/agents/RepoAgentRunCard";
import { TicketDetailModal } from "../components/tickets/TicketDetailModal";
import { IssueSourcesSection } from "../components/issue-sources/IssueSourcesSection";
import { StatusBadge } from "../components/shared/StatusBadge";
import { ColumnHeader, type SortDirection } from "../components/shared/ColumnHeader";
import { parseLabels } from "../utils/ticketUtils";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { ModelPicker } from "../components/shared/ModelPicker";
import { buildTicketTree } from "../utils/ticketDeps";
import { deriveWorktreeSlug } from "../utils/worktreeUtils";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";
import { useHotkeys } from "../hooks/useHotkeys";
import { OnboardingHint, useOnboardingHighlight } from "../components/shared/OnboardingHint";
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

  const { data: prs } = useApi(() => api.listPrs(repoId!), [repoId]);

  const { refetch: refetchRuns } = useApi(
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

  const hasVantage = issueSources?.some((s) => s.source_type === "vantage") ?? false;
  const allVantage = (issueSources?.length ?? 0) > 0 && issueSources!.every((s) => s.source_type === "vantage");

  // Ticket table sort/filter state
  type TicketSortColumn = "source_id" | "title" | "state" | "assignee" | "pipeline" | null;
  const [ticketSortColumn, setTicketSortColumn] = useState<TicketSortColumn>(null);
  const [ticketSortDir, setTicketSortDir] = useState<SortDirection>(null);
  const [ticketColumnFilters, setTicketColumnFilters] = useState<Record<string, Set<string>>>({});

  function getTicketPipelineStatus(ticket: Ticket): string {
    try {
      return JSON.parse(ticket.raw_json)?.conductor?.status ?? "";
    } catch {
      return "";
    }
  }

  const ticketFilterOptions = useMemo(() => {
    if (!tickets) return {};
    const states = new Set<string>();
    const assignees = new Set<string>();
    const labels = new Set<string>();
    const pipelines = new Set<string>();
    for (const t of tickets) {
      states.add(t.state);
      if (t.assignee) assignees.add(t.assignee);
      for (const l of parseLabels(t.labels)) labels.add(l);
      const ps = getTicketPipelineStatus(t);
      if (ps) pipelines.add(ps);
    }
    return {
      state: Array.from(states).sort(),
      assignee: Array.from(assignees).sort(),
      labels: Array.from(labels).sort(),
      pipeline: Array.from(pipelines).sort(),
    };
  }, [tickets]);

  const sortedFilteredTickets = useMemo(() => {
    if (!tickets) return [];
    let result = [...tickets];

    // Column filters
    for (const [col, values] of Object.entries(ticketColumnFilters)) {
      if (values.size === 0) continue;
      result = result.filter((t) => {
        switch (col) {
          case "state": return values.has(t.state);
          case "assignee": return values.has(t.assignee ?? "");
          case "labels": return parseLabels(t.labels).some((l) => values.has(l));
          case "pipeline": return values.has(getTicketPipelineStatus(t));
          default: return true;
        }
      });
    }

    // Sort
    if (ticketSortColumn && ticketSortDir) {
      const dir = ticketSortDir === "asc" ? 1 : -1;
      result.sort((a, b) => {
        let va = "";
        let vb = "";
        switch (ticketSortColumn) {
          case "source_id": va = a.source_id; vb = b.source_id; break;
          case "title": va = a.title; vb = b.title; break;
          case "state": va = a.state; vb = b.state; break;
          case "assignee": va = a.assignee ?? ""; vb = b.assignee ?? ""; break;
          case "pipeline": va = getTicketPipelineStatus(a); vb = getTicketPipelineStatus(b); break;
        }
        return va.localeCompare(vb) * dir;
      });
    }

    return result;
  }, [tickets, ticketColumnFilters, ticketSortColumn, ticketSortDir]);

  function handleTicketSort(col: string, dir: SortDirection) {
    setTicketSortColumn(dir ? (col as TicketSortColumn) : null);
    setTicketSortDir(dir);
  }

  function handleTicketFilter(col: string, values: Set<string>) {
    setTicketColumnFilters((prev) => ({ ...prev, [col]: values }));
  }

  function ticketSortDirFor(col: string): SortDirection {
    return ticketSortColumn === col ? ticketSortDir : null;
  }

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
      setActionError(err instanceof Error ? err.message : "Failed to start agent");
    } finally {
      setStartingRepoAgent(false);
    }
  }

  async function handleStopRepoAgent(runId: string) {
    try {
      await api.stopRepoAgent(repoId!, runId);
      refetchRepoAgentRuns();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : "Failed to stop agent");
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
  const [deleting, setDeleting] = useState(false);
  const [unregisterRepoConfirm, setUnregisterRepoConfirm] = useState(false);
  const [selectedTicket, setSelectedTicket] = useState<Ticket | null>(null);
  const [collapsedNodes, setCollapsedNodes] = useState<Set<string>>(new Set());

  const toggleCollapse = useCallback((sourceId: string) => {
    setCollapsedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(sourceId)) next.delete(sourceId);
      else next.add(sourceId);
      return next;
    });
  }, []);
  const [createWtOpen, setCreateWtOpen] = useState(false);
  const [editingModel, setEditingModel] = useState(false);
  const highlightIssues = useOnboardingHighlight("issue-sources");
  const [settingsOpen, setSettingsOpen] = useState(highlightIssues);
  const [startingWorkflow, setStartingWorkflow] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // Fetch active workflow runs for this repo to show running indicators on tickets
  const { data: activeWorkflowRuns, refetch: refetchWorkflowRuns } = useApi(
    () => api.listAllWorkflowRuns(["running", "pending", "waiting", "failed"]),
    [],
  );

  // Build ticket dependency tree
  const ticketTree = useMemo(
    () => (tickets ? buildTicketTree(tickets, worktrees ?? undefined, prs ?? undefined) : null),
    [tickets, worktrees, prs],
  );

  // Map ticket internal id → source_id (e.g. "D-160") for worktree display
  const ticketSourceIdMap = useMemo(() => {
    const m = new Map<string, string>();
    for (const t of tickets ?? []) m.set(t.id, t.source_id);
    return m;
  }, [tickets]);

  // Set of ticket IDs that already have a worktree
  const ticketsWithWorktree = useMemo(() => {
    const s = new Set<string>();
    for (const wt of worktrees ?? []) {
      if (wt.ticket_id) s.add(wt.ticket_id);
    }
    return s;
  }, [worktrees]);

  // Map worktree_id -> workflow run (for both worktree table and ticket indicators)
  const workflowRunByWorktreeId = useMemo(() => {
    const m = new Map<string, WorkflowRun>();
    if (!activeWorkflowRuns) return m;
    for (const run of activeWorkflowRuns) {
      // Only top-level runs (no parent workflow), scoped to this repo
      if (run.repo_id === repoId && run.worktree_id && !run.parent_workflow_run_id) {
        m.set(run.worktree_id, run);
      }
    }
    return m;
  }, [activeWorkflowRuns, repoId]);

  // Map ticket source_id -> workflow status (via worktree linkage)
  const workflowStatusByTicketSourceId = useMemo(() => {
    const m = new Map<string, WorkflowRun["status"]>();
    if (!worktrees || !workflowRunByWorktreeId.size) return m;
    for (const wt of worktrees) {
      const run = workflowRunByWorktreeId.get(wt.id);
      if (!run) continue;
      if (tickets && wt.ticket_id) {
        const ticket = tickets.find((t) => t.id === wt.ticket_id);
        if (ticket) {
          m.set(ticket.source_id, run.status);
        }
      }
    }
    return m;
  }, [worktrees, workflowRunByWorktreeId, tickets]);

  async function handleStartTicketToPr(ticket: Ticket) {
    if (startingWorkflow) return;
    setStartingWorkflow(ticket.id);
    try {
      const wtName = deriveWorktreeSlug(ticket.source_id, ticket.title);
      const wt = await api.createWorktree(repoId!, {
        name: wtName,
        ticket_id: ticket.id,
      });
      const result = await api.runWorkflow(wt.id, {
        name: "ticket-to-pr",
        inputs: {
          ticket_id: ticket.source_id,
          qualify: "true",
          auto_merge: "false",
        },
      });
      refetchWorktrees();
      refetchWorkflowRuns();

      // Poll for early failure — workflows that fail during init (schema validation,
      // missing agents, etc.) complete before the first poll would normally catch them.
      if (result.run_id) {
        setTimeout(async () => {
          try {
            const run = await api.getWorkflowRun(result.run_id);
            if (run?.status === "failed") {
              setActionError(
                `Workflow failed: ${run.result_summary || "unknown error"}`,
              );
              refetchWorkflowRuns();
            }
          } catch {
            // Ignore poll errors
          }
        }, 3000);
      }
    } catch (err) {
      setActionError(err instanceof Error ? err.message : "Failed to start workflow");
    } finally {
      setStartingWorkflow(null);
    }
  }

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
    setDeleting(true);
    try {
      await api.deleteWorktree(deleteTarget);
      setDeleteTarget(null);
      refetchWorktrees();
    } catch {
      setDeleteTarget(null);
    } finally {
      setDeleting(false);
    }
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
      setActionError(err instanceof Error ? err.message : "Failed to save model");
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
      setActionError(err instanceof Error ? err.message : "Failed to update setting");
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

  function renderTicketRows(ticketList: Ticket[], depth: number): React.ReactNode[] {
    const rows: React.ReactNode[] = [];
    for (const t of ticketList) {
      const children = ticketTree?.childMap.get(t.source_id);
      const hasChildren = !!children && children.length > 0;
      const isCollapsed = collapsedNodes.has(t.source_id);
      const wfStatus = workflowStatusByTicketSourceId.get(t.source_id) as "running" | "pending" | "waiting" | "failed" | "completed" | undefined;
      rows.push(
        <TicketRow
          key={`${t.id}-d${depth}`}
          ticket={t}
          agentTotals={ticketTotals?.[t.id]}
          onClick={setSelectedTicket}
          depth={depth}
          blocked={ticketTree?.blocked.has(t.id) ?? false}
          unlocked={ticketTree?.unlocked.has(t.id) ?? false}
          workflowStatus={wfStatus ?? null}
          onStartWorkflow={handleStartTicketToPr}
          showPipeline={hasVantage}
          hideStateAndLabels={allVantage}
          hasChildren={hasChildren}
          collapsed={isCollapsed}
          onToggleCollapse={toggleCollapse}
          hasWorktree={ticketsWithWorktree.has(t.id)}
        />,
      );
      if (hasChildren && !isCollapsed) {
        rows.push(...renderTicketRows(children, depth + 1));
      }
    }
    return rows;
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
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold text-gray-900">{repo.slug}</h2>
        <button
          onClick={() => setSettingsOpen(!settingsOpen)}
          className={`p-2 rounded-md transition-colors ${settingsOpen ? "bg-gray-100 text-gray-700" : "text-gray-400 hover:text-gray-600 hover:bg-gray-50"}`}
          title="Settings"
        >
          <svg className="w-5 h-5" viewBox="0 0 20 20" fill="currentColor">
            <path fillRule="evenodd" d="M7.84 1.804A1 1 0 0 1 8.82 1h2.36a1 1 0 0 1 .98.804l.331 1.652a6.993 6.993 0 0 1 1.929 1.115l1.598-.54a1 1 0 0 1 1.186.447l1.18 2.044a1 1 0 0 1-.205 1.251l-1.267 1.113a7.047 7.047 0 0 1 0 2.228l1.267 1.113a1 1 0 0 1 .206 1.25l-1.18 2.045a1 1 0 0 1-1.187.447l-1.598-.54a6.993 6.993 0 0 1-1.929 1.115l-.33 1.652a1 1 0 0 1-.98.804H8.82a1 1 0 0 1-.98-.804l-.331-1.652a6.993 6.993 0 0 1-1.929-1.115l-1.598.54a1 1 0 0 1-1.186-.447l-1.18-2.044a1 1 0 0 1 .205-1.251l1.267-1.114a7.05 7.05 0 0 1 0-2.227L1.821 7.773a1 1 0 0 1-.206-1.25l1.18-2.045a1 1 0 0 1 1.187-.447l1.598.54A6.993 6.993 0 0 1 7.51 3.456l.33-1.652ZM10 13a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z" clipRule="evenodd" />
          </svg>
        </button>
      </div>

      {actionError && (
        <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700 flex items-center justify-between">
          <span>{actionError}</span>
          <button onClick={() => setActionError(null)} className="text-red-400 hover:text-red-600 ml-2">&times;</button>
        </div>
      )}

      {/* Settings (collapsible) */}
      {settingsOpen && (
      <div className="rounded-lg border border-gray-200 p-5 space-y-5">
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
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

      <hr className="border-gray-200" />

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
          <>
            <div className="hidden md:block rounded-lg border border-gray-200 bg-white overflow-hidden">
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
                        <StatusBadge status={run.status} />
                      </td>
                      <td className="px-4 py-2 text-gray-500">{run.cost_usd != null ? `$${run.cost_usd.toFixed(2)}` : "-"}</td>
                      <td className="px-4 py-2 text-gray-500">{new Date(run.started_at).toLocaleString()}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="md:hidden space-y-2">
              {repoAgentRuns.slice(0, 5).map((run) => (
                <RepoAgentRunCard key={run.id} run={run} />
              ))}
            </div>
          </>
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

      <hr className="border-gray-200" />

      {/* Issue Sources */}
      <OnboardingHint target="issue-sources" label="Add an issue source here">
        <IssueSourcesSection
          repoId={repoId!}
          remoteUrl={repo.remote_url}
          sources={issueSources ?? []}
          loading={sourcesLoading}
          onChanged={refetchSources}
        />
      </OnboardingHint>

      <hr className="border-gray-200" />

      {/* Danger Zone */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-red-400 mb-3">
          Danger Zone
        </h3>
        <div className="rounded-lg border border-red-200 bg-white p-4 flex items-center justify-between">
          <div>
            <p className="text-sm font-medium text-gray-900">Delete this repo</p>
            <p className="text-xs text-gray-500 mt-0.5">Unregister this repo from Conductor. This cannot be undone.</p>
          </div>
          <button
            onClick={() => setUnregisterRepoConfirm(true)}
            className="px-3 py-2 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50"
          >
            Delete Repo
          </button>
        </div>
      </section>
      </div>
      )}

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
            <OnboardingHint target="create-worktree" label="Start here">
              <CreateWorktreeForm repoId={repoId!} onCreated={refetchWorktrees} open={createWtOpen} onOpenChange={setCreateWtOpen} />
            </OnboardingHint>
          </div>
        </div>
        {wtLoading ? (
          <LoadingSpinner />
        ) : !worktrees || worktrees.length === 0 ? (
          <EmptyState message="No platforms active. Create a worktree to lay some track." />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden overflow-x-auto">
            <table className="w-full text-sm min-w-[520px]">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">Branch</th>
                  <th className="px-4 py-2">Ticket</th>
                  <th className="px-4 py-2">Status</th>
                  <th className="px-4 py-2">Workflow</th>
                  <th className="px-4 py-2">Created</th>
                  <th className="px-4 py-2"></th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {worktrees.map((wt, index) => (
                  <WorktreeRow
                    key={wt.id}
                    worktree={wt}
                    workflowRun={workflowRunByWorktreeId.get(wt.id)}
                    onDelete={setDeleteTarget}
                    selected={index === selectedIndex}
                    index={index}
                    ticketSourceId={wt.ticket_id ? ticketSourceIdMap.get(wt.ticket_id) : null}
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
          {issueSources && issueSources.length > 0 ? (
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
          ) : (
            <button
              onClick={() => setSettingsOpen(true)}
              className="px-3 py-1.5 text-sm rounded-md border border-indigo-300 text-indigo-600 hover:bg-indigo-50"
            >
              Configure Issue Sources
            </button>
          )}
        </div>
        {ticketsLoading ? (
          <LoadingSpinner />
        ) : !issueSources || issueSources.length === 0 ? (
          <EmptyState message="No issue sources configured. Add one in Settings to sync tickets." />
        ) : !tickets || tickets.length === 0 ? (
          <EmptyState message="No tickets issued. Sync your issues to start the journey." />
        ) : (
          <>
            <div className="hidden md:block rounded-lg border border-gray-200 bg-white overflow-hidden overflow-x-auto">
              <table className="w-full text-sm min-w-[480px]">
                <thead className="bg-gray-50 text-left text-gray-500">
                  <tr>
                    <th className="px-4 py-2 text-xs font-medium uppercase">#</th>
                    <th className="px-4 py-2 text-xs font-medium uppercase">Title</th>
                    {!allVantage && <ColumnHeader label="State" columnKey="state" sortDirection={ticketSortDirFor("state")} onSort={handleTicketSort} filterOptions={ticketFilterOptions.state} activeFilters={ticketColumnFilters.state} onFilter={handleTicketFilter} />}
                    {!allVantage && <ColumnHeader label="Labels" columnKey="labels" sortDirection={null} onSort={() => {}} filterOptions={ticketFilterOptions.labels} activeFilters={ticketColumnFilters.labels} onFilter={handleTicketFilter} />}
                    <ColumnHeader label="Assignee" columnKey="assignee" sortDirection={ticketSortDirFor("assignee")} onSort={handleTicketSort} filterOptions={ticketFilterOptions.assignee} activeFilters={ticketColumnFilters.assignee} onFilter={handleTicketFilter} />
                    {hasVantage && <ColumnHeader label="Pipeline" columnKey="pipeline" sortDirection={ticketSortDirFor("pipeline")} onSort={handleTicketSort} filterOptions={ticketFilterOptions.pipeline} activeFilters={ticketColumnFilters.pipeline} onFilter={handleTicketFilter} />}
                    <th className="px-4 py-2 text-xs font-medium uppercase">Agent</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-gray-100">
                  {ticketTree ? renderTicketRows(ticketTree.roots, 0) : sortedFilteredTickets.map((t) => (
                    <TicketRow
                      key={t.id}
                      ticket={t}
                      agentTotals={ticketTotals?.[t.id]}
                      onClick={setSelectedTicket}
                      showPipeline={hasVantage}
                      hideStateAndLabels={allVantage}
                    />
                  ))}
                </tbody>
              </table>
            </div>
            <div className="md:hidden space-y-2">
              {tickets.map((t) => (
                <TicketCard
                  key={t.id}
                  ticket={t}
                  agentTotals={ticketTotals?.[t.id]}
                  onClick={setSelectedTicket}
                />
              ))}
            </div>
          </>
        )}
      </section>

      {/* Error Banner */}
      {actionError && (
        <div className="px-3 py-2 text-sm text-red-700 bg-red-50 rounded-md border border-red-200 flex items-center justify-between">
          <span>{actionError}</span>
          <button onClick={() => setActionError(null)} className="text-red-500 hover:text-red-700 text-xs ml-2">Dismiss</button>
        </div>
      )}

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
        loading={deleting}
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
