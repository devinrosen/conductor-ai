import { test as base } from "@playwright/test";
import { execSync } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

export interface TestRepo {
  id: string;
  slug: string;
  local_path: string;
  remote_url: string;
  default_branch: string;
  workspace_dir: string;
  created_at: string;
  model: string | null;
  allow_agent_issue_creation: boolean;
}

export interface TestWorktree {
  id: string;
  repo_id: string;
  slug: string;
  branch: string;
  path: string;
  ticket_id: string | null;
  status: string;
  created_at: string;
  completed_at: string | null;
  model: string | null;
}

/**
 * Extended test fixtures providing seeded repos and worktrees via REST API.
 * Each fixture creates real on-disk git repos/worktrees so conductor-web can
 * run `git worktree add` and workflow defs can be placed on disk.
 */
export const test = base.extend<{
  testRepo: TestRepo;
  testWorktree: TestWorktree;
}>({
  testRepo: async ({ request }, use) => {
    // Create a minimal git repo in a temp directory so conductor can create
    // worktrees from it.
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "conductor-e2e-"));
    try {
      execSync(
        'git init && git config user.email "test@e2e.local" && git config user.name "E2E Test" && git commit --allow-empty -m "init"',
        { cwd: tmpDir, stdio: "pipe" },
      );
    } catch {
      // If git init fails (unlikely in CI), the repo won't have a HEAD branch.
      // Tests that need a real worktree will fail — which is the correct signal.
    }

    const slug = `e2e-repo-${Date.now()}`;
    const response = await request.post("/api/repos", {
      data: {
        remote_url: `file://${tmpDir}`,
        slug,
        local_path: tmpDir,
      },
    });

    const repo: TestRepo = await response.json();

    await use(repo);

    // Cleanup: delete the repo registration then the temp dir.
    await request.delete(`/api/repos/${repo.id}`).catch(() => {});
    fs.rmSync(tmpDir, { recursive: true, force: true });
  },

  testWorktree: async ({ request, testRepo }, use) => {
    const name = `e2e-wt-${Date.now()}`;
    const response = await request.post(`/api/repos/${testRepo.id}/worktrees`, {
      data: { name },
    });

    const worktree: TestWorktree = await response.json();

    await use(worktree);

    // Cleanup: delete the worktree registration (also removes the git worktree).
    await request.delete(`/api/worktrees/${worktree.id}`).catch(() => {});
  },
});

export { expect } from "@playwright/test";
