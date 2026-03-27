import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import type { WorkflowRun, WorkflowRunStep, Repo } from "../../api/types";
import { api } from "../../api/client";
import { StatusPulseBadge, PULSE_STATUSES } from "../shared/StatusPulseBadge";
import { TimeAgo } from "../shared/TimeAgo";
import { formatDuration, liveElapsedMs } from "../../utils/agentStats";

// Same palette as Sidebar RepoIndicator — keeps repo colors consistent
const REPO_COLORS = [
  "#2B5EA7", "#39B54A", "#FF9500", "#D73020", "#CD853F",
  "#00B5AD", "#9B0056", "#6CBE45", "#B36305", "#0098D4",
];

interface WorktreeCtx {
  repoId: string;
  worktreeId: string;
  repoSlug: string;
  branch: string;
}

interface WorkflowRunTreeProps {
  runs: WorkflowRun[];
  repos: Repo[];
  ctxMap: Map<string, WorktreeCtx>;
  onCancel?: (runId: string) => void;
}

type TargetType = "worktree" | "pr";

interface ParsedTarget {
  repoSlug: string;
  targetKey: string;
  type: TargetType;
}

function parseTargetLabel(label: string): ParsedTarget {
  if (label.includes("#")) {
    return { repoSlug: "unknown", targetKey: label, type: "pr" };
  }
  const slashPos = label.indexOf("/");
  if (slashPos !== -1) {
    return {
      repoSlug: label.slice(0, slashPos),
      targetKey: label.slice(slashPos + 1),
      type: "worktree",
    };
  }
  return { repoSlug: "unknown", targetKey: label, type: "worktree" };
}

// --- Icons ---

function RepoIcon({ color }: { color: string }) {
  return (
    <svg className="w-4 h-4 shrink-0" style={{ color }} viewBox="0 0 16 16" fill="currentColor">
      <path d="M2 2.5A2.5 2.5 0 0 1 4.5 0h8.75a.75.75 0 0 1 .75.75v12.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1 0-1.5h1.75v-2h-8a1 1 0 0 0-.714 1.7.75.75 0 1 1-1.072 1.05A2.495 2.495 0 0 1 2 11.5Zm10.5-1h-8a1 1 0 0 0-1 1v6.708A2.486 2.486 0 0 1 4.5 9h8ZM5 12.25a.25.25 0 0 1 .25-.25h3.5a.25.25 0 0 1 .25.25v3.25a.25.25 0 0 1-.4.2l-1.45-1.087a.249.249 0 0 0-.3 0L5.4 15.7a.25.25 0 0 1-.4-.2Z" />
    </svg>
  );
}

function BranchIcon({ color }: { color?: string }) {
  return (
    <svg className="w-4 h-4 shrink-0" style={{ color: color ?? "currentColor" }} viewBox="0 0 16 16" fill="currentColor">
      <path d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.493 2.493 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z" />
    </svg>
  );
}

function WorkflowIcon({ color }: { color?: string }) {
  return (
    <svg className="w-3.5 h-3.5 shrink-0" style={{ color: color ?? "currentColor" }} viewBox="0 0 16 16" fill="currentColor">
      <path d="M0 1.75C0 .784.784 0 1.75 0h3.5C6.216 0 7 .784 7 1.75v3.5A1.75 1.75 0 0 1 5.25 7H4v4a1 1 0 0 0 1 1h4v-1.25C9 9.784 9.784 9 10.75 9h3.5c.966 0 1.75.784 1.75 1.75v3.5A1.75 1.75 0 0 1 14.25 16h-3.5A1.75 1.75 0 0 1 9 14.25V13H5a2.5 2.5 0 0 1-2.5-2.5V7H1.75A1.75 1.75 0 0 1 0 5.25Zm1.75-.25a.25.25 0 0 0-.25.25v3.5c0 .138.112.25.25.25h3.5a.25.25 0 0 0 .25-.25v-3.5a.25.25 0 0 0-.25-.25Zm9 9.5a.25.25 0 0 0-.25.25v3.5c0 .138.112.25.25.25h3.5a.25.25 0 0 0 .25-.25v-3.5a.25.25 0 0 0-.25-.25Z" />
    </svg>
  );
}

