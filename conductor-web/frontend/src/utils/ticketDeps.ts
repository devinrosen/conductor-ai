import type { Ticket, TicketDependencies } from "../api/types";

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
 * - A blocked ticket is "unlocked" if every blocker has `workflow_completed === true`.
 * - Non-vantage tickets without apiDeps are always roots.
 */
export function buildTicketTree(
  tickets: Ticket[],
  _worktrees?: unknown,
  _prs?: unknown,
  apiDeps?: Record<string, TicketDependencies>,
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

  // Compute unlocked set: blocked tickets whose blocking parents all have
  // completed ticket-to-pr workflows (determined by the backend).
  const unlocked = new Set<string>();
  for (const ticketId of blocked) {
    const parentIds = blockingParentIds.get(ticketId);
    if (!parentIds?.length) continue;

    const allCompleted = parentIds.every((parentId) => {
      const parent = byId.get(parentId);
      return parent?.workflow_completed === true;
    });

    if (allCompleted) {
      unlocked.add(ticketId);
    }
  }

  return { roots, childMap, blocked, unlocked };
}
