import { useEffect, useState } from "react";
import { Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { api } from "../api/client";
import type { Worktree, AgentRun } from "../api/types";
import { RepoCard } from "../components/repos/RepoCard";
import { CreateRepoForm } from "../components/repos/CreateRepoForm";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { agentStatusColor } from "../utils/agentStats";

export function DashboardPage() {
  const { repos, loading: reposLoading, refreshRepos } = useRepos();
  const [worktreeCounts, setWorktreeCounts] = useState<Record<string, number>>(
    {},
  );
  const [activeWorktrees, setActiveWorktrees] = useState<
    (Worktree & { repoSlug: string })[]
  >([]);
  const [latestRuns, setLatestRuns] = useState<Record<string, AgentRun>>({});

  useEffect(() => {
    if (repos.length === 0) return;
    Promise.all([
      Promise.all(
        repos.map((r) =>
          api
            .listWorktrees(r.id)
            .then((wts) => ({ repoId: r.id, slug: r.slug, wts })),
        ),
      ),
      api.latestRunsByWorktree(),
    ]).then(([results, runs]) => {
      const counts: Record<string, number> = {};
      const active: (Worktree & { repoSlug: string })[] = [];
      for (const { repoId, slug, wts } of results) {
        counts[repoId] = wts.length;
        for (const wt of wts) {
          if (wt.status === "active") {
            active.push({ ...wt, repoSlug: slug });
          }
        }
      }
      setWorktreeCounts(counts);
      setActiveWorktrees(active);
      setLatestRuns(runs);
    });
  }, [repos]);

  if (reposLoading) return <LoadingSpinner />;

  return (
    <div className="space-y-8">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold text-gray-900">Dashboard</h2>
        <CreateRepoForm onCreated={refreshRepos} />
      </div>

      {/* Repos */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Repos
        </h3>
        {repos.length === 0 ? (
          <EmptyState message="No repos registered yet. Add one to get started." />
        ) : (
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {repos.map((repo) => (
              <RepoCard
                key={repo.id}
                repo={repo}
                worktreeCount={worktreeCounts[repo.id] ?? 0}
              />
            ))}
          </div>
        )}
      </section>

      {/* Active Worktrees */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Active Worktrees
        </h3>
        {activeWorktrees.length === 0 ? (
          <EmptyState message="No active worktrees" />
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-hidden">
            <table className="w-full text-sm">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <tr>
                  <th className="px-4 py-2">Branch</th>
                  <th className="px-4 py-2">Repo</th>
                  <th className="px-4 py-2">Status</th>
                  <th className="px-4 py-2">Agent</th>
                  <th className="px-4 py-2">Created</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {activeWorktrees.map((wt) => {
                  const run = latestRuns[wt.id];
                  return (
                    <tr key={wt.id}>
                      <td className="px-4 py-2">
                        <Link
                          to={`/repos/${wt.repo_id}/worktrees/${wt.id}`}
                          className="text-indigo-600 hover:underline"
                        >
                          {wt.branch}
                        </Link>
                      </td>
                      <td className="px-4 py-2 text-gray-600">
                        {wt.repoSlug}
                      </td>
                      <td className="px-4 py-2">
                        <StatusBadge status={wt.status} />
                      </td>
                      <td className="px-4 py-2">
                        {run ? (
                          <span
                            className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${agentStatusColor(run.status)}`}
                          >
                            {run.status}
                          </span>
                        ) : (
                          <span className="text-xs text-gray-400">-</span>
                        )}
                      </td>
                      <td className="px-4 py-2 text-gray-500">
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
    </div>
  );
}
