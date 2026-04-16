import { describe, it, expect } from "vitest";
import { buildTicketTree, parseDeps } from "./ticketDeps";
import type { Ticket, Worktree, GithubPr, TicketDependencies } from "../api/types";

function makeTicket(overrides: Partial<Ticket> & { id: string; source_id: string }): Ticket {
  return {
    source_type: "vantage",
    title: "Test ticket",
    state: "open",
    assignee: null,
    raw_json: "{}",
    repo_id: "repo-1",
    body: "",
    labels: "",
    priority: null,
    url: "",
    synced_at: "2024-01-01T00:00:00Z",
    ...overrides,
  };
}

function makeWorktree(ticketId: string, branch: string): Worktree {
  return {
    id: `wt-${ticketId}`,
    repo_id: "repo-1",
    slug: branch,
    branch,
    ticket_id: ticketId,
    status: "active",
    path: `/tmp/${branch}`,
    created_at: "2024-01-01T00:00:00Z",
    completed_at: null,
    model: null,
  };
}

function makePr(headRef: string, reviewDecision: string | null): GithubPr {
  return {
    number: 1,
    title: "Test PR",
    url: "https://github.com/test/repo/pull/1",
    author: "test-user",
    state: "open",
    head_ref_name: headRef,
    is_draft: false,
    review_decision: reviewDecision,
    ci_status: "success",
  };
}

describe("parseDeps", () => {
  it("returns empty arrays for non-vantage tickets", () => {
    const ticket = makeTicket({ id: "t1", source_id: "1", source_type: "github" });
    expect(parseDeps(ticket)).toEqual({ dependencies: [], blocks: [] });
  });

  it("parses dependencies and blocks from valid raw_json", () => {
    const ticket = makeTicket({
      id: "t1",
      source_id: "1",
      raw_json: JSON.stringify({ dependencies: ["2", "3"], blocks: ["4"] }),
    });
    expect(parseDeps(ticket)).toEqual({ dependencies: ["2", "3"], blocks: ["4"] });
  });

  it("returns empty arrays on malformed raw_json", () => {
    const ticket = makeTicket({ id: "t1", source_id: "1", raw_json: "not-json{" });
    expect(parseDeps(ticket)).toEqual({ dependencies: [], blocks: [] });
  });

  it("returns empty arrays when dependencies/blocks fields are missing", () => {
    const ticket = makeTicket({ id: "t1", source_id: "1", raw_json: '{"other":"field"}' });
    expect(parseDeps(ticket)).toEqual({ dependencies: [], blocks: [] });
  });
});

describe("buildTicketTree — terminal status checks", () => {
  it("unlocks a blocked ticket when parent conductor.status is in terminalStatuses", () => {
    const parent = makeTicket({
      id: "parent-1",
      source_id: "P1",
      raw_json: JSON.stringify({ conductor: { status: "merged" } }),
    });
    const child = makeTicket({ id: "child-1", source_id: "C1" });

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    const tree = buildTicketTree(
      [parent, child],
      [],
      [],
      apiDeps,
      ["merged", "pr_approved", "released"],
    );

    expect(tree.blocked.has("child-1")).toBe(true);
    expect(tree.unlocked.has("child-1")).toBe(true);
  });

  it("does not unlock when conductor.status is not in terminalStatuses", () => {
    const parent = makeTicket({
      id: "parent-1",
      source_id: "P1",
      raw_json: JSON.stringify({ conductor: { status: "in_progress" } }),
    });
    const child = makeTicket({ id: "child-1", source_id: "C1" });

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    const tree = buildTicketTree(
      [parent, child],
      [],
      [],
      apiDeps,
      ["merged", "pr_approved", "released"],
    );

    expect(tree.blocked.has("child-1")).toBe(true);
    expect(tree.unlocked.has("child-1")).toBe(false);
  });

  it("skips terminal status check when terminalStatuses is absent", () => {
    const parent = makeTicket({
      id: "parent-1",
      source_id: "P1",
      raw_json: JSON.stringify({ conductor: { status: "merged" } }),
    });
    const child = makeTicket({ id: "child-1", source_id: "C1" });
    const worktrees = [makeWorktree("parent-1", "feat/parent")];
    // No approved PR for the parent's branch
    const prs = [makePr("feat/parent", null)];

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    // No terminalStatuses passed — should not unlock via conductor.status
    const tree = buildTicketTree([parent, child], worktrees, prs, apiDeps, undefined);

    expect(tree.blocked.has("child-1")).toBe(true);
    expect(tree.unlocked.has("child-1")).toBe(false);
  });

  it("does not unlock when parent raw_json is malformed", () => {
    const parent = makeTicket({ id: "parent-1", source_id: "P1", raw_json: "bad{json" });
    const child = makeTicket({ id: "child-1", source_id: "C1" });

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    const tree = buildTicketTree(
      [parent, child],
      [],
      [],
      apiDeps,
      ["merged", "pr_approved", "released"],
    );

    expect(tree.blocked.has("child-1")).toBe(true);
    expect(tree.unlocked.has("child-1")).toBe(false);
  });
});

describe("buildTicketTree — PR approval checks", () => {
  it("unlocks a blocked ticket when the parent branch has an approved PR", () => {
    const parent = makeTicket({ id: "parent-1", source_id: "P1" });
    const child = makeTicket({ id: "child-1", source_id: "C1" });
    const worktrees = [makeWorktree("parent-1", "feat/parent")];
    const prs = [makePr("feat/parent", "APPROVED")];

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    const tree = buildTicketTree([parent, child], worktrees, prs, apiDeps);

    expect(tree.blocked.has("child-1")).toBe(true);
    expect(tree.unlocked.has("child-1")).toBe(true);
  });

  it("does not unlock when PR exists but is not approved", () => {
    const parent = makeTicket({ id: "parent-1", source_id: "P1" });
    const child = makeTicket({ id: "child-1", source_id: "C1" });
    const worktrees = [makeWorktree("parent-1", "feat/parent")];
    const prs = [makePr("feat/parent", "REVIEW_REQUIRED")];

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    const tree = buildTicketTree([parent, child], worktrees, prs, apiDeps);

    expect(tree.blocked.has("child-1")).toBe(true);
    expect(tree.unlocked.has("child-1")).toBe(false);
  });
});

describe("buildTicketTree — roots and structure", () => {
  it("returns all tickets as roots when there are no dependencies", () => {
    const t1 = makeTicket({ id: "t1", source_id: "1" });
    const t2 = makeTicket({ id: "t2", source_id: "2" });

    const tree = buildTicketTree([t1, t2]);

    expect(tree.roots).toHaveLength(2);
    expect(tree.blocked.size).toBe(0);
    expect(tree.unlocked.size).toBe(0);
  });

  it("nests child tickets under their parent", () => {
    const parent = makeTicket({ id: "parent-1", source_id: "P1" });
    const child = makeTicket({ id: "child-1", source_id: "C1" });

    const apiDeps: Record<string, TicketDependencies> = {
      "child-1": {
        parent,
        blocked_by: [parent],
        blocks: [],
        children: [],
      },
    };

    const tree = buildTicketTree([parent, child], [], [], apiDeps);

    expect(tree.roots.map((t) => t.id)).toEqual(["parent-1"]);
    expect(tree.childMap.get("P1")?.map((t) => t.id)).toEqual(["child-1"]);
  });
});
