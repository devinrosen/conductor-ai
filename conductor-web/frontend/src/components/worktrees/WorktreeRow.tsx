import { Link } from "react-router";
import type { Worktree, AgentRun } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";
import { agentStatusColor } from "../../utils/agentStats";

export function WorktreeRow({
  worktree,
  latestRun,
  onDelete,
  selected,
  index,
}: {
  worktree: Worktree;
  latestRun?: AgentRun;
  onDelete: (id: string) => void;
  selected?: boolean;
  index?: number;
}) {
  return (
    <tr className={selected ? "bg-indigo-50 ring-1 ring-inset ring-indigo-200" : ""} data-list-index={index}>
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
      <td className="px-4 py-2">
        {latestRun ? (
          <span
            className={`inline-block px-2 py-0.5 text-xs font-medium rounded-full ${agentStatusColor(latestRun.status)}`}
          >
            {latestRun.status}
          </span>
        ) : (
          <span className="text-xs text-gray-400">-</span>
        )}
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
