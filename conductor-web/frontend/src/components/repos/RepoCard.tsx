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
      className="group block rounded-lg border border-gray-200 bg-white overflow-hidden hover:border-indigo-300 transition-all hover:shadow-[0_0_12px_rgba(43,94,167,0.15)]"
    >
      {/* Station accent bar */}
      <div className="h-1 bg-indigo-500 group-hover:bg-indigo-400 transition-colors" />
      <div className="p-3">
        <h3 className="font-semibold text-gray-900">{repo.slug}</h3>
        <p className="mt-0.5 text-xs text-gray-500 truncate">{repo.remote_url}</p>
        <div className="mt-2 flex items-center gap-3 text-xs text-gray-500">
          <span className="flex items-center gap-1">
            <span className="inline-block w-1.5 h-1.5 rounded-full bg-green-500" />
            {worktreeCount} worktree{worktreeCount !== 1 ? "s" : ""}
          </span>
          <span>
            Created <TimeAgo date={repo.created_at} />
          </span>
        </div>
      </div>
    </Link>
  );
}
