import { describe, it, expect } from "vitest";
import { deriveWorktreeSlug } from "./worktreeUtils";

describe("deriveWorktreeSlug", () => {
  // ── no-label (fallback to feat) ─────────────────────────────────────────

  it("produces a normal slug with feat prefix when no labels", () => {
    expect(deriveWorktreeSlug("123", "Add login flow")).toBe(
      "feat-123-add-login-flow",
    );
  });

  it("strips special characters", () => {
    expect(deriveWorktreeSlug("42", "Fix: null-ptr crash!!")).toBe(
      "feat-42-fix-null-ptr-crash",
    );
  });

  it("collapses consecutive dashes", () => {
    expect(deriveWorktreeSlug("7", "hello---world   test")).toBe(
      "feat-7-hello-world-test",
    );
  });

  it("truncates long titles to 40 chars total", () => {
    const longTitle = "a".repeat(100);
    const slug = deriveWorktreeSlug("99", longTitle);
    expect(slug.length).toBeLessThanOrEqual(40);
    expect(slug.startsWith("feat-99-")).toBe(true);
  });

  it("truncates at word boundary (dash) when possible", () => {
    const title = "aaa-bbb-ccc-ddd-eee-fff-ggg-hhh-iii-jjj-kkk";
    const slug = deriveWorktreeSlug("1234", title);
    expect(slug.length).toBeLessThanOrEqual(40);
    expect(slug).not.toMatch(/-$/);
  });

  it("handles empty title", () => {
    expect(deriveWorktreeSlug("123", "")).toBe("feat-123");
  });

  it("handles all-special-character title", () => {
    expect(deriveWorktreeSlug("123", "!!!")).toBe("feat-123");
  });

  it("handles dash-at-boundary edge case matching Rust behavior", () => {
    // source_id = "10" (2 chars), prefix = "feat" (4 chars)
    // prefix+sep+id+sep = 4+1+2+1 = 8, budget = 40-8 = 32
    // Title slug of "a{32}-rest" has a dash at index 32; lastIndexOf("-", 31) → no dash → hard truncate
    const title = "a".repeat(32) + "-rest";
    const slug = deriveWorktreeSlug("10", title);
    expect(slug).toBe("feat-10-" + "a".repeat(32));
    expect(slug.length).toBe(40);
  });

  // ── label-driven prefix selection ───────────────────────────────────────

  it("bug label produces fix prefix", () => {
    const slug = deriveWorktreeSlug("42", "null ptr crash", ["bug"]);
    expect(slug.startsWith("fix-42-")).toBe(true);
  });

  it("fix label produces fix prefix", () => {
    const slug = deriveWorktreeSlug("1", "fix something", ["fix"]);
    expect(slug.startsWith("fix-1-")).toBe(true);
  });

  it("security label produces fix prefix", () => {
    const slug = deriveWorktreeSlug("2", "cve patch", ["security"]);
    expect(slug.startsWith("fix-2-")).toBe(true);
  });

  it("enhancement label produces feat prefix", () => {
    const slug = deriveWorktreeSlug("3", "add thing", ["enhancement"]);
    expect(slug.startsWith("feat-3-")).toBe(true);
  });

  it("chore label produces chore prefix", () => {
    const slug = deriveWorktreeSlug("7", "clean up deps", ["chore"]);
    expect(slug.startsWith("chore-7-")).toBe(true);
  });

  it("docs label produces docs prefix", () => {
    const slug = deriveWorktreeSlug("8", "update readme", ["docs"]);
    expect(slug.startsWith("docs-8-")).toBe(true);
  });

  it("refactor label produces refactor prefix", () => {
    const slug = deriveWorktreeSlug("9", "extract fn", ["refactor"]);
    expect(slug.startsWith("refactor-9-")).toBe(true);
  });

  it("test label produces test prefix", () => {
    const slug = deriveWorktreeSlug("10", "add coverage", ["test"]);
    expect(slug.startsWith("test-10-")).toBe(true);
  });

  it("ci label produces ci prefix", () => {
    const slug = deriveWorktreeSlug("11", "update actions", ["ci"]);
    expect(slug.startsWith("ci-11-")).toBe(true);
  });

  it("perf label produces perf prefix", () => {
    const slug = deriveWorktreeSlug("12", "cache results", ["perf"]);
    expect(slug.startsWith("perf-12-")).toBe(true);
  });

  it("mixed-case label is matched case-insensitively", () => {
    const slug = deriveWorktreeSlug("13", "crash fix", ["Bug"]);
    expect(slug.startsWith("fix-13-")).toBe(true);
  });

  it("unknown label falls back to feat", () => {
    const slug = deriveWorktreeSlug("14", "some work", ["wontfix"]);
    expect(slug.startsWith("feat-14-")).toBe(true);
  });

  it("first matching label wins", () => {
    const slug = deriveWorktreeSlug("15", "work", ["bug", "enhancement"]);
    expect(slug.startsWith("fix-15-")).toBe(true);
  });

  it("total slug length ≤ 40 chars with long prefix", () => {
    const longTitle = "a".repeat(100);
    const slug = deriveWorktreeSlug("99", longTitle, ["refactor"]);
    expect(slug.length).toBeLessThanOrEqual(40);
  });
});