function StatusIcon({ status }: { status: string }) {
  if (status === "completed") {
    return (
      <svg className="w-4 h-4 text-green-500 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.75.75 0 0 1 1.06-1.06L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z" />
      </svg>
    );
  }
  if (status === "failed" || status === "cancelled") {
    return (
      <svg className="w-4 h-4 text-red-500 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M3.72 3.72a.75.75 0 0 1 1.06 0L8 6.94l3.22-3.22a.75.75 0 1 1 1.06 1.06L9.06 8l3.22 3.22a.75.75 0 1 1-1.06 1.06L8 9.06l-3.22 3.22a.75.75 0 0 1-1.06-1.06L6.94 8 3.72 4.78a.75.75 0 0 1 0-1.06Z" />
      </svg>
    );
  }
  if (status === "running" || status === "waiting" || status === "pending") {
    return (
      <span className="relative flex w-4 h-4 shrink-0">
        <span className="absolute inset-0.5 rounded-full bg-amber-400/30 animate-ping" style={{ animationDuration: "2s" }} />
        <span className="relative inline-flex w-4 h-4 items-center justify-center">
          <span className="w-2 h-2 rounded-full bg-amber-500" />
        </span>
      </span>
    );
  }
  if (status === "skipped") {
    return (
      <svg className="w-4 h-4 text-gray-400 shrink-0" viewBox="0 0 16 16" fill="currentColor">
        <path d="M1.5 4.5a.75.75 0 0 1 1.28-.53l4.72 4.72 4.72-4.72a.75.75 0 1 1 1.06 1.06l-5.25 5.25a.75.75 0 0 1-1.06 0L1.72 5.03a.75.75 0 0 1-.22-.53Z" />
      </svg>
    );
  }
  return <span className="w-4 h-4 rounded-full border-2 border-gray-300 shrink-0" />;
}

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      className={`w-4.5 h-4.5 text-gray-400 transition-transform duration-150 ${open ? "rotate-90" : ""}`}
      viewBox="0 0 16 16"
      fill="currentColor"
    >
      <path d="M6.22 4.22a.75.75 0 0 1 1.06 0l3.25 3.25a.75.75 0 0 1 0 1.06l-3.25 3.25a.75.75 0 0 1-1.06-1.06L8.94 8 6.22 5.28a.75.75 0 0 1 0-1.06Z" />
    </svg>
  );
}

// --- Components ---

const MAX_STEP_NAMES = 3;

function StepLeaves({ steps }: { steps: WorkflowRunStep[] }) {
  if (steps.length === 0) return null;

  const label =
    steps.length > MAX_STEP_NAMES
      ? `${steps.length} steps running`
      : steps.map((s) => s.step_name).join(", ");

  const isWaiting = steps.every((s) => s.status === "waiting");

  return (
    <div className="ml-4 mb-1 flex items-center gap-2 px-3 py-1.5 rounded border border-gray-100 bg-gray-50">
      <span className="shrink-0">
        {isWaiting ? (
          <span className="inline-block w-2 h-2 rounded-full bg-amber-400" />
        ) : (
          <span className="inline-block w-2 h-2 rounded-full bg-indigo-500 animate-pulse" />
        )}
      </span>
      <span className="text-xs text-gray-500 truncate">{label}</span>
    </div>
  );
}

function runDurationMs(run: WorkflowRun): number | null {
  if (run.ended_at) return new Date(run.ended_at).getTime() - new Date(run.started_at).getTime();
  if (run.status === "running" || run.status === "waiting") return liveElapsedMs(run.started_at);
  return null;
}

