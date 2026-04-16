import type { GithubPr, Ticket, TicketDependencies, Worktree } from "../api/types";

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
 * - When `apiDeps` is provided (preferred), uses DB-backed dependency data for all
 *   source types. Falls back to Vantage `raw_json` parsing when `apiDeps` is absent.
 * - A ticket is "blocked" if any blocker in `blocked_by` is not closed.
 * - A blocked ticket is "unlocked" if every blocker has a PR with review_decision "APPROVED",
 *   OR if every blocker's Vantage conductor.status is in `terminalStatuses`.
 * - Non-vantage tickets without apiDeps are always roots.
 * - `terminalStatuses` is fetched from GET /api/vantage/terminal-statuses; when absent
 *   the Vantage conductor.status check is skipped (PR-approval-only fallback).
 */
export function buildTicketTree(
  tickets: Ticket[],
  worktrees?: Worktree[],
  prs?: GithubPr[],
  apiDeps?: Record<string, TicketDependencies>,
  terminalStatuses?: string[],
): TicketTree {
  // Index tickets by source_id and by id for fast lookup
  const bySourceId = new Map<string, Ticket>();
  const byId = new Map<string, Ticket>();

  for (const t of tickets) {
    bySourceId.set(t.source_id, t);
    byId.set(t.id, t);
  }

  const childMap = new Map<string, Ticket[]>();
  const blocked = new Set<string>();
  const hasParentInList = new Set<string>(); // source_ids that appear as children

  // Track which blocking parents each ticket has (for unlock computation)
  // Value is the parent ticket IDs (conductor id)
  const blockingParentIds = new Map<string, string[]>(); // ticket id → blocker ticket ids

  if (apiDeps) {
    // API-provided deps path: works for all source types
    for (const t of tickets) {
      const deps = apiDeps[t.id];
      if (!deps) continue;

      // Nest under parent if present and open
      if (deps.parent && byId.has(deps.parent.id) && deps.parent.state !== "closed") {
        hasParentInList.add(t.source_id);
        const siblings = childMap.get(deps.parent.source_id) ?? [];
        siblings.push(t);
        childMap.set(deps.parent.source_id, siblings);
      }

      // Blocked if any blocker is open and in the list
      const activeBlockers = deps.blocked_by.filter(
        (b) => b.state !== "closed" && byId.has(b.id),
      );
      if (activeBlockers.length > 0) {
        blocked.add(t.id);
        blockingParentIds.set(t.id, activeBlockers.map((b) => b.id));
      }
    }
  } else {
    // Fallback: Vantage raw_json path
    const depsMap = new Map<string, DepInfo>();
    for (const t of tickets) {
      depsMap.set(t.source_id, parseDeps(t));
    }

    for (const t of tickets) {
      if (t.source_type !== "vantage") continue;

      const deps = depsMap.get(t.source_id)!;
      let isBlocked = false;
      let nestedUnder: string | null = null;
      const blockers: string[] = [];

      for (const depId of deps.dependencies) {
        const parent = bySourceId.get(depId);
        if (!parent) continue;

        if (parent.state !== "closed") {
          isBlocked = true;
          blockers.push(depId);
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
        // In fallback path, blockers are source_ids; store parent.id for unlock
        blockingParentIds.set(
          t.id,
          blockers.map((sid) => bySourceId.get(sid)?.id).filter(Boolean) as string[],
        );
      }
    }
  }

  // Roots: tickets that have no parent in the list
  const roots = tickets.filter((t) => !hasParentInList.has(t.source_id));

  // Compute unlocked set: blocked tickets whose blocking parents all have approved PRs
  // or a terminal Vantage conductor.status.
  const unlocked = new Set<string>();

  // Build lookup maps for PR approval (only useful when worktrees and prs are available)
  const wtBranchByTicketId = new Map<string, string>();
  if (worktrees?.length) {
    for (const wt of worktrees) {
      if (wt.ticket_id) {
        wtBranchByTicketId.set(wt.ticket_id, wt.branch);
      }
    }
  }
  const prByBranch = new Map<string, GithubPr>();
  if (prs?.length) {
    for (const pr of prs) {
      prByBranch.set(pr.head_ref_name, pr);
    }
  }

  // Pre-parse raw_json once per ticket to avoid repeated JSON.parse in the inner loop
  const parsedRawById = new Map<string, unknown>();
  if (terminalStatuses?.length) {
    for (const ticket of tickets) {
      if (ticket.source_type === "vantage") {
        try {
          parsedRawById.set(ticket.id, JSON.parse(ticket.raw_json));
        } catch {
          // malformed raw_json — leave absent from map
        }
      }
    }
  }

  if (blocked.size > 0 && (wtBranchByTicketId.size > 0 || terminalStatuses?.length)) {
    for (const ticketId of blocked) {
      const parentIds = blockingParentIds.get(ticketId);
      if (!parentIds?.length) continue;

      const allApproved = parentIds.every((parentId) => {
        // Check PR approval
        const branch = wtBranchByTicketId.get(parentId);
        if (branch) {
          const pr = prByBranch.get(branch);
          if (pr?.review_decision === "APPROVED") return true;
        }
        // Check Vantage conductor.status against the backend-provided terminal list
        if (terminalStatuses?.length) {
          const parent = byId.get(parentId);
          if (parent?.source_type === "vantage") {
            const raw = parsedRawById.get(parentId) as { conductor?: { status?: string } } | undefined;
            const conductorStatus = raw?.conductor?.status;
            if (conductorStatus && terminalStatuses.includes(conductorStatus)) {
              return true;
            }
          }
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
