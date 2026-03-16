import { memo, useCallback, useMemo, useState } from "react";
import { Link } from "react-router";
import type { WorkflowRun, Repo } from "../../api/types";
import { StatusBadge } from "../shared/StatusBadge";
import { TimeAgo } from "../shared/TimeAgo";

interface WorktreeCtx {
  repoId: string;
  worktreeId: string;
}

interface WorkflowRunTreeProps {
  runs: WorkflowRun[];
  repos: Repo[];
  ctxMap: Map<string, WorktreeCtx>;
  onCancel: (runId: string) => void;
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

const RunRow = memo(function RunRow({
  run,
  ctxMap,
  onCancel,
  indent,
}: {
  run: WorkflowRun;
  ctxMap: Map<string, WorktreeCtx>;
  onCancel: (runId: string) => void;
  indent: boolean;
}) {
  const ctx = run.worktree_id ? ctxMap.get(run.worktree_id) : undefined;

  const nameEl = ctx ? (
    <Link
      to={`/repos/${ctx.repoId}/worktrees/${ctx.worktreeId}/workflows/runs/${run.id}`}
      className="text-indigo-600 hover:underline text-sm font-medium truncate block"
    >
      {run.workflow_name}
    </Link>
  ) : (
    <span className="text-sm font-medium text-gray-800 truncate block">{run.workflow_name}</span>
  );

  return (
    <div className={`rounded border border-gray-100 bg-white p-3 mb-1 flex items-center justify-between gap-2${indent ? " ml-6 border-l-2 border-l-gray-200" : ""}`}>
      <div className="min-w-0 flex-1">{nameEl}</div>
      <div className="flex items-center gap-2 shrink-0">
        <StatusBadge status={run.status} />
        <span className="text-xs text-gray-400">
          <TimeAgo date={run.started_at} />
        </span>
        {(run.status === "running" || run.status === "waiting") && (
          <button
            onClick={() => onCancel(run.id)}
            className="px-2 py-0.5 text-xs bg-red-100 text-red-700 rounded hover:bg-red-200"
          >
            Cancel
          </button>
        )}
      </div>
    </div>
  );
});

export function WorkflowRunTree({ runs, repos, ctxMap, onCancel }: WorkflowRunTreeProps) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const toggle = useCallback((key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  // Build child set and children map
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

    // Sort children ASC by started_at
    for (const children of childrenMap.values()) {
      children.sort((a, b) => a.started_at.localeCompare(b.started_at));
    }

    return { childIds, childrenMap };
  }, [runs]);

  // Group root runs into (repoSlug → targetKey → runs[]) preserving first-seen order
  const { repoSlugs, repoGroups } = useMemo(() => {
    const repoSlugs: string[] = [];
    const repoGroups = new Map<string, Map<string, WorkflowRun[]>>();

    for (const run of runs) {
      if (childIds.has(run.id)) continue;

      let repoSlug = "unknown";
      let targetKey = "unknown";

      const repo = run.repo_id ? repos.find((r) => r.id === run.repo_id) : undefined;

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
  }, [runs, repos, childIds]);

  if (runs.length === 0) {
    return (
      <div className="text-center py-8 text-gray-400 text-sm">No active workflow runs</div>
    );
  }

  return (
    <div className="space-y-1">
      {repoSlugs.map((repoSlug) => {
        const repoKey = `repo:${repoSlug}`;
        const isRepoCollapsed = collapsed.has(repoKey);
        const targetGroups = repoGroups.get(repoSlug)!;

        return (
          <div key={repoSlug}>
            <button
              onClick={() => toggle(repoKey)}
              className="w-full flex items-center gap-1.5 py-1.5 px-2 text-sm font-semibold text-gray-700 hover:bg-gray-50 rounded"
            >
              <span className="text-xs text-gray-400">{isRepoCollapsed ? "▶" : "▼"}</span>
              <span>{repoSlug}</span>
            </button>

            {!isRepoCollapsed &&
              Array.from(targetGroups.keys()).map((targetKey) => {
                const targetGroupKey = `target:${repoSlug}/${targetKey}`;
                const isTargetCollapsed = collapsed.has(targetGroupKey);
                const targetRuns = targetGroups.get(targetKey)!;

                return (
                  <div key={targetKey} className="ml-4">
                    <button
                      onClick={() => toggle(targetGroupKey)}
                      className="w-full flex items-center gap-1.5 py-1 px-2 text-xs text-gray-500 hover:bg-gray-50 rounded"
                    >
                      <span className="text-xs text-gray-400">
                        {isTargetCollapsed ? "▶" : "▼"}
                      </span>
                      <span>{targetKey}</span>
                    </button>

                    {!isTargetCollapsed &&
                      targetRuns.map((run) => (
                        <div key={run.id} className="ml-4">
                          <RunRow run={run} ctxMap={ctxMap} onCancel={onCancel} indent={false} />
                          {childrenMap.get(run.id)?.map((child) => (
                            <RunRow
                              key={child.id}
                              run={child}
                              ctxMap={ctxMap}
                              onCancel={onCancel}
                              indent={true}
                            />
                          ))}
                        </div>
                      ))}
                  </div>
                );
              })}
          </div>
        );
      })}
    </div>
  );
}
