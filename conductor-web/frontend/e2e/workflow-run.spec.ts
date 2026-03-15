import { test, expect } from "./fixtures";

test.describe("Workflow run smoke", () => {
  test("Workflows tab shows empty state when no .wf files exist", async ({
    page,
    testRepo,
    testWorktree,
  }) => {
    await page.goto(`/repos/${testRepo.id}/worktrees/${testWorktree.id}`);

    // Switch to the Workflows tab.
    await page.getByRole("button", { name: "Workflows" }).click();

    // The panel displays these empty-state messages when there are no definitions
    // or runs in the worktree (our temp repo has no .wf files).
    await expect(
      page.getByText("No workflow definitions found"),
    ).toBeVisible({ timeout: 10_000 });

    await expect(page.getByText("No workflow runs yet")).toBeVisible({
      timeout: 5_000,
    });
  });
});
