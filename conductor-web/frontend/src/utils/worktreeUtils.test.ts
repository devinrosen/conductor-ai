import { describe, it, expect } from "vitest";
import { deriveWorktreeSlug } from "./worktreeUtils";

describe("deriveWorktreeSlug", () => {
  it("produces a normal slug", () => {
    expect(deriveWorktreeSlug("123", "Add login flow")).toBe(
      "123-add-login-flow",
    );
  });

  it("strips special characters", () => {
    expect(deriveWorktreeSlug("42", "Fix: null-ptr crash!!")).toBe(
      "42-fix-null-ptr-crash",
    );
  });

  it("collapses consecutive dashes", () => {
    expect(deriveWorktreeSlug("7", "hello---world   test")).toBe(
      "7-hello-world-test",
    );
  });

  it("truncates long titles to 40 chars total", () => {
    const longTitle = "a".repeat(100);
    const slug = deriveWorktreeSlug("99", longTitle);
    expect(slug.length).toBeLessThanOrEqual(40);
    expect(slug.startsWith("99-")).toBe(true);
  });

  it("truncates at word boundary (dash) when possible", () => {
    // source_id "1234" = 4 chars, separator = 1, budget = 35
    // Title slug: "aaa...(15 a's)-bbb...(15 b's)-ccc...(15 c's)"
    // At budget=35, should truncate at the last dash before index 35
    const title = "aaa-bbb-ccc-ddd-eee-fff-ggg-hhh-iii-jjj-kkk";
    const slug = deriveWorktreeSlug("1234", title);
    expect(slug.length).toBeLessThanOrEqual(40);
    // Should end at a word boundary (no trailing partial word)
    expect(slug).not.toMatch(/-$/);
  });

  it("handles empty title", () => {
    expect(deriveWorktreeSlug("123", "")).toBe("123");
  });

  it("handles all-special-character title", () => {
    expect(deriveWorktreeSlug("123", "!!!")).toBe("123");
  });

  it("handles dash-at-boundary edge case matching Rust behavior", () => {
    // Build a title where a dash falls exactly at the budget index
    // source_id = "10", budget = 40 - 2 - 1 = 37
    // Create slug where char at index 37 is a dash
    // "a{37}-rest" -> titleSlug[37] = "-"
    // Rust: title_slug[..37] searches 0..36, finds no dash in "aaa...a" -> hard truncate
    // TS with fix: lastIndexOf("-", 36) also finds no dash -> hard truncate
    const title = "a".repeat(37) + "-rest";
    const slug = deriveWorktreeSlug("10", title);
    expect(slug).toBe("10-" + "a".repeat(37));
    expect(slug.length).toBe(40);
  });
});
