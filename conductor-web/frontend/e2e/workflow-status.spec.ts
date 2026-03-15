import { test, expect } from "./fixtures";
import type { APIRequestContext, Page } from "@playwright/test";
import * as fs from "fs";
import * as path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/** Poll GET /api/workflows/runs/{id} until status matches or timeout elapses. */
async function waitForRunStatus(
  request: APIRequestContext,
  runId: string,
  status: string,
  timeoutMs = 10_000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await request.get(`/api/workflows/runs/${runId}`);
    if (resp.ok()) {
      const run = await resp.json();
      if (run.status === status) return true;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  return false;
}

/** Seed test-workflow.wf into a worktree's .conductor/workflows/ directory. */
function seedTestWorkflow(worktreePath: string): void {
  const workflowsDir = path.join(worktreePath, ".conductor", "workflows");
  fs.mkdirSync(workflowsDir, { recursive: true });
  const wfContent = fs.readFileSync(
    path.join(__dirname, "fixtures", "test-workflow.wf"),
    "utf-8",
  );
  fs.writeFileSync(path.join(workflowsDir, "test-workflow.wf"), wfContent);
}

/** Seed the test workflow, start a run, and return the run ID. */
async function startTestWorkflowRun(
  request: APIRequestContext,
  worktreePath: string,
  worktreeId: string,
): Promise<string> {
  seedTestWorkflow(worktreePath);

  const runResp = await request.post(
    `/api/worktrees/${worktreeId}/workflows/run`,
    { data: { name: "test-workflow" } },
  );
  expect(runResp.ok()).toBeTruthy();

  const runsResp = await request.get(
    `/api/worktrees/${worktreeId}/workflows/runs`,
  );
  const runs = await runsResp.json();
  expect(runs.length).toBeGreaterThan(0);
  return runs[0].id as string;
}

/** Navigate to the worktree detail page and open the Workflows tab. */
async function openWorkflowsTab(
  page: Page,
  repoId: string,
  worktreeId: string,
): Promise<void> {
  await page.goto(`/repos/${repoId}/worktrees/${worktreeId}`);
  await page.getByRole("button", { name: "Workflows" }).click();
}

test.describe("Workflow status smoke", () => {
  test("step tree expands when clicking a run row", async ({
    page,
    request,
    testRepo,
    testWorktree,
  }) => {
    await startTestWorkflowRun(request, testWorktree.path, testWorktree.id);
    await openWorkflowsTab(page, testRepo.id, testWorktree.id);

    // Click the run row to expand the step tree.
    await page.getByText("test-workflow").first().click();

    // The gate step named "human_approval" should appear in the expanded tree.
    await expect(page.getByText("human_approval")).toBeVisible({
      timeout: 10_000,
    });
  });

  test("gate step shows Approve and Reject buttons when waiting", async ({
    page,
    request,
    testRepo,
    testWorktree,
  }) => {
    const runId = await startTestWorkflowRun(
      request,
      testWorktree.path,
      testWorktree.id,
    );

    // Poll via the API until the run reaches "waiting" — the gate step
    // pauses execution immediately since it is the first and only node.
    const reached = await waitForRunStatus(request, runId, "waiting");
    expect(reached, "run should reach 'waiting' before timeout").toBeTruthy();

    // Navigate to the worktree detail page and open the Workflows tab.
    await openWorkflowsTab(page, testRepo.id, testWorktree.id);

    // Expand the run row to reveal the step list.
    await page.getByText("test-workflow").first().click();

    // Approve and Reject buttons should be visible for the waiting gate step.
    const approveBtn = page.getByRole("button", { name: "Approve" });
    const rejectBtn = page.getByRole("button", { name: "Reject" });
    await expect(approveBtn).toBeVisible({ timeout: 10_000 });
    await expect(rejectBtn).toBeVisible({ timeout: 5_000 });

    // Click Approve — the UI calls POST /api/workflows/runs/{id}/gate/approve.
    await approveBtn.click();

    // After approval the gate step transitions to completed; buttons disappear.
    await expect(approveBtn).not.toBeVisible({ timeout: 10_000 });
  });
});
