import { test } from "@playwright/test";
import * as path from "path";
import * as fs from "fs";
import { fileURLToPath } from "url";

/**
 * Screenshot capture harness for mobile UX audits.
 *
 * Navigates to every conductor-web view on both mobile device profiles
 * (iPhone 14 + Pixel 7) and saves full-page screenshots. Not a real test —
 * used by the mobile-ux-audit workflow to feed screenshots to the AI agent.
 *
 * Screenshots are saved to SCREENSHOT_OUTPUT_DIR (env) or a default location.
 */

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const SCREENSHOT_DIR =
  process.env.SCREENSHOT_OUTPUT_DIR ||
  path.join(__dirname, "screenshots", new Date().toISOString().slice(0, 10));

// Ensure the output directory exists.
fs.mkdirSync(SCREENSHOT_DIR, { recursive: true });

/** Wait for the page to settle — SSE /api/events prevents networkidle. */
async function waitForSettle(page: import("@playwright/test").Page) {
  await page.waitForLoadState("domcontentloaded");
  await page.waitForTimeout(2000);
}

function screenshotPath(device: string, name: string): string {
  const slug = device.toLowerCase().replace(/\s+/g, "-");
  return path.join(SCREENSHOT_DIR, `${slug}-${name}.png`);
}

function deviceSlug(testInfo: import("@playwright/test").TestInfo): string {
  return (testInfo.project.name || "unknown").toLowerCase().replace(/\s+/g, "-");
}

test.describe("Mobile UX Screenshots", () => {
  test("activity page (home)", async ({ page }, testInfo) => {
    await page.goto("/");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "activity"),
      fullPage: true,
    });
  });

  test("repos list", async ({ page }, testInfo) => {
    await page.goto("/repos");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "repos-list"),
      fullPage: true,
    });
  });

  test("workflows page", async ({ page }, testInfo) => {
    await page.goto("/workflows");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "workflows"),
      fullPage: true,
    });
  });

  test("tickets page", async ({ page }, testInfo) => {
    await page.goto("/tickets");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "tickets"),
      fullPage: true,
    });
  });

  test("settings page", async ({ page }, testInfo) => {
    await page.goto("/settings");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "settings"),
      fullPage: true,
    });
  });

  test("not found page", async ({ page }, testInfo) => {
    await page.goto("/nonexistent-route");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "not-found"),
      fullPage: true,
    });
  });

  test("bottom tab bar navigation", async ({ page }, testInfo) => {
    // Navigate to repos to show the bottom nav bar with a non-active Activity tab.
    await page.goto("/repos");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "bottom-tab-bar"),
      fullPage: true,
    });
  });

  test("register repo modal", async ({ page }, testInfo) => {
    await page.goto("/repos");
    await waitForSettle(page);

    // Open the Register Repo modal.
    const registerBtn = page.getByRole("button", { name: /register/i });
    if (await registerBtn.isVisible().catch(() => false)) {
      await registerBtn.click();
      await page.waitForTimeout(500);
      await page.screenshot({
        path: screenshotPath(deviceSlug(testInfo), "register-repo-modal"),
        fullPage: true,
      });
    }
  });

  test("empty repo detail", async ({ page }, testInfo) => {
    // Navigate to a nonexistent repo ID to capture the empty/error state.
    await page.goto("/repos/00000000-0000-0000-0000-000000000000");
    await waitForSettle(page);
    await page.screenshot({
      path: screenshotPath(deviceSlug(testInfo), "repo-detail-empty"),
      fullPage: true,
    });
  });
});
