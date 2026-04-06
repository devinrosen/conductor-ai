import type { Ticket, TicketDependencies } from "../api/types";

export interface TreePosition {
  depth: number;
  isLastSibling: boolean;
  ancestorsAreLast: boolean[];
  isParent: boolean;
}

/**
 * Build the Unicode tree-drawing prefix string for a given position.
 * Mirrors `TreePosition::to_prefix()` from conductor-tui/src/state/tree.rs.
 */
export function toTreePrefix(pos: TreePosition): string {
  if (pos.depth === 0) return "";
  let p = "";
  for (const ancestorIsLast of pos.ancestorsAreLast) {
    p += ancestorIsLast ? "  " : "│ ";
  }
  p += pos.isLastSibling ? "└ " : "├ ";
  return p;
}

/**
 * Returns true if the ticket has at least one unresolved (non-closed) blocker.
 * Mirrors `TicketDependencies::is_actively_blocked()`.
 */
export function isActivelyBlocked(
  ticketId: string,
  deps: Record<string, TicketDependencies>,
): boolean {
  const d = deps[ticketId];
  if (!d) return false;
  return d.blocked_by.some((b) => b.state !== "closed");
}

/**
 * Core DFS tree ordering — port of `dfs_tree_order` from tree.rs.
 *
 * Builds parent→children map, identifies roots, then DFS with explicit stack.
 * `reverse` sorts children descending (used for tickets: newer source_ids first).
 */
function dfsTreeOrder(
  n: number,
  getBranch: (i: number) => string,
  getParent: (i: number) => string,
  defaultBranch: string,
  reverse: boolean,
): { indices: number[]; positions: TreePosition[] } {
  if (n === 0) return { indices: [], positions: [] };

  const branchSet = new Set<string>();
  for (let i = 0; i < n; i++) branchSet.add(getBranch(i));

  const childrenOf = new Map<string, number[]>();
  for (let i = 0; i < n; i++) {
    const parent = getParent(i);
    let list = childrenOf.get(parent);
    if (!list) {
      list = [];
      childrenOf.set(parent, list);
    }
    list.push(i);
  }

  const roots: number[] = [];
  for (let i = 0; i < n; i++) {
    const parent = getParent(i);
    if (parent === defaultBranch || !branchSet.has(parent)) {
      roots.push(i);
    }
  }

  const sortFn = (a: number, b: number): number => {
    const cmp = getBranch(a).localeCompare(getBranch(b));
    return reverse ? -cmp : cmp;
  };

  roots.sort(sortFn);
  for (const children of childrenOf.values()) {
    children.sort(sortFn);
  }

  const indices: number[] = [];
  const positions: TreePosition[] = [];
  const visited = new Set<number>();

  // DFS via explicit stack: [index, depth, isLastSibling, ancestorsAreLast]
  const stack: [number, number, boolean, boolean[]][] = [];

  const rootCount = roots.length;
  for (let ri = roots.length - 1; ri >= 0; ri--) {
    stack.push([roots[ri], 0, ri === rootCount - 1, []]);
  }

  while (stack.length > 0) {
    const [idx, depth, isLast, ancestorsAreLast] = stack.pop()!;
    if (visited.has(idx)) continue;
    visited.add(idx);

    const branch = getBranch(idx);
    const children = childrenOf.get(branch);
    const hasChildren = children != null && children.length > 0;

    positions.push({
      depth,
      isLastSibling: isLast,
      ancestorsAreLast: [...ancestorsAreLast],
      isParent: hasChildren,
    });
    indices.push(idx);

    if (children) {
      const childAncestors = [...ancestorsAreLast, isLast];
      const len = children.length;
      for (let ci = children.length - 1; ci >= 0; ci--) {
        stack.push([children[ci], depth + 1, ci === len - 1, childAncestors]);
      }
    }
  }

  // Append any unvisited items (cycle members) as depth-0 roots
  for (let i = 0; i < n; i++) {
    if (!visited.has(i)) {
      const branch = getBranch(i);
      const children = childrenOf.get(branch);
      const hasChildren = children != null && children.length > 0;
      positions.push({
        depth: 0,
        isLastSibling: true,
        ancestorsAreLast: [],
        isParent: hasChildren,
      });
      indices.push(i);
      visited.add(i);
    }
  }

  return { indices, positions };
}

export interface TicketTreeResult {
  ordered: Ticket[];
  positions: TreePosition[];
  childToParent: Record<string, string>;
}

/**
 * Build ticket tree ordering from flat tickets + dependency map.
 * Port of `build_ticket_tree_indices` from tree.rs.
 *
 * Uses reverse-alphabetical sort (descending source_id) so newer tickets appear first,
 * matching TUI behavior.
 */
export function buildTicketTree(
  tickets: Ticket[],
  deps: Record<string, TicketDependencies>,
): TicketTreeResult {
  // Build child_id → parent_id reverse map
  const childToParent: Record<string, string> = {};
  for (const [parentId, dep] of Object.entries(deps)) {
    for (const child of dep.children) {
      childToParent[child.id] = parentId;
    }
  }

  const getBranch = (i: number) => tickets[i].id;
  const getParent = (i: number) => childToParent[tickets[i].id] ?? "";

  const { indices, positions } = dfsTreeOrder(
    tickets.length,
    getBranch,
    getParent,
    "",
    true,
  );

  const ordered = indices.map((i) => tickets[i]);
  return { ordered, positions, childToParent };
}
