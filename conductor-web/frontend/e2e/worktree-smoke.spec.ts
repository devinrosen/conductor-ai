import * as crypto from "crypto";
import { test, expect } from "./fixtures";
import type { TestWorktree } from "./fixtures";

test.describe("Worktree smoke", () => {
  test("create worktree via UI shows new row in list", async ({
    page,
    request,
    testRepo,
  }) => {
    await page.goto(`/repos/${testRepo.id}`);

    // Open the Create Worktree form.
    await page.getByRole("button", { name: "Create Worktree" }).click();

    // Fill in a unique worktree name (collision-resistant under parallel runs).
    const name = `e2e-create-${Date.now()}-${crypto.randomBytes(3).toString("hex")}`;
    await page.getByPlaceholder("feat-my-feature").fill(name);

    // Submit the form.
    await page.getByRole("button", { name: "Create" }).click();

    // The WorktreeRow renders the branch name (e.g. feat/e2e-create-…) as a link.
    // Use a partial text match so we don't need to know the exact normalised branch.
    await expect(
      page.getByRole("link", { name: new RegExp(name.replace(/-/g, "[-/]")) }),
    ).toBeVisible({ timeout: 15_000 });

    // Cleanup: delete the worktree created by this test so it doesn't leak to disk.
    const resp = await request.get(`/api/repos/${testRepo.id}/worktrees`);
    const worktrees: TestWorktree[] = await resp.json();
    const created = worktrees.find((w) => w.slug.includes(name));
    if (created) {
      await request.delete(`/api/worktrees/${created.id}`).catch(() => {});
    }
  });

  test("delete worktree via UI removes row from list", async ({
    page,
    testRepo,
    testWorktree,
  }) => {
    await page.goto(`/repos/${testRepo.id}`);

    // Locate the row that contains this worktree's branch name.
    const row = page.getByRole("row").filter({ hasText: testWorktree.branch });
    await expect(row).toBeVisible({ timeout: 10_000 });

    // Click the Delete button inside that row.
    await row.getByRole("button", { name: "Delete" }).click();

    // Confirm the deletion dialog.
    await page.getByRole("button", { name: "Confirm" }).click();

    // The row should disappear from the list.
    await expect(row).not.toBeVisible({ timeout: 10_000 });
  });
});
