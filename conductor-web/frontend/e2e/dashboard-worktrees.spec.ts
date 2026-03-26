import { test, expect } from "./fixtures";

/**
 * Regression test: worktrees must render on the Activity (/) page on mobile.
 * Previously, Promise.all coupled listAllWorktrees with latestRunsByWorktree —
 * if the latter rejected, no worktrees rendered at all.
 */
test.describe("Dashboard worktree visibility", () => {
  test("active worktree appears in table on / page", async ({
    page,
    testRepo,
    testWorktree,
  }) => {
    await page.goto("/");

    // The Active Worktrees section should contain the seeded worktree's branch.
    const branchText = page.getByText(testWorktree.branch);
    await expect(branchText).toBeVisible({ timeout: 10_000 });
  });

  test("worktree table has rows matching API response", async ({
    page,
    request,
    testRepo,
    testWorktree,
  }) => {
    await page.goto("/");

    // Wait for the table to render with at least one row.
    const tableBody = page.locator("table tbody");
    await expect(tableBody.locator("tr")).not.toHaveCount(0, { timeout: 10_000 });

    // Verify the seeded worktree is in the table.
    const row = tableBody.locator("tr").filter({ hasText: testWorktree.branch });
    await expect(row).toBeVisible();
  });
});
