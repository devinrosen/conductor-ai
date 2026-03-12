# Getting Started with the Conductor CLI

This guide is for teams that want to use `conductor` as a standalone command-line tool — running workflows, managing worktrees, and interacting with agents — without the TUI or web UI running.

## Prerequisites

- **macOS or Linux**
- **[`gh` CLI](https://cli.github.com/)** — authenticated (`gh auth login`)
- **[`tmux`](https://github.com/tmux/tmux)** — agents run in tmux windows
- **Claude API key** — set as `ANTHROPIC_API_KEY` in your environment
- **Git**

## Installation

### Download a release binary (recommended)

> Release binaries are not yet published. See [Build from source](#build-from-source) below.

### Build from source

```bash
git clone https://github.com/devinrosen/conductor-ai.git
cd conductor-ai
cargo build --release --bin conductor
# Move to somewhere on your PATH:
cp target/release/conductor /usr/local/bin/conductor
```

Verify:
```bash
conductor --help
```

## One-time setup

### Register your repo

```bash
# If you already have a local checkout:
conductor repo add https://github.com/your-org/your-repo --local-path ~/path/to/repo

# If you want conductor to clone it:
conductor repo add https://github.com/your-org/your-repo
```

Conductor stores all state in `~/.conductor/conductor.db`. Nothing is written into your repo except the `.conductor/` directory (workflows, agents, prompts) which you manage yourself.

## Running workflows

### Against a PR (no worktree needed)

The fastest way to run a workflow on a PR — no registration, no worktree. Conductor shallow-clones the PR branch to a temp directory, loads the workflow from that repo's `.conductor/workflows/`, runs it, and cleans up.

```bash
conductor workflow run draft-release-notes --pr https://github.com/your-org/your-repo/pull/42

# Short-form reference also works:
conductor workflow run draft-release-notes --pr your-org/your-repo#42

# Pass inputs:
conductor workflow run publish-docs --pr your-org/your-repo#42 --input force=true
```

The workflow must exist in the PR branch's `.conductor/workflows/` directory and must have `targets = ["pr"]` in its meta block.

### Against a registered repo + worktree

```bash
# Create a worktree from a ticket or branch name:
conductor worktree create my-repo fix-login-bug

# List available workflows:
conductor workflow list my-repo fix-login-bug

# Run a workflow:
conductor workflow run my-repo fix-login-bug ticket-to-pr --input ticket_id=PROJ-123

# Dry run (agents with can_commit = true will not commit or push):
conductor workflow run my-repo fix-login-bug ticket-to-pr --dry-run
```

### Validate a workflow before running

```bash
# Against a local directory (no DB registration required):
conductor workflow validate publish-docs --path ~/path/to/repo

# Against a registered worktree:
conductor workflow validate ticket-to-pr my-repo fix-login-bug
```

## Monitoring runs

```bash
# List runs for a repo:
conductor workflow runs my-repo

# List runs for a specific worktree:
conductor workflow runs my-repo fix-login-bug

# Show step-by-step detail for a run:
conductor workflow run-show <run-id>
```

## Gates

When a workflow reaches a `gate human_approval` or `gate human_review` step, it pauses and waits. Use these commands to unblock it:

```bash
# Approve (continue):
conductor workflow gate-approve <run-id>

# Reject (fails the workflow):
conductor workflow gate-reject <run-id>

# Approve with feedback (injected into next agent as {{gate_feedback}}):
conductor workflow gate-feedback <run-id> "Focus on the auth module, skip UI changes"
```

## Resuming failed runs

```bash
# Resume from the last failed step:
conductor workflow resume <run-id>

# Restart from the beginning:
conductor workflow resume <run-id> --restart

# Resume from a specific step:
conductor workflow resume <run-id> --from implement
```

## Embedding conductor in a project

If your team wants to run conductor workflows directly from a project repo without setting up the TUI or web UI:

1. **Add a `.conductor/` directory** to your repo with your workflows and agents:
   ```
   .conductor/
   ├── workflows/
   │   └── publish-docs.wf
   └── agents/
       ├── build-docutil.md
       └── publish-docs.md
   ```

2. **Register the repo once** per developer machine:
   ```bash
   conductor repo add https://github.com/your-org/your-repo --local-path .
   ```

3. **Run workflows** from anywhere:
   ```bash
   conductor workflow run publish-docs --pr https://github.com/your-org/your-repo/pull/99
   ```

For PR-targeted workflows using `--pr`, no registration is needed at all — conductor resolves everything from the PR URL.

## Tips

- **Model override:** Pass `--model claude-opus-4-6` to any `workflow run` to use a specific model for all agent steps.
- **Step timeout:** Default is 30 minutes per step. Override with `--step-timeout-secs 3600`.
- **Continue on failure:** Use `--no-fail-fast` to run remaining steps even if one fails.
- **Multiple inputs:** `--input` can be repeated: `--input ticket_id=123 --input skip_tests=true`
- **Agent logs:** Agent output streams live to the tmux window. Attach with `tmux attach` to watch in real time.
