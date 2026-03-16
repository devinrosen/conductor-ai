import { useEffect, useState, useCallback, useMemo } from "react";
import { Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { api } from "../api/client";
import type { AgentRun, WorkflowRun, FeedbackRequest } from "../api/types";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { agentStatusColor } from "../utils/agentStats";
import { WorkflowRunTree } from "../components/workflows/WorkflowRunTree";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";

function ErrorBanner({ message }: { message: string }) {
  return (
    <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">
      {message}
    </div>
  );
}

interface WorktreeContext {
  repoId: string;
  repoSlug: string;
  branch: string;
  worktreeId: string;
}

interface ActivityData {
  pendingFeedback: { feedback: FeedbackRequest; ctx: WorktreeContext }[];
  activeAgentRuns: { run: AgentRun; ctx: WorktreeContext }[];
  activeWorkflowRuns: WorkflowRun[];
}

export function ActivityPage() {
  const { repos, loading: reposLoading } = useRepos();
  const [activity, setActivity] = useState<ActivityData>({
    pendingFeedback: [],
    activeAgentRuns: [],
    activeWorkflowRuns: [],
  });
  const [ctxMap, setCtxMap] = useState<Map<string, WorktreeContext>>(new Map());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [feedbackText, setFeedbackText] = useState<Record<string, string>>({});

  const refresh = useCallback(() => setTick((n) => n + 1), []);

  useEffect(() => {
    if (repos.length === 0) {
      setLoading(false);
      return;
    }

    const fetchAll = async () => {
      // Fetch worktree listings, latest agent runs, and all active workflow runs in parallel
      const [repoWorktrees, latestRuns, activeWorkflowRuns] = await Promise.all([
        Promise.all(
          repos.map((r) =>
            api.listWorktrees(r.id).then((wts) => ({ repo: r, wts })),
          ),
        ),
        api.latestRunsByWorktree(),
        api.listAllWorkflowRuns(),
      ]);

      const ctxMap = new Map<string, WorktreeContext>();
      for (const { repo, wts } of repoWorktrees) {
        for (const wt of wts) {
          if (wt.status === "active") {
            ctxMap.set(wt.id, {
              repoId: repo.id,
              repoSlug: repo.slug,
              branch: wt.branch,
              worktreeId: wt.id,
            });
          }
        }
      }

      const activeWorktreeIds = Array.from(ctxMap.keys());

      const activeAgentRuns: { run: AgentRun; ctx: WorktreeContext }[] = [];
      const feedbackWorktreeIds: string[] = [];

      for (const wtId of activeWorktreeIds) {
        const run = (latestRuns as Record<string, AgentRun>)[wtId];
        if (!run) continue;
        const ctx = ctxMap.get(wtId)!;
        if (run.status === "running" || run.status === "waiting_for_feedback") {
          activeAgentRuns.push({ run, ctx });
        }
        if (run.status === "waiting_for_feedback") {
          feedbackWorktreeIds.push(wtId);
        }
      }

      const feedbackResults = await Promise.all(
        feedbackWorktreeIds.map((wtId) =>
          api.getPendingFeedback(wtId).then((fb) => ({ wtId, fb })).catch(() => null),
        ),
      );

      const pendingFeedback: { feedback: FeedbackRequest; ctx: WorktreeContext }[] = [];
      for (const result of feedbackResults) {
        if (!result || !result.fb) continue;
        const ctx = ctxMap.get(result.wtId);
        if (!ctx) continue;
        pendingFeedback.push({ feedback: result.fb, ctx });
      }

      setActivity({ pendingFeedback, activeAgentRuns, activeWorkflowRuns });
      setCtxMap(ctxMap);
      setError(null);
      setLoading(false);
    };

    fetchAll().catch((err: unknown) => {
      setError(err instanceof Error ? err.message : "Failed to load activity");
      setLoading(false);
    });
  }, [repos, tick]);

  // 5-second polling
  useEffect(() => {
    const interval = setInterval(refresh, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  const handlers = useMemo(() => {
    const handleChange = (_data: ConductorEventData) => refresh();
    const map: Partial<Record<ConductorEventType, (data: ConductorEventData) => void>> = {
      agent_started: handleChange,
      agent_stopped: handleChange,
      worktree_created: handleChange,
      worktree_deleted: handleChange,
    };
    return map;
  }, [refresh]);

  useConductorEvents(handlers);

  const handleSubmitFeedback = async (ctx: WorktreeContext, feedbackId: string) => {
    const text = feedbackText[feedbackId] ?? "";
    try {
      await api.submitFeedback(ctx.worktreeId, feedbackId, text);
      setActionError(null);
      refresh();
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to submit feedback");
    }
  };

  const handleDismissFeedback = async (ctx: WorktreeContext, feedbackId: string) => {
    try {
      await api.dismissFeedback(ctx.worktreeId, feedbackId);
      setActionError(null);
      refresh();
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to dismiss feedback");
    }
  };

  const handleCancelWorkflow = async (runId: string) => {
    try {
      await api.cancelWorkflow(runId);
      setActionError(null);
      refresh();
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to cancel workflow");
    }
  };

  if (reposLoading || loading) return <LoadingSpinner />;

  const isEmpty =
    activity.pendingFeedback.length === 0 &&
    activity.activeAgentRuns.length === 0 &&
    activity.activeWorkflowRuns.length === 0;

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold text-gray-900">Activity</h2>

      {error && <ErrorBanner message={error} />}
      {actionError && <ErrorBanner message={actionError} />}

      {/* Pending Feedback */}
      {activity.pendingFeedback.length > 0 && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Pending Feedback
          </h3>
          <div className="space-y-3">
            {activity.pendingFeedback.map(({ feedback, ctx }) => (
              <div
                key={feedback.id}
                className="rounded-lg border border-amber-200 bg-amber-50 p-4 space-y-3"
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <p className="text-xs text-gray-500">
                      {ctx.repoSlug} · {ctx.branch}
                    </p>
                    <p className="text-sm text-gray-800 mt-1">{feedback.prompt}</p>
                  </div>
                </div>
                <textarea
                  value={feedbackText[feedback.id] ?? ""}
                  onChange={(e) =>
                    setFeedbackText((prev) => ({ ...prev, [feedback.id]: e.target.value }))
                  }
                  placeholder="Your response..."
                  rows={3}
                  className="w-full px-3 py-2 text-sm border border-gray-300 rounded resize-none focus:outline-none focus:ring-1 focus:ring-indigo-500"
                />
                <div className="flex gap-2 justify-end">
                  <button
                    onClick={() => handleDismissFeedback(ctx, feedback.id)}
                    className="px-3 py-1.5 text-sm text-gray-600 border border-gray-300 rounded hover:bg-gray-50"
                  >
                    Dismiss
                  </button>
                  <button
                    onClick={() => handleSubmitFeedback(ctx, feedback.id)}
                    className="px-3 py-1.5 text-sm bg-indigo-600 text-white rounded hover:bg-indigo-500"
                  >
                    Submit
                  </button>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Active Agent Runs */}
      {activity.activeAgentRuns.length > 0 && (
        <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Active Agent Runs
          </h3>
          <div className="space-y-2">
            {activity.activeAgentRuns.map(({ run, ctx }) => (
              <div
                key={run.id}
                className="rounded-lg border border-gray-200 bg-white p-4"
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <Link
                      to={`/repos/${ctx.repoId}/worktrees/${ctx.worktreeId}`}
                      className="text-indigo-600 hover:underline text-sm font-medium truncate block"
                    >
                      {ctx.branch}
                    </Link>
                    <p className="text-xs text-gray-500 mt-0.5">{ctx.repoSlug}</p>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <span
                      className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${agentStatusColor(run.status)}`}
                    >
                      {run.status}
                    </span>
                    <span className="text-xs text-gray-400"><TimeAgo date={run.started_at} /></span>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Active Workflow Runs */}
      <section>
          <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
            Active Workflow Runs
          </h3>
          <WorkflowRunTree
            runs={activity.activeWorkflowRuns}
            repos={repos}
            ctxMap={ctxMap}
            onCancel={handleCancelWorkflow}
          />
        </section>

      {/* Empty state */}
      {isEmpty && !error && (
        <div className="text-center py-16 text-gray-400">
          <p className="text-lg font-medium">No active runs</p>
          <p className="text-sm mt-1">Start an agent or workflow to see activity here.</p>
        </div>
      )}
    </div>
  );
}