const RunRow = memo(function RunRow({
  run,
  ctxMap,
  indent,
  onCancel,
}: {
  run: WorkflowRun;
  ctxMap: Map<string, WorktreeCtx>;
  indent: boolean;
  onCancel?: (runId: string) => void;
}) {
  const ctx = run.worktree_id ? ctxMap.get(run.worktree_id) : undefined;

  const nameEl = ctx ? (
    <Link
      to={`/repos/${ctx.repoId}/worktrees/${ctx.worktreeId}/workflows/runs/${run.id}`}
      className="text-sm truncate block hover:underline"
    >
      {run.workflow_name}
    </Link>
  ) : (
    <span className="text-sm truncate block">{run.workflow_name}</span>
  );

  const isActive = run.status === "running" || run.status === "waiting";
  const ms = runDurationMs(run);

  return (
    <div className={`flex items-center justify-between gap-2 px-3 py-2 mb-0.5 ${indent ? "ml-6" : ""}`}>
      <div className="flex items-center gap-2 min-w-0 flex-1">
        <StatusIcon status={run.status} />
        {nameEl}
      </div>
      <div className="flex items-center gap-2 shrink-0 text-xs text-gray-400">
        {PULSE_STATUSES.has(run.status) && (
          <StatusPulseBadge status={run.status} />
        )}
        {ms != null && (
          <span className="font-mono tabular-nums">{formatDuration(ms)}</span>
        )}
        <TimeAgo date={run.started_at} short />
        {isActive && onCancel && (
          <button
            onClick={(e) => { e.preventDefault(); onCancel(run.id); }}
            className="px-2 py-0.5 text-xs bg-red-100 text-red-700 rounded hover:bg-red-200"
          >
            Cancel
          </button>
        )}
      </div>
    </div>
  );
});

