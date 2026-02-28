import { Link } from "react-router";
import type { Worktree } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";

export function WorktreeRow({
  worktree,
  onDelete,
}: {
  worktree: Worktree;
  onDelete: (id: string) => void;
}) {
  return (
    <tr>
      <td className="px-4 py-2">
        <Link
          to={`/repos/${worktree.repo_id}/worktrees/${worktree.id}`}
          className="text-indigo-600 hover:underline"
        >
          {worktree.branch}
        </Link>
      </td>
      <td className="px-4 py-2">
        <StatusBadge status={worktree.status} />
      </td>
      <td className="px-4 py-2 text-gray-500 text-xs truncate max-w-xs">
        {worktree.path}
      </td>
      <td className="px-4 py-2 text-gray-500">
        <TimeAgo date={worktree.created_at} />
      </td>
      <td className="px-4 py-2">
        <button
          onClick={() => onDelete(worktree.id)}
          className="text-xs text-red-600 hover:underline"
        >
          Delete
        </button>
      </td>
    </tr>
  );
}
