import { test, expect } from "./fixtures";

test.describe("Features page", () => {
  test("features page loads and shows heading", async ({ page }) => {
    await page.goto("/features");

    await expect(
      page.getByRole("heading", { name: "Features" }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("sidebar contains Features nav link", async ({ page }) => {
    await page.goto("/");

    const link = page.getByRole("link", { name: "Features" });
    await expect(link).toBeVisible({ timeout: 10_000 });
  });

  test("sidebar Features link navigates to features page", async ({ page }) => {
    await page.goto("/");

    await page.getByRole("link", { name: "Features" }).click();

    await expect(page).toHaveURL(/\/features/);
    await expect(
      page.getByRole("heading", { name: "Features" }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("features page shows repo sections when repos are registered", async ({
    page,
    testRepo,
  }) => {
    await page.goto("/features");

    // The repo's slug should appear as a section heading.
    await expect(
      page.getByText(testRepo.slug),
    ).toBeVisible({ timeout: 10_000 });
  });
});
