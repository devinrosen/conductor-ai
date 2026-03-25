/**
 * Derive a worktree slug from a ticket's source_id and title.
 * Mirrors the TUI's `derive_worktree_slug()` in helpers.rs.
 */
export function deriveWorktreeSlug(sourceId: string, title: string): string {
  // Lowercase, replace non-alphanumeric with dashes
  const raw = title
    .toLowerCase()
    .replace(/[^a-z0-9]/g, "-");

  // Collapse consecutive dashes and trim
  const titleSlug = raw
    .replace(/-{2,}/g, "-")
    .replace(/^-+|-+$/g, "");

  // Budget: 40 chars total, minus source_id and separator
  const budget = Math.max(0, 40 - sourceId.length - 1);

  let truncated = titleSlug;
  if (titleSlug.length > budget) {
    const lastDash = titleSlug.lastIndexOf("-", budget);
    truncated = lastDash > 0 ? titleSlug.slice(0, lastDash) : titleSlug.slice(0, budget);
  }

  return `${sourceId}-${truncated}`;
}
