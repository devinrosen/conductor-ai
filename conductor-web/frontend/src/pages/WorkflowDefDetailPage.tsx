import { useState, useEffect } from "react";
import { useParams, Link } from "react-router";
import { api } from "../api/client";
import type { WorkflowDef } from "../api/types";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { TransitBreadcrumb } from "../components/shared/TransitBreadcrumb";
import { WorkflowDefViewer } from "../components/workflows/WorkflowDefViewer";

export function WorkflowDefDetailPage() {
  const { repoId, worktreeId, defName } = useParams<{
    repoId: string;
    worktreeId: string;
    defName: string;
  }>();

  const [def, setDef] = useState<WorkflowDef | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!worktreeId || !defName) return;
    api
      .getWorkflowDef(worktreeId, defName)
      .then(setDef)
      .catch((err) => setError(err instanceof Error ? err.message : "Failed to load definition"))
      .finally(() => setLoading(false));
  }, [worktreeId, defName]);

  if (loading) return <LoadingSpinner />;

  if (error || !def) {
    return (
      <div className="text-center py-12">
        <p className="text-red-500 text-sm">{error ?? "Definition not found"}</p>
        <Link
          to={`/repos/${repoId}/worktrees/${worktreeId}`}
          className="text-indigo-600 hover:underline text-sm mt-2 inline-block"
        >
          Back to worktree
        </Link>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <TransitBreadcrumb stops={[
        { label: "Home", href: "/" },
        { label: "Repo", href: `/repos/${repoId}` },
        { label: "Worktree", href: `/repos/${repoId}/worktrees/${worktreeId}` },
        { label: def.name, current: true },
      ]} />

      <WorkflowDefViewer def={def} />
    </div>
  );
}
