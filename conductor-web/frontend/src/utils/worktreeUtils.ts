/**
 * Map ticket labels to conventional-commit branch prefixes.
 * Mirrors label_to_branch_prefix() in conductor-core/src/worktree/manager.rs.
 */
const LABEL_PREFIX_MAP: Record<string, string> = {
  bug: "fix",
  fix: "fix",
  security: "fix",
  enhancement: "feat",
  feature: "feat",
  chore: "chore",
  maintenance: "chore",
  documentation: "docs",
  docs: "docs",
  refactor: "refactor",
  test: "test",
  testing: "test",
  ci: "ci",
  build: "ci",
  perf: "perf",
  performance: "perf",
};

/**
 * Derive a worktree slug from a ticket's source_id, title, and optional labels.
 * Mirrors the TUI's `derive_worktree_slug()` in helpers.rs.
 * Format: `{prefix}-{sourceId}-{slugified-title}`
 */
export function deriveWorktreeSlug(
  sourceId: string,
  title: string,
  labels?: string[],
): string {
  const prefix =
    (labels ?? [])
      .map((l) => LABEL_PREFIX_MAP[l.toLowerCase()])
      .find(Boolean) ?? "feat";

  // Lowercase, replace non-alphanumeric with dashes
  const raw = title.toLowerCase().replace(/[^a-z0-9]/g, "-");

  // Collapse consecutive dashes and trim
  const titleSlug = raw.replace(/-{2,}/g, "-").replace(/^-+|-+$/g, "");

  // Budget: 40 chars total, minus prefix, separator, source_id, and separator
  const budget = Math.max(0, 40 - prefix.length - 1 - sourceId.length - 1);

  let truncated = titleSlug;
  if (titleSlug.length > budget) {
    const lastDash = titleSlug.lastIndexOf("-", budget - 1);
    truncated =
      lastDash > 0 ? titleSlug.slice(0, lastDash) : titleSlug.slice(0, budget);
  }

  const slug = truncated
    ? `${prefix}-${sourceId}-${truncated}`
    : `${prefix}-${sourceId}`;
  return slug.replace(/-+$/, "");
}
