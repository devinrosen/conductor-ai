import { useState } from "react";
import { useParams, Link, useNavigate } from "react-router";
import { useApi } from "../hooks/useApi";
import { api } from "../api/client";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { ConfirmDialog } from "../components/shared/ConfirmDialog";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";

export function WorktreeDetailPage() {
  const { repoId, worktreeId } = useParams<{
    repoId: string;
    worktreeId: string;
  }>();
  const navigate = useNavigate();

  const { data: worktrees, loading } = useApi(
    () => api.listWorktrees(repoId!),
    [repoId],
  );

  const { data: tickets } = useApi(
    () => api.listTickets(repoId!),
    [repoId],
  );

  const [deleteConfirm, setDeleteConfirm] = useState(false);

  const worktree = worktrees?.find((w) => w.id === worktreeId);
  const linkedTicket = worktree?.ticket_id
    ? tickets?.find((t) => t.id === worktree.ticket_id)
    : null;

  async function handleDelete() {
    await api.deleteWorktree(worktreeId!);
    navigate(`/repos/${repoId}`);
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
            <dd className="mt-1 text-gray-900 truncate">{worktree.path}</dd>
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
