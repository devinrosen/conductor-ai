import * as crypto from "crypto";
import { test, expect } from "./fixtures";
import type { TestWorktree } from "./fixtures";

/**
 * Mobile smoke tests for the 3 core flows on iPhone 14 (390px) and Pixel 7 (412px).
 * These run against both projects defined in playwright.config.ts.
 */

test.describe("Mobile smoke: create worktree", () => {
  test("create worktree via mobile UI shows new row in list", async ({
    page,
    request,
    testRepo,
  }) => {
    await page.goto(`/repos/${testRepo.id}`);

    // On mobile the sidebar is hidden — the main content is visible immediately.
    // Open the Create Worktree form.
    await page.getByRole("button", { name: "Create Worktree" }).click();

    // Fill in a unique worktree name.
    const name = `e2e-mobile-${Date.now()}-${crypto.randomBytes(3).toString("hex")}`;
    await page.getByPlaceholder("feat-my-feature").fill(name);

    // Submit.
    await page.getByRole("button", { name: "Create" }).click();

    // The WorktreeRow renders the branch name as a link.
    await expect(
      page.getByRole("link", { name: new RegExp(name.replace(/-/g, "[-/]")) }),
    ).toBeVisible({ timeout: 15_000 });

    // Cleanup created worktree.
    const resp = await request.get(`/api/repos/${testRepo.id}/worktrees`);
    const worktrees: TestWorktree[] = await resp.json();
    const created = worktrees.find((w) => w.slug.includes(name));
    if (created) {
      await request.delete(`/api/worktrees/${created.id}`).catch(() => {});
    }
  });
});

test.describe("Mobile smoke: delete worktree", () => {
  test("delete worktree via mobile UI removes row from list", async ({
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

test.describe("Mobile smoke: workflows tab", () => {
  test("Workflows tab is reachable from worktree detail", async ({
    page,
    testRepo,
    testWorktree,
  }) => {
    await page.goto(`/repos/${testRepo.id}/worktrees/${testWorktree.id}`);

    // Tap the Workflows tab button.
    await page.getByRole("button", { name: "Workflows" }).click();

    // The workflows panel renders either definitions or an empty state message.
    // Either way it confirms the tab loaded successfully.
    const workflowPanel = page.locator("text=Available Workflows");
    await expect(workflowPanel).toBeVisible({ timeout: 10_000 });
  });
});

test.describe("Mobile smoke: bottom tab bar navigation", () => {
  test("Activity tab navigates to ActivityPage and shows heading", async ({ page }) => {
    // Start at the repos page then navigate via the bottom tab bar.
    await page.goto("/repos");

    // The bottom tab bar is visible on mobile (md:hidden).
    const activityTab = page.getByRole("link", { name: "Activity" });
    await expect(activityTab).toBeVisible({ timeout: 5_000 });
    await activityTab.click();

    // ActivityPage renders an "Activity" heading.
    await expect(page.getByRole("heading", { name: "Activity" })).toBeVisible({
      timeout: 10_000,
    });
    // URL should be root.
    await expect(page).toHaveURL("/");
  });

  test("Repos tab navigates to repos list", async ({ page }) => {
    // Start at the activity (root) page.
    await page.goto("/");

    const reposTab = page.getByRole("link", { name: "Repos" });
    await expect(reposTab).toBeVisible({ timeout: 5_000 });
    await reposTab.click();

    // URL should change to /repos.
    await expect(page).toHaveURL("/repos");
  });

  test("Workflows tab navigates to WorkflowsPage and shows heading", async ({ page }) => {
    await page.goto("/");

    const workflowsTab = page.getByRole("link", { name: "Workflows" });
    await expect(workflowsTab).toBeVisible({ timeout: 5_000 });
    await workflowsTab.click();

    // WorkflowsPage renders a "Workflows" heading.
    await expect(page.getByRole("heading", { name: "Workflows" })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page).toHaveURL("/workflows");
  });

  test("active tab is highlighted when on matching route", async ({ page }) => {
    await page.goto("/workflows");

    // The active NavLink receives text-indigo-600 — check by colour via CSS class presence.
    const workflowsTab = page.getByRole("link", { name: "Workflows" });
    await expect(workflowsTab).toHaveClass(/text-indigo-600/, { timeout: 5_000 });

    // Other tabs should not be active.
    const activityTab = page.getByRole("link", { name: "Activity" });
    await expect(activityTab).not.toHaveClass(/text-indigo-600/);
  });
});
