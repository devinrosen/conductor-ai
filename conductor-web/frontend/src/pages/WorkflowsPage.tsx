import { useEffect, useState, useCallback } from "react";
import { Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { api } from "../api/client";
import type { Repo, Worktree, WorkflowRun, WorkflowDefSummary } from "../api/types";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { RunWorkflowModal } from "../components/workflows/RunWorkflowModal";

interface WorktreeContext {
  repoId: string;
  repoSlug: string;
  branch: string;
  worktreeId: string;
}

type PickerStep = "repo" | "worktree" | "def" | "confirm";

export function WorkflowsPage() {
  const { repos, loading: reposLoading } = useRepos();
  const [runs, setRuns] = useState<{ run: WorkflowRun; ctx: WorktreeContext }[]>([]);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  // Picker state
  const [pickerStep, setPickerStep] = useState<PickerStep | null>(null);
  const [pickerRepo, setPickerRepo] = useState<Repo | null>(null);
  const [pickerWorktrees, setPickerWorktrees] = useState<Worktree[]>([]);
  const [pickerWorktree, setPickerWorktree] = useState<Worktree | null>(null);
  const [pickerDefs, setPickerDefs] = useState<WorkflowDefSummary[]>([]);
  const [pickerDef, setPickerDef] = useState<WorkflowDefSummary | null>(null);

  const refresh = useCallback(() => setTick((n) => n + 1), []);

  useEffect(() => {
    if (repos.length === 0) {
      setLoading(false);
      return;
    }

    const fetchAll = async () => {
      const repoWorktrees = await Promise.all(
        repos.map((r) =>
          api.listWorktrees(r.id).then((wts) => ({ repo: r, wts })),
        ),
      );

      const ctxMap = new Map<string, WorktreeContext>();
      for (const { repo, wts } of repoWorktrees) {
        for (const wt of wts) {
          if (wt.status === "active") {
            ctxMap.set(wt.id, {
              repoId: repo.id,
              repoSlug: repo.slug,
              branch: wt.branch,
              worktreeId: wt.id,
            });
          }
        }
      }

      const activeWorktreeIds = Array.from(ctxMap.keys());
      const runArrays = await Promise.all(
        activeWorktreeIds.map((wtId) =>
          api.listWorkflowRuns(wtId).catch(() => [] as WorkflowRun[]),
        ),
      );

      const allRuns: { run: WorkflowRun; ctx: WorktreeContext }[] = [];
      for (let i = 0; i < activeWorktreeIds.length; i++) {
        const wtId = activeWorktreeIds[i];
        const ctx = ctxMap.get(wtId)!;
        for (const run of runArrays[i]) {
          allRuns.push({ run, ctx });
        }
      }

      allRuns.sort(
        (a, b) =>
          new Date(b.run.started_at).getTime() - new Date(a.run.started_at).getTime(),
      );

      setRuns(allRuns);
      setLoading(false);
    };

    fetchAll().catch(() => setLoading(false));
  }, [repos, tick]);

  // 5-second polling
  useEffect(() => {
    const interval = setInterval(refresh, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  const handleCancelWorkflow = async (runId: string) => {
    try {
      await api.cancelWorkflow(runId);
      refresh();
    } catch {
      // ignore
    }
  };

  // Picker: select repo → load worktrees
  const handleSelectRepo = async (repo: Repo) => {
    setPickerRepo(repo);
    const wts = await api.listWorktrees(repo.id);
    setPickerWorktrees(wts.filter((wt) => wt.status === "active"));
    setPickerStep("worktree");
  };

  // Picker: select worktree → load defs
  const handleSelectWorktree = async (wt: Worktree) => {
    setPickerWorktree(wt);
    const defs = await api.listWorkflowDefs(wt.id);
    setPickerDefs(defs);
    setPickerStep("def");
  };

  // Picker: select def → confirm
  const handleSelectDef = (def: WorkflowDefSummary) => {
    setPickerDef(def);
    setPickerStep("confirm");
  };

  const resetPicker = () => {
    setPickerStep(null);
    setPickerRepo(null);
    setPickerWorktree(null);
    setPickerDef(null);
    setPickerDefs([]);
    setPickerWorktrees([]);
  };

  if (reposLoading || loading) return <LoadingSpinner />;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold text-gray-900">Workflows</h2>
        {!pickerStep && (
          <button
            onClick={() => setPickerStep("repo")}
            className="px-3 py-2 text-sm bg-indigo-600 text-white rounded-md hover:bg-indigo-500"
          >
            Start Workflow
          </button>
        )}
      </div>

      {/* Start Workflow Picker */}
      {pickerStep && (
        <div className="rounded-lg border border-indigo-200 bg-indigo-50 p-4 space-y-3">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-semibold text-indigo-800">
              {pickerStep === "repo" && "Select a repo"}
              {pickerStep === "worktree" && `Select a worktree — ${pickerRepo?.slug}`}
              {pickerStep === "def" && `Select a workflow — ${pickerWorktree?.branch}`}
              {pickerStep === "confirm" && `Run: ${pickerDef?.name}`}
            </h3>
            <button
              onClick={resetPicker}
              className="text-xs text-gray-500 hover:text-gray-700"
            >
              Cancel
            </button>
          </div>

          {pickerStep === "repo" && (
            <div className="space-y-1">
              {repos.map((repo) => (
                <button
                  key={repo.id}
                  onClick={() => handleSelectRepo(repo)}
                  className="w-full text-left px-3 py-2 text-sm bg-white rounded border border-gray-200 hover:border-indigo-300 hover:bg-indigo-50"
                >
                  {repo.slug}
                </button>
              ))}
            </div>
          )}

          {pickerStep === "worktree" && (
            <div className="space-y-1">
              <button
                onClick={() => setPickerStep("repo")}
                className="text-xs text-indigo-600 hover:underline mb-1"
              >
                ← Back
              </button>
              {pickerWorktrees.length === 0 ? (
                <p className="text-sm text-gray-500">No active worktrees in this repo.</p>
              ) : (
                pickerWorktrees.map((wt) => (
                  <button
                    key={wt.id}
                    onClick={() => handleSelectWorktree(wt)}
                    className="w-full text-left px-3 py-2 text-sm bg-white rounded border border-gray-200 hover:border-indigo-300 hover:bg-indigo-50"
                  >
                    {wt.branch}
                  </button>
                ))
              )}
            </div>
          )}

          {pickerStep === "def" && (
            <div className="space-y-1">
              <button
                onClick={() => setPickerStep("worktree")}
                className="text-xs text-indigo-600 hover:underline mb-1"
              >
                ← Back
              </button>
              {pickerDefs.length === 0 ? (
                <p className="text-sm text-gray-500">No workflow definitions found.</p>
              ) : (
                pickerDefs.map((def) => (
                  <button
                    key={def.name}
                    onClick={() => handleSelectDef(def)}
                    className="w-full text-left px-3 py-2 text-sm bg-white rounded border border-gray-200 hover:border-indigo-300 hover:bg-indigo-50"
                  >
                    <span className="font-medium">{def.name}</span>
                    {def.description && (
                      <span className="text-gray-500 ml-2">{def.description}</span>
                    )}
                  </button>
                ))
              )}
            </div>
          )}
        </div>
      )}

      {/* RunWorkflowModal */}
      {pickerStep === "confirm" && pickerDef && pickerWorktree && (
        <RunWorkflowModal
          def={pickerDef}
          worktreeId={pickerWorktree.id}
          onClose={resetPicker}
          onStarted={() => {
            resetPicker();
            refresh();
          }}
        />
      )}

      {/* Workflow Runs */}
      <section>
        <h3 className="text-sm font-semibold uppercase tracking-wider text-gray-400 mb-3">
          Recent Runs
        </h3>
        {runs.length === 0 ? (
          <div className="text-center py-12 text-gray-400">
            <p className="text-sm">No workflow runs yet.</p>
          </div>
        ) : (
          <div className="space-y-2">
            {runs.map(({ run, ctx }) => (
              <div
                key={run.id}
                className="rounded-lg border border-gray-200 bg-white p-4"
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <Link
                      to={`/repos/${ctx.repoId}/worktrees/${ctx.worktreeId}/workflows/runs/${run.id}`}
                      className="text-indigo-600 hover:underline text-sm font-medium truncate block"
                    >
                      {run.workflow_name}
                    </Link>
                    <p className="text-xs text-gray-500 mt-0.5">
                      {ctx.repoSlug} · {ctx.branch}
                    </p>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <StatusBadge status={run.status} />
                    {run.dry_run && (
                      <span className="text-xs px-1.5 py-0.5 bg-yellow-100 text-yellow-700 rounded">
                        dry-run
                      </span>
                    )}
                    <span className="text-xs text-gray-400">
                      <TimeAgo date={run.started_at} />
                    </span>
                    {(run.status === "running" || run.status === "waiting") && (
                      <button
                        onClick={() => handleCancelWorkflow(run.id)}
                        className="px-2 py-0.5 text-xs bg-red-100 text-red-700 rounded hover:bg-red-200"
                      >
                        Cancel
                      </button>
                    )}
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
