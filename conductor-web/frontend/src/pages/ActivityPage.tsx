import { useEffect, useState, useCallback, useMemo } from "react";
import { useNavigate } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { api } from "../api/client";
import type { Worktree, AgentRun, FeedbackRequest } from "../api/types";
import { RepoCard } from "../components/repos/RepoCard";
import { RegisterRepoForm } from "../components/repos/RegisterRepoForm";
import { GitHubDiscoverModal } from "../components/repos/GitHubDiscoverModal";
import { StatusBadge } from "../components/shared/StatusBadge";
import { SignalLight } from "../components/shared/SignalLight";
import { SplitFlap } from "../components/shared/SplitFlap";
import { TrackDivider } from "../components/shared/TrackDivider";
import { StationHeader } from "../components/shared/StationHeader";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { WelcomeAboard } from "../components/shared/WelcomeAboard";
import { ErrorBanner } from "../components/shared/ErrorBanner";
import { agentStatusColor } from "../utils/agentStats";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";
import { useHotkeys } from "../hooks/useHotkeys";
import { useListNav } from "../hooks/useListNav";

interface WorktreeContext {
  repoId: string;
  repoSlug: string;
}

export function ActivityPage() {
  const { repos, loading: reposLoading, refreshRepos } = useRepos();
  const navigate = useNavigate();

  const [worktreeCounts, setWorktreeCounts] = useState<Record<string, number>>({});
  const [activeWorktrees, setActiveWorktrees] = useState<(Worktree & WorktreeContext)[]>([]);
  const [latestRuns, setLatestRuns] = useState<Record<string, AgentRun>>({});
  const [pendingFeedback, setPendingFeedback] = useState<{ feedback: FeedbackRequest; ctx: WorktreeContext & { branch: string; worktreeId: string } }[]>([]);
  const [wtTick, setWtTick] = useState(0);
  const [registerRepoOpen, setRegisterRepoOpen] = useState(false);
  const [discoverOpen, setDiscoverOpen] = useState(false);
  const [feedbackText, setFeedbackText] = useState<Record<string, string>>({});
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const refreshWorktrees = useCallback(() => setWtTick((n) => n + 1), []);

  useEffect(() => {
    if (repos.length === 0) return;

    const fetchData = async () => {
      const repoSlugById: Record<string, string> = {};
      for (const r of repos) repoSlugById[r.id] = r.slug;

      const [allWorktrees, runs] = await Promise.all([
        api.listAllWorktrees(),
        api.latestRunsByWorktree(),
      ]);

      const counts: Record<string, number> = {};
      const active: (Worktree & WorktreeContext)[] = [];
      for (const wt of allWorktrees) {
        const repoId = wt.repo_id;
        const slug = repoSlugById[repoId] ?? "";
        counts[repoId] = (counts[repoId] ?? 0) + 1;
        if (wt.status === "active") {
          active.push({ ...wt, repoId, repoSlug: slug });
        }
      }
      setWorktreeCounts(counts);
      setActiveWorktrees(active);
      setLatestRuns(runs);

      const feedbackWorktrees: { worktreeId: string; ctx: WorktreeContext & { branch: string; worktreeId: string } }[] = [];
      for (const wt of allWorktrees) {
        if (wt.status !== "active") continue;
        const repoId = wt.repo_id;
        const slug = repoSlugById[repoId] ?? "";
        const run = runs[wt.id];
        if (run && run.status === "waiting_for_feedback") {
          feedbackWorktrees.push({ worktreeId: wt.id, ctx: { repoId, repoSlug: slug, branch: wt.branch, worktreeId: wt.id } });
        }
      }

      if (feedbackWorktrees.length > 0) {
        const results = await Promise.all(
          feedbackWorktrees.map(({ worktreeId, ctx }) =>
            api.getPendingFeedback(worktreeId).then((fb) => fb ? { feedback: fb, ctx } : null).catch(() => null)
          )
        );
        setPendingFeedback(results.filter(Boolean) as typeof pendingFeedback);
      } else {
        setPendingFeedback([]);
      }

      setLoadError(null);
    };

    fetchData().catch((err: unknown) => {
      setLoadError(err instanceof Error ? err.message : "Failed to load activity data");
    });
  }, [repos, wtTick]);

  // 10-second polling
  useEffect(() => {
    const interval = setInterval(refreshWorktrees, 10000);
    return () => clearInterval(interval);
  }, [refreshWorktrees]);

  const handlers = useMemo(() => {
    const handle = (_data: ConductorEventData) => refreshWorktrees();
    const map: Partial<Record<ConductorEventType, (data: ConductorEventData) => void>> = {
      worktree_created: handle, worktree_deleted: handle,
      agent_started: handle, agent_stopped: handle,
      repo_registered: handle, repo_unregistered: handle,
    };
    return map;
  }, [refreshWorktrees]);

  useConductorEvents(handlers);

  // Feedback handlers
  const handleSubmitFeedback = async (worktreeId: string, feedbackId: string) => {
    const text = feedbackText[feedbackId] ?? "";
    try {
      await api.submitFeedback(worktreeId, feedbackId, text);
      setActionError(null);
      refreshWorktrees();
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to submit feedback");
    }
  };

  const handleDismissFeedback = async (worktreeId: string, feedbackId: string) => {
    try {
      await api.dismissFeedback(worktreeId, feedbackId);
      setActionError(null);
      refreshWorktrees();
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to dismiss feedback");
    }
  };

  // Keyboard nav for worktree list
  const { selectedIndex, moveDown, moveUp, reset } = useListNav(activeWorktrees.length);
  const openSelected = useCallback(() => {
    const wt = activeWorktrees[selectedIndex];
    if (wt) navigate(`/repos/${wt.repoId}/worktrees/${wt.id}`);
  }, [activeWorktrees, selectedIndex, navigate]);
  const openCreateRepo = useCallback(() => setRegisterRepoOpen(true), []);
  const openDiscover = useCallback(() => setDiscoverOpen(true), []);
  const handleEscape = useCallback(() => { if (selectedIndex >= 0) reset(); }, [selectedIndex, reset]);

  useHotkeys([
    { key: "j", handler: moveDown, description: "Next worktree" },
    { key: "k", handler: moveUp, description: "Previous worktree" },
    { key: "Enter", handler: openSelected, description: "Open selected", enabled: selectedIndex >= 0 },
    { key: "c", handler: openCreateRepo, description: "Register repo" },
    { key: "d", handler: openDiscover, description: "Discover GitHub repos" },
    { key: "Escape", handler: handleEscape, description: "Deselect", enabled: selectedIndex >= 0 },
  ]);

  if (reposLoading) return <LoadingSpinner />;

  if (repos.length === 0) {
    return <WelcomeAboard onRepoCreated={refreshRepos} />;
  }

  // Summary counts
  const runningAgents = Object.values(latestRuns).filter((r) => r.status === "running").length;

  return (
    <div className="flex flex-col h-[calc(100vh-4rem)] overflow-hidden gap-3">
      {/* Header + departure board summary */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h2 className="text-lg font-bold text-gray-900">Home</h2>
          <SplitFlap
            text={`${repos.length} REPOS  ${activeWorktrees.length} TRACKS  ${runningAgents} AGENTS`}
            length={32}
          />
        </div>
        <div className="flex items-center gap-2">
          <button onClick={() => setDiscoverOpen(true)} className="px-3 py-1.5 text-sm rounded-md border border-gray-300 text-gray-600 hover:bg-gray-100">
            Discover
          </button>
          <RegisterRepoForm onCreated={refreshRepos} open={registerRepoOpen} onOpenChange={setRegisterRepoOpen} />
        </div>
      </div>

      <ErrorBanner error={loadError} />
      <ErrorBanner error={actionError} />

      {/* Attention Required — pending feedback */}
      {pendingFeedback.length > 0 && (
        <section className="shrink-0">
          <StationHeader>Attention Required</StationHeader>
          <div className="space-y-2">
            {pendingFeedback.map(({ feedback, ctx }) => (
              <div key={feedback.id} className="rounded-lg border border-amber-200 bg-amber-50 p-3 space-y-2">
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <p className="text-xs text-gray-500">{ctx.repoSlug} · {ctx.branch}</p>
                    <p className="text-sm text-gray-800 mt-0.5">{feedback.prompt}</p>
                  </div>
                </div>
                <textarea
                  value={feedbackText[feedback.id] ?? ""}
                  onChange={(e) => setFeedbackText((prev) => ({ ...prev, [feedback.id]: e.target.value }))}
                  placeholder="Your response..."
                  rows={2}
                  className="w-full px-2 py-1.5 text-sm border border-gray-300 rounded resize-none focus:outline-none focus:ring-1 focus:ring-indigo-500"
                />
                <div className="flex gap-2 justify-end">
                  <button onClick={() => handleDismissFeedback(ctx.worktreeId, feedback.id)}
                    className="px-2 py-1 text-xs text-gray-600 border border-gray-300 rounded hover:bg-gray-50">Dismiss</button>
                  <button onClick={() => handleSubmitFeedback(ctx.worktreeId, feedback.id)}
                    className="px-2 py-1 text-xs bg-indigo-600 text-white rounded hover:bg-indigo-500">Submit</button>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Main content area */}
      <div className="flex-1 flex flex-col gap-3 min-h-0 overflow-y-auto">
        {/* Active Worktrees */}
        <section className="flex flex-col shrink-0">
          <StationHeader count={activeWorktrees.length}>Active Worktrees</StationHeader>
          {activeWorktrees.length === 0 ? (
            <div className="text-center py-8 text-gray-400 text-sm">
              No platforms active. Create a worktree from a repo to lay some track.
            </div>
          ) : (
            <div className="rounded-lg border border-gray-200 bg-white overflow-x-auto">
              <table className="w-full text-sm min-w-[540px]">
                <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase sticky top-0 z-10">
                  <tr>
                    <th className="px-2 py-1.5 w-8"></th>
                    <th className="px-3 py-1.5">Branch</th>
                    <th className="px-3 py-1.5">Repo</th>
                    <th className="px-3 py-1.5">Status</th>
                    <th className="px-3 py-1.5">Agent</th>
                    <th className="px-3 py-1.5">Created</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-gray-100">
                  {activeWorktrees.map((wt, index) => {
                    const run = latestRuns[wt.id];
                    const overallStatus = run?.status === "running" ? "running"
                      : run?.status === "waiting_for_feedback" ? "waiting"
                      : run?.status === "failed" ? "failed"
                      : wt.status;
                    return (
                      <tr
                        key={wt.id}
                        data-list-index={index}
                        className={`cursor-pointer transition-colors hover:bg-gray-50 border-l-2 ${
                          selectedIndex === index
                            ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200 border-l-indigo-500"
                            : "border-l-transparent hover:border-l-indigo-300"
                        }`}
                        onClick={() => navigate(`/repos/${wt.repoId}/worktrees/${wt.id}`)}
                      >
                        <td className="px-2 py-1.5">
                          <SignalLight status={overallStatus} size={18} />
                        </td>
                        <td className="px-3 py-1.5">
                          <span className="text-indigo-600 font-medium">{wt.branch}</span>
                        </td>
                        <td className="px-3 py-1.5">
                          <span className="inline-block px-1.5 py-0.5 text-[11px] font-mono rounded bg-gray-100 text-gray-600">
                            {wt.repoSlug}
                          </span>
                        </td>
                        <td className="px-3 py-1.5">
                          <StatusBadge status={wt.status} />
                        </td>
                        <td className="px-3 py-1.5">
                          {run ? (
                            <span className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${agentStatusColor(run.status)}`}>
                              {run.status}
                            </span>
                          ) : (
                            <span className="text-xs text-gray-400">idle</span>
                          )}
                        </td>
                        <td className="px-3 py-1.5 text-xs text-gray-500">
                          <TimeAgo date={wt.created_at} />
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>

        <TrackDivider />

        {/* Repos Grid */}
        <section className="shrink-0">
          <StationHeader count={repos.length}>Stations</StationHeader>
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
            {repos.map((repo) => (
              <RepoCard key={repo.id} repo={repo} worktreeCount={worktreeCounts[repo.id] ?? 0} />
            ))}
          </div>
        </section>
      </div>

      <GitHubDiscoverModal open={discoverOpen} onClose={() => setDiscoverOpen(false)} onImported={refreshRepos} />
    </div>
  );
}