/** A single child workflow run with lazy-loaded expandable steps */
function ChildRunWithSteps({ run, ctxMap, onCancel }: {
  run: WorkflowRun;
  ctxMap: Map<string, WorktreeCtx>;
  onCancel?: (runId: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [steps, setSteps] = useState<WorkflowRunStep[] | null>(null);
  const [stepError, setStepError] = useState(false);

  useEffect(() => {
    if (!expanded || steps !== null) return;
    setStepError(false);
    api.getWorkflowSteps(run.id)
      .then((data) => setSteps(data.slice().sort((a, b) => a.position - b.position)))
      .catch(() => setStepError(true));
  }, [expanded, steps, run.id]);

  const isActive = run.status === "running" || run.status === "waiting";
  const ms = runDurationMs(run);
  const ctx = run.worktree_id ? ctxMap.get(run.worktree_id) : undefined;

  return (
    <div className="ml-6">
      <div
        className="flex items-center justify-between gap-2 px-3 py-1.5 cursor-pointer hover:bg-gray-50/50 rounded transition-colors"
        onClick={() => setExpanded(!expanded)}
      >
        <div className="flex items-center gap-2 min-w-0 flex-1">
          <Chevron open={expanded} />
          <StatusIcon status={run.status} />
          {ctx ? (
            <Link
              to={`/repos/${ctx.repoId}/worktrees/${ctx.worktreeId}/workflows/runs/${run.id}`}
              className="text-xs truncate hover:underline"
              onClick={(e) => e.stopPropagation()}
            >
              {run.workflow_name}
            </Link>
          ) : (
            <span className="text-xs truncate">{run.workflow_name}</span>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0 text-xs text-gray-400">
          {ms != null && (
            <span className="font-mono tabular-nums">{formatDuration(ms)}</span>
          )}
          {isActive && onCancel && (
            <button
              onClick={(e) => { e.preventDefault(); e.stopPropagation(); onCancel(run.id); }}
              className="px-2 py-0.5 text-xs bg-red-100 text-red-700 rounded hover:bg-red-200"
            >
              Cancel
            </button>
          )}
        </div>
      </div>
      {expanded && (
        <div className="ml-8 border-l border-gray-600/30 pl-3 mb-1">
          {stepError ? (
            <p className="text-xs text-red-400 py-1">
              Failed to load steps —{" "}
              <button onClick={() => { setStepError(false); setSteps(null); }} className="underline hover:text-red-600">retry</button>
            </p>
          ) : steps === null ? (
            <p className="text-xs text-gray-400 py-1">Loading steps...</p>
          ) : steps.length === 0 ? (
            <p className="text-xs text-gray-400 py-1">No steps</p>
          ) : (
            steps.map((step) => {
              const stepMs = step.ended_at && step.started_at
                ? new Date(step.ended_at).getTime() - new Date(step.started_at).getTime()
                : (step.status === "running" || step.status === "waiting") && step.started_at
                  ? liveElapsedMs(step.started_at) : null;
              return (
                <div key={step.id} className="flex items-center justify-between gap-2 py-0.5">
                  <div className="flex items-center gap-2 min-w-0">
                    <StatusIcon status={step.status} />
                    <span className="text-xs">{step.step_name}</span>
                    {step.role && step.role !== "workflow" && (
                      <span className="text-[10px] px-1 py-0.5 bg-gray-100 text-gray-500 rounded">{step.role}</span>
                    )}
                  </div>
                  {stepMs != null && (
                    <span className="text-xs text-gray-400 font-mono tabular-nums shrink-0">{formatDuration(stepMs)}</span>
                  )}
                </div>
              );
            })
          )}
        </div>
      )}
    </div>
  );
}

/** Collapsible child workflows with lazy step loading */
function ChildRuns({ children, ctxMap, toggle, isOpen, onCancel, color }: {
  children: WorkflowRun[];
  ctxMap: Map<string, WorktreeCtx>;
  toggle: () => void;
  isOpen: boolean;
  onCancel?: (runId: string) => void;
  color?: string;
}) {
  if (children.length === 0) return null;

  const allDone = children.every((c) => c.status === "completed");
  const anyFailed = children.some((c) => c.status === "failed");
  const anyActive = children.some((c) => c.status === "running" || c.status === "waiting");

  return (
    <div className="ml-6 mb-1">
      <button
        onClick={toggle}
        className="flex items-center gap-1.5 px-2 py-1 text-xs text-gray-500 hover:text-gray-700 rounded hover:bg-gray-50/50 transition-colors"
      >
        <Chevron open={isOpen} />
        <WorkflowIcon color={color} />
        <span>
          {children.length} sub-workflow{children.length !== 1 ? "s" : ""}
        </span>
        {!isOpen && (
          <span className={`ml-0.5 ${anyFailed ? "text-red-500" : anyActive ? "text-amber-500" : allDone ? "text-green-500" : "text-gray-400"}`}>
            {anyFailed ? "— failed" : anyActive ? "— running" : allDone ? "— done" : ""}
          </span>
        )}
      </button>
      {isOpen && (
        <div className="mt-0.5">
          {children.map((child) => (
            <ChildRunWithSteps
              key={child.id}
              run={child}
              ctxMap={ctxMap}
              onCancel={onCancel}
            />
          ))}
        </div>
      )}
    </div>
  );
}

export function WorkflowRunTree({ runs, repos, ctxMap, onCancel }: WorkflowRunTreeProps) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const initialized = useRef(false);

  const toggle = useCallback((key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const { childIds, childrenMap } = useMemo(() => {
    const childIds = new Set<string>();
    const childrenMap = new Map<string, WorkflowRun[]>();

    for (const run of runs) {
      if (run.parent_workflow_run_id) {
        childIds.add(run.id);
        if (!childrenMap.has(run.parent_workflow_run_id)) {
          childrenMap.set(run.parent_workflow_run_id, []);
        }
        childrenMap.get(run.parent_workflow_run_id)!.push(run);
      }
    }

    for (const children of childrenMap.values()) {
      children.sort((a, b) => a.started_at.localeCompare(b.started_at));
    }

    return { childIds, childrenMap };
  }, [runs]);

  const repoById = useMemo(() => new Map(repos.map((r) => [r.id, r])), [repos]);
  // Map repo slugs to their sidebar color (same index order as repos array)
  const repoColorMap = useMemo(() => {
    const m = new Map<string, string>();
    repos.forEach((r, i) => m.set(r.slug, REPO_COLORS[i % REPO_COLORS.length]));
    return m;
  }, [repos]);

  const { repoSlugs, repoGroups } = useMemo(() => {
    const repoSlugs: string[] = [];
    const repoGroups = new Map<string, Map<string, WorkflowRun[]>>();

    for (const run of runs) {
      if (childIds.has(run.id)) continue;

      let repoSlug = "unknown";
      let targetKey = "unknown";

      const repo = run.repo_id ? repoById.get(run.repo_id) : undefined;

      if (run.target_label) {
        const parsed = parseTargetLabel(run.target_label);
        repoSlug = parsed.repoSlug;
        targetKey = parsed.targetKey;
        if (repoSlug === "unknown" && repo) repoSlug = repo.slug;
      } else if (repo) {
        repoSlug = repo.slug;
      }

      if (!repoGroups.has(repoSlug)) {
        repoGroups.set(repoSlug, new Map());
        repoSlugs.push(repoSlug);
      }
      const targetGroups = repoGroups.get(repoSlug)!;
      if (!targetGroups.has(targetKey)) {
        targetGroups.set(targetKey, []);
      }
      targetGroups.get(targetKey)!.push(run);
    }

    return { repoSlugs, repoGroups };
  }, [runs, repoById, childIds]);

  // Collapse everything on first data load
  useEffect(() => {
    if (initialized.current || runs.length === 0) return;
    initialized.current = true;
    const keys = new Set<string>();
    for (const slug of repoSlugs) {
      keys.add(`repo:${slug}`);
      const targets = repoGroups.get(slug);
      if (targets) {
        for (const [targetKey, targetRuns] of targets) {
          keys.add(`target:${slug}/${targetKey}`);
          for (const run of targetRuns) {
            if (childrenMap.has(run.id)) {
              keys.add(`children:${run.id}`);
            }
          }
        }
      }
    }
    setCollapsed(keys);
  }, [runs.length, repoSlugs, repoGroups, childrenMap]);

  if (runs.length === 0) {
    return (
      <div className="text-center py-8 text-gray-400 text-sm">No active workflow runs</div>
    );
  }

  return (
    <div className="space-y-1">
      {repoSlugs.map((repoSlug) => {
        const repoKey = `repo:${repoSlug}`;
        const isRepoOpen = !collapsed.has(repoKey);
        const targetGroups = repoGroups.get(repoSlug)!;
        const repoColor = repoColorMap.get(repoSlug) ?? REPO_COLORS[0];

        return (
          <div key={repoSlug} className="mb-1">
            <button
              onClick={() => toggle(repoKey)}
              className="w-full flex items-center gap-1.5 py-1.5 px-2 text-sm font-semibold text-gray-700 hover:bg-gray-50 rounded transition-colors"
            >
              <Chevron open={isRepoOpen} />
              <RepoIcon color={repoColor} />
              <span>{repoSlug}</span>
            </button>

            {isRepoOpen &&
              Array.from(targetGroups.keys()).map((targetKey) => {
                const targetGroupKey = `target:${repoSlug}/${targetKey}`;
                const isTargetOpen = !collapsed.has(targetGroupKey);
                const targetRuns = targetGroups.get(targetKey)!;

                return (
                  <div key={targetKey} className={`ml-4 mb-1 ${isTargetOpen ? "rounded-lg p-2 border-l-2" : ""}`} style={isTargetOpen ? { borderLeftColor: repoColor } : undefined}>
                    <button
                      onClick={() => toggle(targetGroupKey)}
                      className="w-full flex items-center gap-1.5 py-1 px-2 text-sm text-gray-600 hover:text-gray-800 hover:bg-gray-50 rounded transition-colors"
                    >
                      <Chevron open={isTargetOpen} />
                      <BranchIcon color={repoColor} />
                      <span>{targetKey}</span>
                    </button>

                    {isTargetOpen &&
                      targetRuns.map((run) => {
                        const children = childrenMap.get(run.id) ?? [];
                        const childKey = `children:${run.id}`;
                        const isChildrenOpen = !collapsed.has(childKey);
                        return (
                        <div key={run.id} className="ml-4">
                          <RunRow run={run} ctxMap={ctxMap} indent={false} onCancel={onCancel} />
                          <StepLeaves steps={run.active_steps ?? []} />
                          <ChildRuns
                            children={children}
                            ctxMap={ctxMap}
                            isOpen={isChildrenOpen}
                            toggle={() => toggle(childKey)}
                            onCancel={onCancel}
                            color={repoColor}
                          />
                        </div>
                        );
                      })}
                  </div>
                );
              })}
          </div>
        );
      })}
    </div>
  );
}
