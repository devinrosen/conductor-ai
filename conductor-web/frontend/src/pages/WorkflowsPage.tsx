import { useEffect, useState, useCallback, useMemo } from "react";
import { Link } from "react-router";
import { useRepos } from "../components/layout/AppShell";
import { api } from "../api/client";
import type { Repo, Worktree, WorkflowRun, WorkflowDefSummary } from "../api/types";
import { StatusBadge } from "../components/shared/StatusBadge";
import { TimeAgo } from "../components/shared/TimeAgo";
import { LoadingSpinner } from "../components/shared/LoadingSpinner";
import { EmptyState } from "../components/shared/EmptyState";
import { RunWorkflowModal } from "../components/workflows/RunWorkflowModal";
import { WorkflowRunTree } from "../components/workflows/WorkflowRunTree";
import { formatDuration, liveElapsedMs } from "../utils/agentStats";

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
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  // Filters & sorting
  const [statusFilter, setStatusFilter] = useState<Set<string>>(new Set());
  const [nameFilter, setNameFilter] = useState("");
  const [searchText, setSearchText] = useState("");
  const [sortCol, setSortCol] = useState<string | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("asc");
  const [viewMode, setViewMode] = useState<"tree" | "table">("tree");

  // Picker state
  const [pickerStep, setPickerStep] = useState<PickerStep | null>(null);
  const [pickerRepo, setPickerRepo] = useState<Repo | null>(null);
  const [pickerWorktrees, setPickerWorktrees] = useState<Worktree[]>([]);
  const [pickerWorktree, setPickerWorktree] = useState<Worktree | null>(null);
  const [pickerDefs, setPickerDefs] = useState<WorkflowDefSummary[]>([]);
  const [pickerDef, setPickerDef] = useState<WorkflowDefSummary | null>(null);

  const refresh = useCallback(() => setTick((n) => n + 1), []);

  useEffect(() => {
    if (repos.length === 0) { setLoading(false); return; }

    const fetchAll = async () => {
      const repoWorktrees = await Promise.all(
        repos.map((r) => api.listWorktrees(r.id).then((wts) => ({ repo: r, wts }))),
      );

      const ctxMap = new Map<string, WorktreeContext>();
      for (const { repo, wts } of repoWorktrees) {
        for (const wt of wts) {
          if (wt.status === "active") {
            ctxMap.set(wt.id, { repoId: repo.id, repoSlug: repo.slug, branch: wt.branch, worktreeId: wt.id });
          }
        }
      }

      const activeWorktreeIds = Array.from(ctxMap.keys());
      const runArrays = await Promise.all(
        activeWorktreeIds.map((wtId) => api.listWorkflowRuns(wtId).catch(() => [] as WorkflowRun[])),
      );

      const allRuns: { run: WorkflowRun; ctx: WorktreeContext }[] = [];
      for (let i = 0; i < activeWorktreeIds.length; i++) {
        const ctx = ctxMap.get(activeWorktreeIds[i])!;
        for (const run of runArrays[i]) allRuns.push({ run, ctx });
      }
      allRuns.sort((a, b) => new Date(b.run.started_at).getTime() - new Date(a.run.started_at).getTime());

      setRuns(allRuns);
      setError(null);
      setLoading(false);
    };

    fetchAll().catch((err: unknown) => {
      setError(err instanceof Error ? err.message : "Failed to load workflows");
      setLoading(false);
    });
  }, [repos, tick]);

  useEffect(() => {
    const interval = setInterval(refresh, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  // Derived data
  const uniqueStatuses = useMemo(() => [...new Set(runs.map((r) => r.run.status))].sort(), [runs]);
  const uniqueNames = useMemo(() => [...new Set(runs.map((r) => r.run.workflow_name))].sort(), [runs]);

  const statusCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const { run } of runs) counts[run.status] = (counts[run.status] || 0) + 1;
    return counts;
  }, [runs]);

  const filteredRuns = useMemo(() => {
    let result = runs;
    if (statusFilter.size > 0) result = result.filter((r) => statusFilter.has(r.run.status));
    if (nameFilter) result = result.filter((r) => r.run.workflow_name === nameFilter);
    if (searchText) {
      const q = searchText.toLowerCase();
      result = result.filter((r) =>
        r.run.workflow_name.toLowerCase().includes(q) ||
        r.ctx.repoSlug.toLowerCase().includes(q) ||
        r.ctx.branch.toLowerCase().includes(q) ||
        (r.run.target_label?.toLowerCase().includes(q) ?? false)
      );
    }
    if (sortCol) {
      result = [...result].sort((a, b) => {
        if (sortCol === "duration") {
          const da = runDurationMs(a.run) ?? -1;
          const db = runDurationMs(b.run) ?? -1;
          return sortDir === "asc" ? da - db : db - da;
        }
        let va = "", vb = "";
        switch (sortCol) {
          case "workflow": va = a.run.workflow_name; vb = b.run.workflow_name; break;
          case "target": va = a.ctx.repoSlug + a.ctx.branch; vb = b.ctx.repoSlug + b.ctx.branch; break;
          case "status": va = a.run.status; vb = b.run.status; break;
          case "started": va = a.run.started_at; vb = b.run.started_at; break;
        }
        const cmp = va.localeCompare(vb);
        return sortDir === "asc" ? cmp : -cmp;
      });
    }
    return result;
  }, [runs, statusFilter, nameFilter, searchText, sortCol, sortDir]);

  const runDurationMs = useCallback((run: WorkflowRun): number | null => {
    if (run.ended_at) return new Date(run.ended_at).getTime() - new Date(run.started_at).getTime();
    if (run.status === "running" || run.status === "waiting") return liveElapsedMs(run.started_at);
    return null;
  }, []);

  // Build ctxMap for tree view from the runs data
  const treeCtxMap = useMemo(() => {
    const m = new Map<string, { repoId: string; worktreeId: string; repoSlug: string; branch: string }>();
    for (const { run, ctx } of runs) {
      if (run.worktree_id && !m.has(run.worktree_id)) {
        m.set(run.worktree_id, { repoId: ctx.repoId, worktreeId: ctx.worktreeId, repoSlug: ctx.repoSlug, branch: ctx.branch });
      }
    }
    return m;
  }, [runs]);

  const activeFilterCount = statusFilter.size + (nameFilter ? 1 : 0) + (searchText ? 1 : 0);

  const toggleSort = useCallback((col: string) => {
    if (sortCol === col) { setSortDir((d) => d === "asc" ? "desc" : "asc"); }
    else { setSortCol(col); setSortDir("asc"); }
  }, [sortCol]);

  const sortArrow = (col: string) => sortCol === col ? (sortDir === "asc" ? "\u25B2" : "\u25BC") : null;

  const handleCancelWorkflow = async (runId: string) => {
    try {
      await api.cancelWorkflow(runId);
      setActionError(null);
      refresh();
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to cancel workflow");
    }
  };

  const toggleStatus = useCallback((s: string) => {
    setStatusFilter((prev) => {
      const next = new Set(prev);
      if (next.has(s)) next.delete(s); else next.add(s);
      return next;
    });
  }, []);

  const clearFilters = useCallback(() => {
    setStatusFilter(new Set());
    setNameFilter("");
    setSearchText("");
  }, []);

  // Picker handlers
  const handleSelectRepo = async (repo: Repo) => {
    try {
      setPickerRepo(repo);
      const wts = await api.listWorktrees(repo.id);
      setPickerWorktrees(wts.filter((wt) => wt.status === "active"));
      setPickerStep("worktree");
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to load worktrees");
    }
  };
  const handleSelectWorktree = async (wt: Worktree) => {
    try {
      setPickerWorktree(wt);
      const defs = await api.listWorkflowDefs(wt.id);
      setPickerDefs(defs);
      setPickerStep("def");
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : "Failed to load definitions");
    }
  };
  const handleSelectDef = (def: WorkflowDefSummary) => { setPickerDef(def); setPickerStep("confirm"); };
  const resetPicker = () => {
    setPickerStep(null); setPickerRepo(null); setPickerWorktree(null);
    setPickerDef(null); setPickerDefs([]); setPickerWorktrees([]);
  };

  if (reposLoading || loading) return <LoadingSpinner />;

  return (
    <div className="flex flex-col h-[calc(100vh-4rem)] overflow-hidden gap-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h2 className="text-lg font-bold text-gray-900">Workflows</h2>
          {/* Status summary pills */}
          <div className="flex items-center gap-1.5">
            {uniqueStatuses.map((s) => (
              <button
                key={s}
                onClick={() => toggleStatus(s)}
                className={`flex items-center gap-1 px-2 py-0.5 text-xs rounded-full border transition-colors ${
                  statusFilter.has(s)
                    ? "border-indigo-400 bg-indigo-100 text-indigo-700"
                    : "border-gray-200 text-gray-500 hover:border-gray-300"
                }`}
              >
                <StatusBadge status={s} />
                <span className="font-mono">{statusCounts[s] ?? 0}</span>
              </button>
            ))}
          </div>
        </div>
        <div className="flex items-center gap-2">
          <div className="flex rounded-md border border-gray-200 overflow-hidden text-xs">
            <button
              onClick={() => setViewMode("tree")}
              className={`px-2.5 py-1 ${viewMode === "tree" ? "bg-gray-100 text-gray-800 font-medium" : "text-gray-500 hover:bg-gray-50"}`}
            >
              Tree
            </button>
            <button
              onClick={() => setViewMode("table")}
              className={`px-2.5 py-1 border-l border-gray-200 ${viewMode === "table" ? "bg-gray-100 text-gray-800 font-medium" : "text-gray-500 hover:bg-gray-50"}`}
            >
              Table
            </button>
          </div>
          <button
            onClick={() => setPickerStep("repo")}
            className="px-3 py-1.5 text-sm bg-indigo-600 text-white rounded-md hover:bg-indigo-500"
          >
            Start Workflow
          </button>
        </div>
      </div>

      {error && (
        <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">{error}</div>
      )}
      {actionError && (
        <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">{actionError}</div>
      )}

      {/* Start Workflow Picker (modal-style overlay) */}
      {pickerStep && pickerStep !== "confirm" && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={resetPicker}>
          <div className="w-full max-w-md rounded-lg border border-gray-200 bg-white shadow-2xl p-4 space-y-3" onClick={(e) => e.stopPropagation()}>
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold text-gray-900">
                {pickerStep === "repo" && "Select a repo"}
                {pickerStep === "worktree" && `Select a worktree \u2014 ${pickerRepo?.slug}`}
                {pickerStep === "def" && `Select a workflow \u2014 ${pickerWorktree?.branch}`}
              </h3>
              <button onClick={resetPicker} className="text-xs text-gray-400 hover:text-gray-600">Cancel</button>
            </div>

            {pickerStep === "repo" && (
              <div className="space-y-1 max-h-64 overflow-y-auto">
                {repos.map((repo) => (
                  <button key={repo.id} onClick={() => handleSelectRepo(repo)}
                    className="w-full text-left px-3 py-2 text-sm rounded border border-gray-200 hover:border-indigo-300 hover:bg-indigo-50">
                    {repo.slug}
                  </button>
                ))}
              </div>
            )}

            {pickerStep === "worktree" && (
              <div className="space-y-1 max-h-64 overflow-y-auto">
                <button onClick={() => setPickerStep("repo")} className="text-xs text-indigo-600 hover:underline mb-1">&larr; Back</button>
                {pickerWorktrees.length === 0 ? (
                  <p className="text-sm text-gray-500">No active worktrees in this repo.</p>
                ) : pickerWorktrees.map((wt) => (
                  <button key={wt.id} onClick={() => handleSelectWorktree(wt)}
                    className="w-full text-left px-3 py-2 text-sm rounded border border-gray-200 hover:border-indigo-300 hover:bg-indigo-50">
                    {wt.branch}
                  </button>
                ))}
              </div>
            )}

            {pickerStep === "def" && (
              <div className="space-y-1 max-h-64 overflow-y-auto">
                <button onClick={() => setPickerStep("worktree")} className="text-xs text-indigo-600 hover:underline mb-1">&larr; Back</button>
                {pickerDefs.length === 0 ? (
                  <p className="text-sm text-gray-500">No timetable set. Add .wf files to schedule your first route.</p>
                ) : pickerDefs.map((def) => (
                  <button key={def.name} onClick={() => handleSelectDef(def)}
                    className="w-full text-left px-3 py-2 text-sm rounded border border-gray-200 hover:border-indigo-300 hover:bg-indigo-50">
                    <span className="font-medium">{def.name}</span>
                    {def.description && <span className="text-gray-500 ml-2 text-xs">{def.description}</span>}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
      )}

      {pickerStep === "confirm" && pickerDef && pickerWorktree && (
        <RunWorkflowModal
          def={pickerDef}
          worktreeId={pickerWorktree.id}
          onClose={resetPicker}
          onStarted={() => { resetPicker(); refresh(); }}
        />
      )}

      {/* Filter bar */}
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={searchText}
          onChange={(e) => setSearchText(e.target.value)}
          placeholder="Search runs..."
          className="flex-1 sm:max-w-xs px-3 py-1.5 text-sm rounded-md border border-gray-200 bg-gray-50 placeholder-gray-400 focus:outline-none focus:ring-1 focus:ring-indigo-500"
        />
        {uniqueNames.length > 1 && (
          <select
            value={nameFilter}
            onChange={(e) => setNameFilter(e.target.value)}
            className="px-2 py-1.5 text-sm rounded-md border border-gray-200 bg-gray-50 text-gray-700"
          >
            <option value="">All workflows</option>
            {uniqueNames.map((n) => <option key={n} value={n}>{n}</option>)}
          </select>
        )}
        {activeFilterCount > 0 && (
          <button onClick={clearFilters} className="px-2 py-1.5 text-xs text-gray-400 hover:text-gray-600">
            Clear filters
          </button>
        )}
      </div>

      {/* Run history */}
      <div className="flex-1 min-h-0 overflow-hidden">
        {runs.length === 0 ? (
          <EmptyState message="No timetable set. Run a workflow to see activity here." />
        ) : filteredRuns.length === 0 ? (
          <EmptyState message="No runs match your filter." />
        ) : viewMode === "tree" ? (
          <div className="overflow-y-auto h-full">
            <WorkflowRunTree
              runs={filteredRuns.map((r) => r.run)}
              repos={repos}
              ctxMap={treeCtxMap}
              onCancel={handleCancelWorkflow}
            />
          </div>
        ) : (
          <div className="rounded-lg border border-gray-200 bg-white overflow-y-auto overflow-x-auto h-full">
            <table className="w-full text-sm min-w-[600px]">
              <thead className="bg-gray-50 text-left text-xs text-gray-500 uppercase sticky top-0 z-10">
                <tr>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("workflow")} className="hover:text-gray-800 flex items-center gap-1">
                      Workflow {sortArrow("workflow") && <span>{sortArrow("workflow")}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("target")} className="hover:text-gray-800 flex items-center gap-1">
                      Target {sortArrow("target") && <span>{sortArrow("target")}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("status")} className="hover:text-gray-800 flex items-center gap-1">
                      Status {sortArrow("status") && <span>{sortArrow("status")}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("started")} className="hover:text-gray-800 flex items-center gap-1">
                      Started {sortArrow("started") && <span>{sortArrow("started")}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">
                    <button onClick={() => toggleSort("duration")} className="hover:text-gray-800 flex items-center gap-1">
                      Duration {sortArrow("duration") && <span>{sortArrow("duration")}</span>}
                    </button>
                  </th>
                  <th className="px-3 py-1.5">Actions</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {filteredRuns.map(({ run, ctx }) => (
                  <tr key={run.id} className="hover:bg-gray-50">
                    <td className="px-3 py-1.5">
                      <Link
                        to={`/repos/${ctx.repoId}/worktrees/${ctx.worktreeId}/workflows/runs/${run.id}`}
                        className="text-indigo-600 hover:underline font-medium"
                      >
                        {run.workflow_name}
                      </Link>
                    </td>
                    <td className="px-3 py-1.5 text-gray-500">
                      <span className="inline-block px-1.5 py-0.5 text-[11px] font-mono rounded bg-gray-100 text-gray-600 mr-1">
                        {ctx.repoSlug}
                      </span>
                      {ctx.branch}
                    </td>
                    <td className="px-3 py-1.5">
                      <StatusBadge status={run.status} />
                      {run.dry_run && (
                        <span className="ml-1 text-[10px] px-1 py-0.5 bg-yellow-100 text-yellow-700 rounded">dry</span>
                      )}
                    </td>
                    <td className="px-3 py-1.5 text-xs text-gray-400">
                      <TimeAgo date={run.started_at} />
                    </td>
                    <td className="px-3 py-1.5 text-xs text-gray-500 font-mono tabular-nums">
                      {(() => {
                        const ms = runDurationMs(run);
                        return ms != null ? formatDuration(ms) : "\u2014";
                      })()}
                    </td>
                    <td className="px-3 py-1.5">
                      {(run.status === "running" || run.status === "waiting") && (
                        <button
                          onClick={() => handleCancelWorkflow(run.id)}
                          className="px-2 py-0.5 text-xs bg-red-100 text-red-700 rounded hover:bg-red-200"
                        >
                          Cancel
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
