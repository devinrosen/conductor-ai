import { useEffect, useState, useCallback, useMemo } from "react";
import { useRepos } from "../components/layout/AppShell";
import { api } from "../api/client";
import { RepoCard } from "../components/repos/RepoCard";
import { RegisterRepoForm } from "../components/repos/RegisterRepoForm";
import { GitHubDiscoverModal } from "../components/repos/GitHubDiscoverModal";
import { EmptyState } from "../components/shared/EmptyState";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import {
  useConductorEvents,
  type ConductorEventType,
  type ConductorEventData,
} from "../hooks/useConductorEvents";

export function ReposPage() {
  const { repos, loading: reposLoading, refreshRepos } = useRepos();
  const [worktreeCounts, setWorktreeCounts] = useState<Record<string, number>>({});
  const [tick, setTick] = useState(0);
  const [registerRepoOpen, setRegisterRepoOpen] = useState(false);
  const [discoverOpen, setDiscoverOpen] = useState(false);

  const refresh = useCallback(() => setTick((n) => n + 1), []);

  useEffect(() => {
    if (repos.length === 0) return;
    Promise.all(
      repos.map((r) =>
        api.listWorktrees(r.id).then((wts) => ({ repoId: r.id, count: wts.length })),
      ),
    ).then((results) => {
      const counts: Record<string, number> = {};
      for (const { repoId, count } of results) {
        counts[repoId] = count;
      }
      setWorktreeCounts(counts);
    });
  }, [repos, tick]);

  const handlers = useMemo(() => {
    const handleChange = (_data: ConductorEventData) => refresh();
    const map: Partial<Record<ConductorEventType, (data: ConductorEventData) => void>> = {
      worktree_created: handleChange,
      worktree_deleted: handleChange,
    };
    return map;
  }, [refresh]);

  useConductorEvents(handlers);

  if (reposLoading) return <LoadingSpinner />;

  return (
    <div className="space-y-6">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        <h2 className="text-xl font-bold text-gray-900">Repos</h2>
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={() => setDiscoverOpen(true)}
            className="px-3 py-2 text-sm rounded-md border border-gray-300 text-gray-700 hover:bg-gray-50"
          >
            Discover from GitHub
          </button>
          <RegisterRepoForm
            onCreated={refreshRepos}
            open={registerRepoOpen}
            onOpenChange={setRegisterRepoOpen}
          />
        </div>
      </div>

      <GitHubDiscoverModal
        open={discoverOpen}
        onClose={() => setDiscoverOpen(false)}
        onImported={refreshRepos}
      />

      {repos.length === 0 ? (
        <EmptyState message="The station is quiet. Register a repo to get the trains running." />
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
    </div>
  );
}
