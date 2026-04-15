import { useState } from "react";
import { Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { FeatureStatusBadge } from "../components/features/FeatureStatusBadge";
import { FeatureProgressBar } from "../components/features/FeatureProgressBar";
import type { FeatureRow, Repo } from "../api/types";

function TimeAgoText({ iso }: { iso: string | null }) {
  if (!iso) return <span className="text-gray-400">—</span>;
  const date = new Date(iso);
  const now = Date.now();
  const diff = Math.floor((now - date.getTime()) / 1000);
  if (diff < 60) return <span>{diff}s ago</span>;
  if (diff < 3600) return <span>{Math.floor(diff / 60)}m ago</span>;
  if (diff < 86400) return <span>{Math.floor(diff / 3600)}h ago</span>;
  return <span>{Math.floor(diff / 86400)}d ago</span>;
}

interface RepoFeaturesProps {
  repo: Repo;
}

function RepoFeatureSection({ repo }: RepoFeaturesProps) {
  const { data, loading, error, refetch } = useApi(
    () => api.listFeatures(repo.id),
    [repo.id],
  );
  const [closingId, setClosingId] = useState<string | null>(null);

  if (loading) return <LoadingSpinner />;
  if (error) return <p className="text-sm text-red-500 px-4 py-2">{error}</p>;
  if (!data || data.features.length === 0) return (
    <p className="text-sm text-gray-400 px-4 py-2">No features for this repo.</p>
  );

  const dangling = data.features.filter(
    (f) => f.status === "InProgress" && f.worktree_count === 0,
  );

  async function handleClose(feature: FeatureRow) {
    setClosingId(feature.id);
    try {
      await api.closeFeature(repo.id, feature.name);
      refetch();
    } catch (e) {
      alert(`Failed to close feature: ${e instanceof Error ? e.message : e}`);
    } finally {
      setClosingId(null);
    }
  }

  return (
    <div>
      {dangling.length > 0 && (
        <div className="mb-3 px-4 py-3 bg-amber-50 border border-amber-200 rounded-md text-sm text-amber-800">
          <strong>{dangling.length} dangling feature{dangling.length > 1 ? "s" : ""}</strong>
          {" — in-progress with no active worktrees: "}
          {dangling.map((f, i) => (
            <span key={f.id}>
              {i > 0 && ", "}
              <button
                onClick={() => handleClose(f)}
                disabled={closingId === f.id}
                className="underline hover:text-amber-900 disabled:opacity-50"
              >
                {f.name}
              </button>
            </span>
          ))}
        </div>
      )}

      <table className="min-w-full text-sm" data-testid="features-table">
        <thead>
          <tr className="border-b border-gray-200 text-left text-xs text-gray-500 uppercase tracking-wide">
            <th className="pb-2 font-medium">Feature</th>
            <th className="pb-2 font-medium">Status</th>
            <th className="pb-2 font-medium">Progress</th>
            <th className="pb-2 font-medium">Worktrees</th>
            <th className="pb-2 font-medium">Last Activity</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-100">
          {data.features.map((feature) => (
            <tr
              key={feature.id}
              className="hover:bg-gray-50 transition-colors"
              data-testid="feature-row"
            >
              <td className="py-2 pr-4">
                <Link
                  to={`/repos/${repo.id}/features/${encodeURIComponent(feature.name)}`}
                  className="text-indigo-600 hover:underline font-medium"
                >
                  {feature.name}
                </Link>
                <div className="text-xs text-gray-400">{feature.branch}</div>
              </td>
              <td className="py-2 pr-4">
                <FeatureStatusBadge status={feature.status} />
              </td>
              <td className="py-2 pr-4">
                <FeatureProgressBar
                  merged={feature.tickets_merged}
                  total={feature.tickets_total}
                />
              </td>
              <td className="py-2 pr-4 text-gray-600">{feature.worktree_count}</td>
              <td className="py-2 text-gray-500">
                <TimeAgoText iso={feature.last_worktree_activity ?? feature.last_commit_at} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function FeaturesPage() {
  const { repos, loading: reposLoading } = useRepos();

  if (reposLoading) return <LoadingSpinner />;

  if (repos.length === 0) {
    return (
      <div className="p-6">
        <h1 className="text-xl font-semibold text-gray-900 mb-4">Features</h1>
        <p className="text-sm text-gray-500">No repos registered. Register a repo to see features.</p>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-8">
      <h1 className="text-xl font-semibold text-gray-900">Features</h1>
      {repos.map((repo) => (
        <section key={repo.id}>
          <h2 className="text-sm font-semibold text-gray-700 mb-3 flex items-center gap-2">
            {repo.slug}
          </h2>
          <RepoFeatureSection repo={repo} />
        </section>
      ))}
    </div>
  );
}
