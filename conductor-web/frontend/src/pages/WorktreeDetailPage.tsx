import { useState, useEffect, useCallback, useMemo } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { TransitBreadcrumb } from "../components/shared/TransitBreadcrumb";
import type { AgentRun, AgentEvent, AgentCreatedIssue, Ticket } from "../api/types";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { ErrorBanner } from "../components/shared/ErrorBanner";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { AgentPromptModal } from "../components/agents/AgentPromptModal";
import { isActiveRun } from "../utils/agentStats";
import { ModelPicker } from "../components/shared/ModelPicker";
import { AgentStatusDisplay } from "../components/agents/AgentStatusDisplay";
import { AgentActivityLog } from "../components/agents/AgentActivityLog";
import { AgentPlanChecklist } from "../components/agents/AgentPlanChecklist";
import { AgentFeedbackModal } from "../components/agents/AgentFeedbackModal";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";
import { useHotkeys } from "../hooks/useHotkeys";
import { WorkflowSidebar } from "../components/workflows/WorkflowSidebar";
import { getErrorMessage } from "../utils/errorHandling";
import { getSafeUrl } from "../utils/urlValidation";
import { useRepos } from "../components/layout/AppShell";

export function WorktreeDetailPage() {
  const { repoId, worktreeId } = useParams<{
    repoId: string;
    worktreeId: string;
  }>();
  const navigate = useNavigate();

  const {
    data: worktrees,
    loading,
    refetch: refetchWorktrees,
  } = useApi(() => api.listWorktrees(repoId!), [repoId]);

  const { repos } = useRepos();
  const { data: ticketList, refetch: refetchTickets } = useApi(
    () => api.listTickets(repoId!),
    [repoId],
  );
  const tickets = ticketList?.tickets ?? null;
  const repo = repos?.find((r) => r.id === repoId);

  const [deleteConfirm, setDeleteConfirm] = useState(false);
  const [pathCopied, setPathCopied] = useState(false);
  const [linkingTicket, setLinkingTicket] = useState(false);
  const [selectedTicketId, setSelectedTicketId] = useState("");
  const [editingModel, setEditingModel] = useState(false);

  // Agent state
  const [latestRun, setLatestRun] = useState<AgentRun | null>(null);
  const [agentRuns, setAgentRuns] = useState<AgentRun[]>([]);
  const [childRuns, setChildRuns] = useState<AgentRun[]>([]);
  const [agentEvents, setAgentEvents] = useState<AgentEvent[]>([]);
  const [createdIssues, setCreatedIssues] = useState<AgentCreatedIssue[]>([]);
  const [promptModalOpen, setPromptModalOpen] = useState(false);
  const [promptInfo, setPromptInfo] = useState({
    prompt: "",
    resumeSessionId: null as string | null,
  });
  const [agentLoading, setAgentLoading] = useState(false);
  const [stopConfirm, setStopConfirm] = useState(false);
  const [orchestrateModalOpen, setOrchestrateModalOpen] = useState(false);
  const [feedbackModalOpen, setFeedbackModalOpen] = useState(false);

  // Error state
  const [pageError, setPageError] = useState<{ message: string; retry?: () => void } | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  // Activity log collapsed state
  const [activityExpanded, setActivityExpanded] = useState(false);

  // Sidebar collapsed state
  const [sidebarOpen, setSidebarOpen] = useState(true);

  const noModalsOpen = !deleteConfirm && !promptModalOpen && !stopConfirm && !orchestrateModalOpen;

  useHotkeys([
    { key: "d", handler: () => setDeleteConfirm(true), description: "Delete worktree", enabled: noModalsOpen },
    { key: "l", handler: () => handleLaunchClick(), description: "Launch agent", enabled: noModalsOpen },
    { key: "w", handler: () => setSidebarOpen((v) => !v), description: "Toggle workflows sidebar", enabled: noModalsOpen },
    { key: "c", handler: async () => {
      if (worktree) {
        try {
          await navigator.clipboard.writeText(worktree.path);
          setPathCopied(true);
          setTimeout(() => setPathCopied(false), 2000);
        } catch {
          // Clipboard write failed - don't show copied state
        }
      }
    }, description: "Copy worktree path", enabled: noModalsOpen },
    { key: "a", handler: () => setActivityExpanded((v) => !v), description: "Toggle activity log", enabled: noModalsOpen },
    { key: "Escape", handler: () => navigate(`/repos/${repoId}`), description: "Back to repo", enabled: noModalsOpen },
  ]);

  const worktree = worktrees?.find((w) => w.id === worktreeId);
  const linkedTicket = worktree?.ticket_id
    ? tickets?.find((t) => t.id === worktree.ticket_id)
    : null;

  const isActive = worktree?.status === "active";
  const isRunning = latestRun ? isActiveRun(latestRun) : false;
  const isWaitingForFeedback = latestRun?.status === "waiting_for_feedback";

  useEffect(() => {
    setFeedbackModalOpen(!!isWaitingForFeedback);
  }, [isWaitingForFeedback]);

  const availableTickets = tickets?.filter(
    (t: Ticket) => t.id !== worktree?.ticket_id,
  );

  // Fetch agent data
  const refreshAgent = useCallback(async () => {
    if (!worktreeId) return;
    try {
      const [latest, runs, events, issues] = await Promise.all([
        api.latestAgentRun(worktreeId),
        api.listAgentRuns(worktreeId),
        api.getAgentEvents(worktreeId),
        api.getCreatedIssues(worktreeId),
      ]);
      setLatestRun(latest);
      setAgentRuns(runs);
      setAgentEvents(events);
      setCreatedIssues(issues);
      setPageError(null);

      if (latest && !latest.parent_run_id) {
        try {
          const children = await api.listChildRuns(worktreeId, latest.id);
          setChildRuns(children);
        } catch {
          setChildRuns([]);
        }
      } else {
        setChildRuns([]);
      }
    } catch (e) {
      setPageError({ message: getErrorMessage(e, "Failed to load agent data"), retry: refreshAgent });
    }
  }, [worktreeId]);

  useEffect(() => { refreshAgent(); }, [refreshAgent]);

  useEffect(() => {
    if (!isRunning) return;
    const interval = setInterval(refreshAgent, 5000);
    return () => clearInterval(interval);
  }, [isRunning, refreshAgent]);

  const sseHandlers = useMemo(() => {
    const handleWorktreeChange = (ev: ConductorEventData) => {
      const d = ev.data;
      if (!d || d.worktree_id === worktreeId || d.id === worktreeId || d.repo_id === repoId) {
        refetchWorktrees();
      }
    };
    const handleTickets = (ev: ConductorEventData) => {
      if (!ev.data || ev.data.repo_id === repoId) refetchTickets();
    };
    const handleAgentChange = (ev: ConductorEventData) => {
      if (!ev.data || ev.data.worktree_id === worktreeId) refreshAgent();
    };
    const map: Partial<Record<ConductorEventType, (data: ConductorEventData) => void>> = {
      worktree_created: handleWorktreeChange,
      worktree_deleted: handleWorktreeChange,
      tickets_synced: handleTickets,
      agent_started: handleAgentChange,
      agent_stopped: handleAgentChange,
      agent_event: handleAgentChange,
      feedback_requested: handleAgentChange,
      feedback_submitted: handleAgentChange,
    };
    return map;
  }, [repoId, worktreeId, refetchWorktrees, refetchTickets, refreshAgent]);

  useConductorEvents(sseHandlers);

  // ── Handlers ──

  async function handleLaunchClick() {
    if (!worktreeId) return;
    setPageError(null);
    try {
      const info = await api.getAgentPrompt(worktreeId);
      setPromptInfo({ prompt: info.prompt, resumeSessionId: info.resume_session_id });
      setPromptModalOpen(true);
    } catch (e) {
      const msg = getErrorMessage(e, "Failed to load agent prompt");
      setPageError({ message: msg, retry: handleLaunchClick });
    }
  }

  async function handleAgentSubmit(prompt: string, resumeSessionId?: string) {
    if (!worktreeId) return;
    setPromptModalOpen(false);
    setAgentLoading(true);
    setPageError(null);
    try {
      await api.startAgent(worktreeId, prompt, resumeSessionId);
      await refreshAgent();
    } catch (e) {
      const msg = getErrorMessage(e, "Failed to start agent");
      setPageError({ message: msg, retry: () => handleAgentSubmit(prompt, resumeSessionId) });
    } finally {
      setAgentLoading(false);
    }
  }

  async function handleOrchestrateClick() {
    if (!worktreeId) return;
    try {
      const info = await api.getAgentPrompt(worktreeId);
      setPromptInfo({ prompt: info.prompt, resumeSessionId: null });
      setOrchestrateModalOpen(true);
    } catch {
      setPromptInfo({ prompt: "", resumeSessionId: null });
      setOrchestrateModalOpen(true);
    }
  }

  async function handleOrchestrateSubmit(prompt: string) {
    if (!worktreeId) return;
    setOrchestrateModalOpen(false);
    setAgentLoading(true);
    setPageError(null);
    try {
      await api.orchestrateAgent(worktreeId, prompt);
      await refreshAgent();
    } catch (e) {
      const msg = getErrorMessage(e, "Failed to start orchestration");
      setPageError({ message: msg, retry: () => handleOrchestrateSubmit(prompt) });
    } finally {
      setAgentLoading(false);
    }
  }

  async function handleStopAgent() {
    if (!worktreeId) return;
    setStopConfirm(false);
    setAgentLoading(true);
    setPageError(null);
    try {
      await api.stopAgent(worktreeId);
      await refreshAgent();
    } catch (e) {
      const msg = getErrorMessage(e, "Failed to stop agent");
      setPageError({ message: msg, retry: handleStopAgent });
    } finally {
      setAgentLoading(false);
    }
  }

  async function handleDelete() {
    setDeleteError(null);
    try {
      await api.deleteWorktree(worktreeId!);
      navigate(`/repos/${repoId}`);
    } catch (e) {
      setDeleteError(getErrorMessage(e, "Failed to delete worktree"));
    }
  }

  async function handleLinkTicket() {
    if (!selectedTicketId) return;
    setLinkingTicket(true);
    setPageError(null);
    try {
      await api.linkTicket(worktreeId!, selectedTicketId);
      setSelectedTicketId("");
      refetchWorktrees();
    } catch (err) {
      const msg = getErrorMessage(err, "Failed to link ticket");
      setPageError({ message: msg, retry: handleLinkTicket });
    } finally {
      setLinkingTicket(false);
    }
  }

  async function handleModelChange(model: string | null) {
    setPageError(null);
    try {
      await api.setWorktreeModel(worktreeId!, model);
      refetchWorktrees();
    } catch (err) {
      const msg = getErrorMessage(err, "Failed to save model");
      setPageError({ message: msg });
    }
  }

  if (loading) return <LoadingSpinner />;

  if (!worktree) {
    return (
      <div className="text-center py-12">
        <p className="text-gray-500">Worktree not found</p>
        <Link to={`/repos/${repoId}`} className="text-indigo-600 hover:underline text-sm">
          Back to repo
        </Link>
      </div>
    );
  }

  const shouldCollapseLog = !isRunning && agentEvents.length > 20;
  const showAllEvents = activityExpanded || !shouldCollapseLog;
  const displayEvents = showAllEvents ? agentEvents : agentEvents.slice(-10);

  return (
    <div className="flex flex-col h-full">
      {/* ── Compact Header ── */}
      <div className="shrink-0 space-y-2 mb-3">
        <TransitBreadcrumb stops={[
          { label: "Home", href: "/" },
          { label: repo?.slug ?? "Repo", href: `/repos/${repoId}` },
          { label: worktree.branch, current: true },
        ]} />

        {/* Title row with actions */}
        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <h2 className="text-lg font-bold text-gray-900 truncate">{worktree.branch}</h2>
            <StatusBadge status={worktree.status} />
            {linkedTicket && getSafeUrl(linkedTicket.url) && (
              <a
                href={getSafeUrl(linkedTicket.url)}
                target="_blank"
                rel="noopener noreferrer"
                className="text-xs text-indigo-500 hover:underline shrink-0"
                title={linkedTicket.title}
              >
                #{linkedTicket.source_id}
              </a>
            )}
            {linkedTicket && !getSafeUrl(linkedTicket.url) && (
              <span
                className="text-xs text-gray-500 shrink-0"
                title={`${linkedTicket.title} (unsafe URL)`}
              >
                #{linkedTicket.source_id}
              </span>
            )}
          </div>
          {isActive && (
            <div className="flex items-center gap-2 shrink-0">
              {isRunning ? (
                <button
                  onClick={() => setStopConfirm(true)}
                  disabled={agentLoading}
                  className="px-3 py-1.5 text-sm font-medium rounded-md border border-red-300 text-red-600 hover:bg-red-50 active:scale-95 transition-transform disabled:opacity-50"
                >
                  Stop Agent
                </button>
              ) : (
                <>
                  <button
                    onClick={handleLaunchClick}
                    disabled={agentLoading}
                    className="px-3 py-1.5 text-sm font-medium rounded-md bg-indigo-600 text-white hover:bg-indigo-700 hover:brightness-110 active:scale-95 transition-transform disabled:opacity-50"
                  >
                    Launch Agent
                  </button>
                  <button
                    onClick={handleOrchestrateClick}
                    disabled={agentLoading}
                    className="px-3 py-1.5 text-sm font-medium rounded-md border border-indigo-300 text-indigo-700 hover:bg-indigo-50 active:scale-95 transition-transform disabled:opacity-50"
                  >
                    Orchestrate
                  </button>
                </>
              )}
            </div>
          )}
        </div>

        {/* Stats bar — compact inline chips */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-gray-500">
          <span className="flex items-center gap-1.5" title={worktree.path}>
            <button
              onClick={() => {
                navigator.clipboard.writeText(worktree.path);
                setPathCopied(true);
                setTimeout(() => setPathCopied(false), 2000);
              }}
              className="text-gray-600 hover:text-gray-800 underline decoration-dotted"
            >
              {pathCopied ? "Copied!" : "Path"}
            </button>
          </span>
          <span>Created <TimeAgo date={worktree.created_at} /></span>
          {worktree.completed_at && <span>Completed <TimeAgo date={worktree.completed_at} /></span>}
          {isActive && !linkedTicket && availableTickets && availableTickets.length > 0 && (
            <span className="flex items-center gap-1">
              <select
                value={selectedTicketId}
                onChange={(e) => setSelectedTicketId(e.target.value)}
                aria-label="Select a ticket to link"
                className="rounded border border-gray-300 bg-white text-gray-900 px-1 py-0.5 text-xs"
              >
                <option value="">Link ticket...</option>
                {availableTickets.map((t: Ticket) => (
                  <option key={t.id} value={t.id}>#{t.source_id}</option>
                ))}
              </select>
              <button
                onClick={handleLinkTicket}
                disabled={!selectedTicketId || linkingTicket}
                className="px-1.5 py-0.5 text-xs rounded border border-gray-300 text-gray-700 hover:bg-gray-50 active:scale-95 transition-transform disabled:opacity-50"
              >
                {linkingTicket ? "..." : "Link"}
              </button>
            </span>
          )}
          <span className="flex items-center gap-1">
            Model:
            {editingModel ? (
              <span className="inline-flex items-center gap-1">
                <ModelPicker
                  value={worktree.model}
                  onChange={(m) => { handleModelChange(m); setEditingModel(false); }}
                  effectiveDefault={worktree.model}
                  effectiveSource="worktree"
                />
                <button onClick={() => setEditingModel(false)} className="text-gray-500 hover:text-gray-700">&times;</button>
              </span>
            ) : (
              <button
                onClick={() => setEditingModel(true)}
                className="text-gray-700 hover:text-gray-900 underline decoration-dotted"
              >
                {worktree.model ?? "not set"}
              </button>
            )}
          </span>
        </div>

        <ErrorBanner error={pageError?.message ?? null} onDismiss={() => setPageError(null)} onRetry={pageError?.retry} />
      </div>

      {/* ── Two-Pane Layout ── */}
      <div className="flex gap-3 flex-1 min-h-0">
        {/* Main area — agent content */}
        <div className="flex-1 min-w-0 flex flex-col gap-3 overflow-y-auto">
          {/* Agent status + plan combined */}
          {agentLoading && (
            <div className="flex items-center gap-2 text-sm text-gray-500">
              <LoadingSpinner />
              <span>Processing...</span>
            </div>
          )}

          {latestRun ? (
            <AgentStatusDisplay
              run={latestRun}
              runs={agentRuns}
              childRuns={childRuns}
            />
          ) : (
            <div className="rounded-lg border border-gray-200 bg-white p-3 text-sm text-gray-500">
              No agent runs yet — use <strong>Launch Agent</strong> to start.
            </div>
          )}

          {latestRun?.plan && latestRun.plan.length > 0 && (
            <AgentPlanChecklist steps={latestRun.plan} />
          )}

          {/* Activity Log — hero element */}
          {(agentEvents.length > 0 || isRunning) && (
            <div className="flex flex-col flex-1 min-h-0">
              <div className="flex items-center justify-between mb-1.5">
                <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-500">
                  Activity Log
                  <span className="ml-1.5 font-normal normal-case text-gray-600">
                    ({agentEvents.length})
                  </span>
                </h3>
                {shouldCollapseLog && (
                  <button
                    onClick={() => setActivityExpanded(!activityExpanded)}
                    className="text-xs text-indigo-600 hover:text-indigo-700"
                  >
                    {activityExpanded ? "Collapse" : `Show all ${agentEvents.length}`}
                  </button>
                )}
              </div>
              <AgentActivityLog events={displayEvents} runs={agentRuns} isRunning={isRunning} />
            </div>
          )}

          {/* Issues created */}
          {createdIssues.length > 0 && (
            <div>
              <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-500 mb-1.5">
                Issues Created
              </h3>
              <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
                <ul className="divide-y divide-gray-100">
                  {createdIssues.map((issue) => (
                    <li key={issue.id} className="px-3 py-2 flex items-center gap-2">
                      <span className="text-xs font-mono text-gray-400">#{issue.source_id}</span>
                      <a
                        href={issue.url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-sm text-indigo-600 hover:underline flex-1 truncate"
                      >
                        {issue.title}
                      </a>
                    </li>
                  ))}
                </ul>
              </div>
            </div>
          )}

          {/* Danger Zone */}
          <details className="mt-2">
            <summary className="text-xs font-semibold uppercase tracking-wider text-red-400 cursor-pointer select-none list-none flex items-center gap-1">
              <span className="text-[10px] transition-transform [[open]>&]:rotate-90">&#9654;</span>
              Danger Zone
            </summary>
            <div className="rounded-lg border danger-border bg-white p-3 flex flex-col gap-2 mt-1.5">
              <div className="flex items-center justify-between">
                <div>
                  <p className="text-sm font-medium text-gray-900">Delete this worktree</p>
                  <p className="text-xs text-gray-500">Remove the worktree and its git branch. This cannot be undone.</p>
                </div>
                <button
                  onClick={() => setDeleteConfirm(true)}
                  className="px-3 py-1.5 text-sm rounded-md border border-red-300 text-red-600 hover:bg-red-50 active:scale-95 transition-transform focus-visible:ring-2 focus-visible:ring-red-500"
                >
                  Delete
                </button>
              </div>
              <ErrorBanner error={deleteError} onDismiss={() => setDeleteError(null)} onRetry={handleDelete} />
            </div>
          </details>
        </div>

        {/* ── Workflow Sidebar ── */}
        {worktreeId && repoId && (
          <div
            className={`shrink-0 border-l border-gray-200 bg-gray-900 rounded-lg transition-all duration-200 overflow-hidden hidden lg:block ${
              sidebarOpen ? "w-72" : "w-10"
            }`}
          >
            {sidebarOpen ? (
              <div className="flex flex-col h-full">
                <div className="flex items-center justify-between px-3 pt-2 pb-1">
                  <span className="text-xs font-semibold uppercase tracking-wider text-gray-500">Workflows</span>
                  <button
                    onClick={() => setSidebarOpen(false)}
                    className="text-gray-500 hover:text-gray-300 text-sm"
                    title="Hide sidebar (W)"
                  >
                    &raquo;
                  </button>
                </div>
                <WorkflowSidebar repoId={repoId} worktreeId={worktreeId} ticketId={worktree.ticket_id ?? undefined} />
              </div>
            ) : (
              <button
                onClick={() => setSidebarOpen(true)}
                className="w-full h-full flex items-center justify-center text-gray-500 hover:text-gray-300"
                title="Show workflows (W)"
              >
                <span className="writing-mode-vertical text-xs font-semibold uppercase tracking-widest"
                  style={{ writingMode: "vertical-rl" }}>
                  Workflows
                </span>
              </button>
            )}
          </div>
        )}
      </div>

      {/* ── Modals ── */}
      <AgentPromptModal
        open={promptModalOpen}
        title={promptInfo.resumeSessionId ? "Claude Agent (Resume)" : "Claude Agent"}
        initialPrompt={promptInfo.prompt}
        resumeSessionId={promptInfo.resumeSessionId}
        onSubmit={handleAgentSubmit}
        onCancel={() => setPromptModalOpen(false)}
      />

      <AgentPromptModal
        open={orchestrateModalOpen}
        title="Orchestrate (Multi-Step)"
        initialPrompt={promptInfo.prompt}
        resumeSessionId={null}
        onSubmit={(prompt) => handleOrchestrateSubmit(prompt)}
        onCancel={() => setOrchestrateModalOpen(false)}
      />

      <ConfirmDialog
        open={stopConfirm}
        title="Stop Agent"
        message="Are you sure you want to stop the running agent? The tmux session will be killed and the run marked as cancelled."
        onConfirm={handleStopAgent}
        onCancel={() => setStopConfirm(false)}
      />

      <ConfirmDialog
        open={deleteConfirm}
        title="Delete Worktree"
        message="Are you sure? This will remove the worktree and its git branch."
        onConfirm={handleDelete}
        onCancel={() => setDeleteConfirm(false)}
      />

      {worktreeId && (
        <AgentFeedbackModal
          worktreeId={worktreeId}
          open={feedbackModalOpen}
          onClose={() => setFeedbackModalOpen(false)}
          onSubmitted={() => {
            setFeedbackModalOpen(false);
            refreshAgent();
          }}
        />
      )}
    </div>
  );
}
