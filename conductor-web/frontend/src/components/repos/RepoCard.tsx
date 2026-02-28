import { Link } from "react-router";
import type { Repo } from "../../api/types";
import { TimeAgo } from "../shared/TimeAgo";

export function RepoCard({
  repo,
  worktreeCount,
}: {
  repo: Repo;
  worktreeCount: number;
}) {
  return (
    <Link
      to={`/repos/${repo.id}`}
      className="block rounded-lg border border-gray-200 bg-white p-4 hover:border-indigo-300 hover:shadow-sm transition-colors"
    >
      <h3 className="font-semibold text-gray-900">{repo.slug}</h3>
      <p className="mt-1 text-xs text-gray-500 truncate">{repo.remote_url}</p>
      <div className="mt-3 flex items-center gap-4 text-xs text-gray-500">
        <span>{worktreeCount} worktree{worktreeCount !== 1 ? "s" : ""}</span>
        <span>
          Created <TimeAgo date={repo.created_at} />
        </span>
      </div>
    </Link>
  );
}
