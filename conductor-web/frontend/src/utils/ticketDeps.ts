import type { GithubPr, Ticket, WorkflowRun, Worktree } from "../api/types";

interface DepInfo {
  dependencies: string[];
  blocks: string[];
}

/** Parse dependency info from a ticket's raw_json. Non-vantage tickets return empty arrays. */
export function parseDeps(ticket: Ticket): DepInfo {
  if (ticket.source_type !== "vantage") {
    return { dependencies: [], blocks: [] };
  }
  try {
    const raw = JSON.parse(ticket.raw_json);
    return {
      dependencies: Array.isArray(raw.dependencies) ? raw.dependencies : [],
      blocks: Array.isArray(raw.blocks) ? raw.blocks : [],
    };
  } catch {
    return { dependencies: [], blocks: [] };
  }
}

export interface TicketTree {
  /** Top-level tickets (no unresolved dependencies in the list) */
  roots: Ticket[];
  /** Map from parent source_id to child tickets that depend on it */
  childMap: Map<string, Ticket[]>;
  /** Set of ticket IDs (conductor id) that are blocked by unresolved dependencies */
  blocked: Set<string>;
  /** Set of ticket IDs (conductor id) that are blocked but whose parents all have approved PRs */
  unlocked: Set<string>;
}

/**
 * Build a dependency tree from a flat ticket array.
 *
 * - Vantage tickets with `dependencies` are nested under their parents.
 * - A ticket is "blocked" if any dependency exists in the list and is not closed.
 * - A blocked ticket is "unlocked" if every blocking parent has an approved PR
 *   (GitHub review_decision "APPROVED") or a completed conductor workflow.
 * - Tickets with multiple dependencies appear under each parent.
 * - Non-vantage tickets are always roots.
 */
export function buildTicketTree(
  tickets: Ticket[],
  worktrees?: Worktree[],
  prs?: GithubPr[],
  workflowRunByTicketSourceId?: Map<string, WorkflowRun>,
): TicketTree {
  // Index tickets by source_id for fast lookup
  const bySourceId = new Map<string, Ticket>();
  const depsMap = new Map<string, DepInfo>();

  for (const t of tickets) {
    bySourceId.set(t.source_id, t);
    depsMap.set(t.source_id, parseDeps(t));
  }

  const childMap = new Map<string, Ticket[]>();
  const blocked = new Set<string>();
  const hasParentInList = new Set<string>(); // source_ids that appear as children

  // Track which parents block each ticket (for unlock computation)
  const blockingParents = new Map<string, string[]>(); // ticket id → parent source_ids

  for (const t of tickets) {
    if (t.source_type !== "vantage") continue;

    const deps = depsMap.get(t.source_id)!;
    let isBlocked = false;
    let nestedUnder: string | null = null;
    const blockers: string[] = [];

    for (const depId of deps.dependencies) {
      const parent = bySourceId.get(depId);
      if (!parent) continue; // dependency not in this list — don't block

      // Blocked if any parent is not closed
      if (parent.state !== "closed") {
        isBlocked = true;
        blockers.push(depId);
        // Nest under the first open parent
        if (!nestedUnder) {
          nestedUnder = depId;
        }
      }
    }

    if (nestedUnder) {
      hasParentInList.add(t.source_id);
      const children = childMap.get(nestedUnder) ?? [];
      children.push(t);
      childMap.set(nestedUnder, children);
    }

    if (isBlocked) {
      blocked.add(t.id);
      blockingParents.set(t.id, blockers);
    }
  }

  // Roots: tickets that have no parent in the list (or all parents are closed)
  const roots = tickets.filter(
    (t) => t.source_type !== "vantage" || !hasParentInList.has(t.source_id),
  );

  // Compute unlocked set: blocked tickets whose blocking parents all have approved PRs
  const unlocked = new Set<string>();
  if (worktrees?.length && prs?.length) {
    // ticket_id → worktree branch
    const wtBranchByTicketId = new Map<string, string>();
    for (const wt of worktrees) {
      if (wt.ticket_id) {
        wtBranchByTicketId.set(wt.ticket_id, wt.branch);
      }
    }
    // branch → PR
    const prByBranch = new Map<string, GithubPr>();
    for (const pr of prs) {
      prByBranch.set(pr.head_ref_name, pr);
    }

    for (const ticketId of blocked) {
      const parents = blockingParents.get(ticketId);
      if (!parents?.length) continue;

      const allApproved = parents.every((parentSourceId) => {
        const parent = bySourceId.get(parentSourceId);
        if (!parent) return false;
        // Check GitHub PR review decision
        const branch = wtBranchByTicketId.get(parent.id);
        if (branch) {
          const pr = prByBranch.get(branch);
          if (pr?.review_decision === "APPROVED") return true;
        }
        // Also consider approved if the parent's conductor workflow completed
        if (workflowRunByTicketSourceId) {
          const run = workflowRunByTicketSourceId.get(parentSourceId);
          if (run?.status === "completed") return true;
        }
        return false;
      });

      if (allApproved) {
        unlocked.add(ticketId);
      }
    }
  }

  return { roots, childMap, blocked, unlocked };
}
