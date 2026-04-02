import type { Ticket } from "../api/types";

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
}

/**
 * Build a dependency tree from a flat ticket array.
 *
 * - Vantage tickets with `dependencies` are nested under their parents.
 * - A ticket is "blocked" if any dependency exists in the list and is not closed.
 * - Tickets with multiple dependencies appear under each parent.
 * - Non-vantage tickets are always roots.
 */
export function buildTicketTree(tickets: Ticket[]): TicketTree {
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

  for (const t of tickets) {
    if (t.source_type !== "vantage") continue;

    const deps = depsMap.get(t.source_id)!;
    let isBlocked = false;

    for (const depId of deps.dependencies) {
      const parent = bySourceId.get(depId);
      if (!parent) continue; // dependency not in this list — don't block

      // This ticket depends on a parent in the list
      hasParentInList.add(t.source_id);

      // Add to parent's children
      const children = childMap.get(depId) ?? [];
      children.push(t);
      childMap.set(depId, children);

      // Blocked if parent is not closed
      if (parent.state !== "closed") {
        isBlocked = true;
      }
    }

    if (isBlocked) {
      blocked.add(t.id);
    }
  }

  // Roots: tickets that have no parent in the list
  const roots = tickets.filter(
    (t) => t.source_type !== "vantage" || !hasParentInList.has(t.source_id),
  );

  return { roots, childMap, blocked };
}
