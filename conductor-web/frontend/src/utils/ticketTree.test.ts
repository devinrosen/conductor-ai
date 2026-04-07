import { describe, it, expect } from "vitest";
import type { Ticket, TicketDependencies } from "../api/types";
import {
  toTreePrefix,
  isActivelyBlocked,
  buildTicketTree,
  type TreePosition,
} from "./ticketTree";

/** Helper: minimal Ticket stub with only fields the tree logic inspects. */
function ticket(id: string, sourceId: string, state = "open"): Ticket {
  return {
    id,
    repo_id: "r1",
    source_type: "github",
    source_id: sourceId,
    title: `Ticket ${sourceId}`,
    body: "",
    state,
    labels: "",
    assignee: null,
    priority: null,
    url: "",
    synced_at: "",
    raw_json: "{}",
    workflow: null,
    agent_map: null,
  } as Ticket;
}

function emptyDeps(): TicketDependencies {
  return { blocked_by: [], blocks: [], parent: null, children: [] };
}

// ─── toTreePrefix ─────────────────────────────────────────────────────────────

describe("toTreePrefix", () => {
  it("returns empty string for depth 0", () => {
    const pos: TreePosition = {
      depth: 0,
      isLastSibling: true,
      ancestorsAreLast: [],
      isParent: false,
    };
    expect(toTreePrefix(pos)).toBe("");
  });

  it("renders └ for last sibling at depth 1", () => {
    const pos: TreePosition = {
      depth: 1,
      isLastSibling: true,
      ancestorsAreLast: [],
      isParent: false,
    };
    expect(toTreePrefix(pos)).toBe("└ ");
  });

  it("renders ├ for non-last sibling at depth 1", () => {
    const pos: TreePosition = {
      depth: 1,
      isLastSibling: false,
      ancestorsAreLast: [],
      isParent: false,
    };
    expect(toTreePrefix(pos)).toBe("├ ");
  });

  it("renders ancestor continuation lines at depth 2+", () => {
    const pos: TreePosition = {
      depth: 2,
      isLastSibling: true,
      ancestorsAreLast: [false],
      isParent: false,
    };
    // ancestor not last → "│ ", then last sibling → "└ "
    expect(toTreePrefix(pos)).toBe("│ └ ");
  });

  it("renders spaces for last ancestors", () => {
    const pos: TreePosition = {
      depth: 2,
      isLastSibling: false,
      ancestorsAreLast: [true],
      isParent: false,
    };
    // ancestor is last → "  ", then not last sibling → "├ "
    expect(toTreePrefix(pos)).toBe("  ├ ");
  });
});

// ─── isActivelyBlocked ────────────────────────────────────────────────────────

describe("isActivelyBlocked", () => {
  it("returns false when ticket has no deps entry", () => {
    expect(isActivelyBlocked("t1", {})).toBe(false);
  });

  it("returns false when blocked_by is empty", () => {
    const deps: Record<string, TicketDependencies> = {
      t1: { ...emptyDeps(), blocked_by: [] },
    };
    expect(isActivelyBlocked("t1", deps)).toBe(false);
  });

  it("returns false when all blockers are closed", () => {
    const deps: Record<string, TicketDependencies> = {
      t1: {
        ...emptyDeps(),
        blocked_by: [ticket("t2", "2", "closed")],
      },
    };
    expect(isActivelyBlocked("t1", deps)).toBe(false);
  });

  it("returns true when at least one blocker is open", () => {
    const deps: Record<string, TicketDependencies> = {
      t1: {
        ...emptyDeps(),
        blocked_by: [
          ticket("t2", "2", "closed"),
          ticket("t3", "3", "open"),
        ],
      },
    };
    expect(isActivelyBlocked("t1", deps)).toBe(true);
  });
});

// ─── buildTicketTree ──────────────────────────────────────────────────────────

describe("buildTicketTree", () => {
  it("returns flat ordering when there are no dependencies", () => {
    const tickets = [ticket("a", "3"), ticket("b", "1"), ticket("c", "2")];
    const deps: Record<string, TicketDependencies> = {};
    const result = buildTicketTree(tickets, deps);

    // All depth-0, reverse-sorted by id (descending)
    expect(result.ordered.map((t) => t.id)).toEqual(["c", "b", "a"]);
    expect(result.positions.every((p) => p.depth === 0)).toBe(true);
  });

  it("nests children under parents", () => {
    const parent = ticket("p1", "10");
    const child = ticket("c1", "11");
    const tickets = [child, parent];

    const deps: Record<string, TicketDependencies> = {
      p1: { ...emptyDeps(), children: [child] },
      c1: { ...emptyDeps(), parent },
    };

    const result = buildTicketTree(tickets, deps);

    // Parent first, child indented
    expect(result.ordered.map((t) => t.id)).toEqual(["p1", "c1"]);
    expect(result.positions[0].depth).toBe(0);
    expect(result.positions[1].depth).toBe(1);
  });

  it("handles multi-level nesting", () => {
    const grandparent = ticket("gp", "1");
    const parent = ticket("p", "2");
    const child = ticket("c", "3");
    const tickets = [child, parent, grandparent];

    const deps: Record<string, TicketDependencies> = {
      gp: { ...emptyDeps(), children: [parent] },
      p: { ...emptyDeps(), parent: grandparent, children: [child] },
      c: { ...emptyDeps(), parent },
    };

    const result = buildTicketTree(tickets, deps);

    expect(result.ordered.map((t) => t.id)).toEqual(["gp", "p", "c"]);
    expect(result.positions.map((p) => p.depth)).toEqual([0, 1, 2]);
  });

  it("handles cycles gracefully by appending unvisited nodes", () => {
    // Create a cycle: a → b → a (both claim the other as child)
    const a = ticket("a", "1");
    const b = ticket("b", "2");
    const tickets = [a, b];

    const deps: Record<string, TicketDependencies> = {
      a: { ...emptyDeps(), children: [b] },
      b: { ...emptyDeps(), children: [a] },
    };

    const result = buildTicketTree(tickets, deps);

    // Both should appear exactly once
    expect(result.ordered).toHaveLength(2);
    const ids = result.ordered.map((t) => t.id);
    expect(ids).toContain("a");
    expect(ids).toContain("b");
  });

  it("populates childToParent map", () => {
    const parent = ticket("p1", "10");
    const child1 = ticket("c1", "11");
    const child2 = ticket("c2", "12");
    const tickets = [child1, child2, parent];

    const deps: Record<string, TicketDependencies> = {
      p1: { ...emptyDeps(), children: [child1, child2] },
    };

    const result = buildTicketTree(tickets, deps);
    expect(result.childToParent["c1"]).toBe("p1");
    expect(result.childToParent["c2"]).toBe("p1");
    expect(result.childToParent["p1"]).toBeUndefined();
  });
});
