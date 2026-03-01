import { useState, useEffect, useCallback, useMemo } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import type { AgentRun, AgentEvent, Ticket } from "../api/types";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { AgentPromptModal } from "../components/agents/AgentPromptModal";
import { AgentStatusDisplay } from "../components/agents/AgentStatusDisplay";
import { AgentActivityLog } from "../components/agents/AgentActivityLog";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";

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

  const { data: tickets, refetch: refetchTickets } = useApi(
    () => api.listTickets(repoId!),
    [repoId],
  );

  const [deleteConfirm, setDeleteConfirm] = useState(false);
  const [pathCopied, setPathCopied] = useState(false);
  const [pushing, setPushing] = useState(false);
  const [pushResult, setPushResult] = useState<string | null>(null);
  const [creatingPr, setCreatingPr] = useState(false);
  const [prResult, setPrResult] = useState<string | null>(null);
  const [linkingTicket, setLinkingTicket] = useState(false);
  const [selectedTicketId, setSelectedTicketId] = useState("");

  // Agent state
  const [latestRun, setLatestRun] = useState<AgentRun | null>(null);
  const [agentRuns, setAgentRuns] = useState<AgentRun[]>([]);
  const [agentEvents, setAgentEvents] = useState<AgentEvent[]>([]);
  const [promptModalOpen, setPromptModalOpen] = useState(false);
  const [promptInfo, setPromptInfo] = useState({
    prompt: "",
    resumeSessionId: null as string | null,
  });
  const [agentLoading, setAgentLoading] = useState(false);
  const [stopConfirm, setStopConfirm] = useState(false);

  const worktree = worktrees?.find((w) => w.id === worktreeId);
  const linkedTicket = worktree?.ticket_id
    ? tickets?.find((t) => t.id === worktree.ticket_id)
    : null;

  const isActive = worktree?.status === "active";

  // Tickets available for linking: same repo, not already linked to this worktree
  const availableTickets = tickets?.filter(
    (t: Ticket) => t.id !== worktree?.ticket_id,
  );

  // Fetch agent data
  const refreshAgent = useCallback(async () => {
    if (!worktreeId) return;
    try {
      const [latest, runs, events] = await Promise.all([
        api.latestAgentRun(worktreeId),
        api.listAgentRuns(worktreeId),
        api.getAgentEvents(worktreeId),
      ]);
      setLatestRun(latest);
      setAgentRuns(runs);
      setAgentEvents(events);
    } catch {
      // Silently ignore — agent data may not exist yet
    }
  }, [worktreeId]);

  useEffect(() => {
    refreshAgent();
  }, [refreshAgent]);

  // Poll for updates when agent is running
  useEffect(() => {
    if (latestRun?.status !== "running") return;
    const interval = setInterval(refreshAgent, 5000);
    return () => clearInterval(interval);
  }, [latestRun?.status, refreshAgent]);

  // SSE: auto-refresh worktrees, tickets, and agent data on relevant events
  const sseHandlers = useMemo(() => {
    const handleWorktreeChange = (ev: ConductorEventData) => {
      const d = ev.data;
      if (
        !d ||
        d.worktree_id === worktreeId ||
        d.id === worktreeId ||
        d.repo_id === repoId
      ) {
        refetchWorktrees();
      }
    };
    const handleTickets = (ev: ConductorEventData) => {
      if (!ev.data || ev.data.repo_id === repoId) refetchTickets();
    };
    const handleAgentChange = (ev: ConductorEventData) => {
      if (!ev.data || ev.data.worktree_id === worktreeId) {
        refreshAgent();
      }
    };
    const map: Partial<
      Record<ConductorEventType, (data: ConductorEventData) => void>
    > = {
      worktree_created: handleWorktreeChange,
      worktree_deleted: handleWorktreeChange,
      tickets_synced: handleTickets,
      agent_started: handleAgentChange,
      agent_stopped: handleAgentChange,
      agent_event: handleAgentChange,
    };
    return map;
  }, [repoId, worktreeId, refetchWorktrees, refetchTickets, refreshAgent]);

  useConductorEvents(sseHandlers);

  async function handleLaunchClick() {
    if (!worktreeId) return;
    try {
      const info = await api.getAgentPrompt(worktreeId);
      setPromptInfo({
        prompt: info.prompt,
        resumeSessionId: info.resume_session_id,
      });
      setPromptModalOpen(true);
    } catch {
      // If prompt fetch fails, open modal with empty prompt
      setPromptInfo({ prompt: "", resumeSessionId: null });
      setPromptModalOpen(true);
    }
  }

  async function handleAgentSubmit(prompt: string, resumeSessionId?: string) {
    if (!worktreeId) return;
    setPromptModalOpen(false);
    setAgentLoading(true);
    try {
      await api.startAgent(worktreeId, prompt, resumeSessionId);
      await refreshAgent();
    } catch (e) {
      alert(e instanceof Error ? e.message : "Failed to start agent");
    } finally {
      setAgentLoading(false);
    }
  }

  async function handleStopAgent() {
    if (!worktreeId) return;
    setStopConfirm(false);
    setAgentLoading(true);
    try {
      await api.stopAgent(worktreeId);
      await refreshAgent();
    } catch (e) {
      alert(e instanceof Error ? e.message : "Failed to stop agent");
    } finally {
      setAgentLoading(false);
    }
  }

  async function handleDelete() {
    await api.deleteWorktree(worktreeId!);
    navigate(`/repos/${repoId}`);
  }

  async function handlePush() {
    setPushing(true);
    setPushResult(null);
    try {
      const result = await api.pushWorktree(worktreeId!);
      setPushResult(result.message);
    } catch (err) {
      setPushResult(err instanceof Error ? err.message : "Push failed");
    } finally {
      setPushing(false);
    }
  }

  async function handleCreatePr(draft: boolean) {
    setCreatingPr(true);
    setPrResult(null);
    try {
      const result = await api.createPr(worktreeId!, draft);
      setPrResult(result.url);
    } catch (err) {
      setPrResult(err instanceof Error ? err.message : "PR creation failed");
    } finally {
      setCreatingPr(false);
    }
  }

  async function handleLinkTicket() {
    if (!selectedTicketId) return;
    setLinkingTicket(true);
    try {
      await api.linkTicket(worktreeId!, selectedTicketId);
      setSelectedTicketId("");
      refetchWorktrees();
    } catch (err) {
      setPushResult(err instanceof Error ? err.message : "Link failed");
    } finally {
      setLinkingTicket(false);
    }
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

  const isRunning = latestRun?.status === "running";

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
            <dd className="mt-1 flex items-center gap-2">
              <span className="text-gray-900 truncate">{worktree.path}</span>
              <button
                onClick={() => {
                  navigator.clipboard.writeText(worktree.path);
                  setPathCopied(true);
                  setTimeout(() => setPathCopied(false), 2000);
                }}
                className="shrink-0 px-2 py-0.5 text-xs rounded border border-gray-300 text-gray-600 hover:bg-gray-50"
              >
                {pathCopied ? "Copied!" : "Copy"}
              </button>
            </dd>
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

      {/* Actions — only for active worktrees */}
      {isActive && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Actions
          </h3>
          <div className="rounded-lg border border-gray-200 bg-white p-4 space-y-3">
            <div className="flex items-center gap-2">
              <button
                onClick={handlePush}
                disabled={pushing}
                className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50"
              >
                {pushing ? "Pushing..." : "Push Branch"}
              </button>
              <button
                onClick={() => handleCreatePr(false)}
                disabled={creatingPr}
                className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50"
              >
                {creatingPr ? "Creating..." : "Create PR"}
              </button>
              <button
                onClick={() => handleCreatePr(true)}
                disabled={creatingPr}
                className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50"
              >
                Draft PR
              </button>
            </div>
            {(pushResult || prResult) && (
              <p className="text-xs text-gray-500">
                {prResult ? (
                  prResult.startsWith("http") ? (
                    <a
                      href={prResult}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-indigo-600 hover:underline"
                    >
                      {prResult}
                    </a>
                  ) : (
                    prResult
                  )
                ) : (
                  pushResult
                )}
              </p>
            )}
          </div>
        </section>
      )}

      {/* Linked Ticket */}
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

      {/* Link Ticket — only for active worktrees without a linked ticket */}
      {isActive && !linkedTicket && availableTickets && availableTickets.length > 0 && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Link Ticket
          </h3>
          <div className="rounded-lg border border-gray-200 bg-white p-4">
            <div className="flex items-center gap-3">
              <select
                value={selectedTicketId}
                onChange={(e) => setSelectedTicketId(e.target.value)}
                className="flex-1 rounded-md border border-gray-300 px-3 py-1.5 text-sm"
              >
                <option value="">Select a ticket...</option>
                {availableTickets.map((t: Ticket) => (
                  <option key={t.id} value={t.id}>
                    #{t.source_id} — {t.title}
                  </option>
                ))}
              </select>
              <button
                onClick={handleLinkTicket}
                disabled={!selectedTicketId || linkingTicket}
                className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50 disabled:opacity-50"
              >
                {linkingTicket ? "Linking..." : "Link"}
              </button>
            </div>
          </div>
        </section>
      )}

      {/* Agent Section */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Agent
        </h3>

        {agentLoading && (
          <div className="flex items-center gap-2 text-sm text-gray-500 mb-3">
            <LoadingSpinner />
            <span>Processing...</span>
          </div>
        )}

        {latestRun ? (
          <AgentStatusDisplay
            run={latestRun}
            runs={agentRuns}
            onLaunch={handleLaunchClick}
            onStop={() => setStopConfirm(true)}
          />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white p-4 flex items-center justify-between">
            <p className="text-sm text-gray-500">No agent runs yet</p>
            <button
              onClick={handleLaunchClick}
              disabled={agentLoading}
              className="px-3 py-1.5 text-sm rounded-md bg-indigo-600 text-white hover:bg-indigo-700 disabled:opacity-50"
            >
              Launch Agent
            </button>
          </div>
        )}
      </section>

      {/* Agent Activity Log */}
      {(agentEvents.length > 0 || isRunning) && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Activity Log
          </h3>
          <AgentActivityLog events={agentEvents} isRunning={isRunning} />
        </section>
      )}

      <AgentPromptModal
        open={promptModalOpen}
        title={
          promptInfo.resumeSessionId
            ? "Claude Agent (Resume)"
            : "Claude Agent"
        }
        initialPrompt={promptInfo.prompt}
        resumeSessionId={promptInfo.resumeSessionId}
        onSubmit={handleAgentSubmit}
        onCancel={() => setPromptModalOpen(false)}
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
    </div>
  );
}
