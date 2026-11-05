# Getting Started with the Conductor CLI

This guide is for teams that want to use `conductor` as a standalone command-line tool — running workflows, managing worktrees, and interacting with agents — without the TUI or web UI running.

## Prerequisites

- **macOS or Linux**
- **[`gh` CLI](https://cli.github.com/)** — authenticated (`gh auth login`)
- **[Claude Code CLI (`claude`)](https://docs.anthropic.com/en/docs/claude-code)** — installed and authenticated
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
conductor repo register https://github.com/your-org/your-repo --local-path ~/path/to/repo

# If you want conductor to clone it:
conductor repo register https://github.com/your-org/your-repo
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

# Small change with clear acceptance criteria (no plan step):
conductor workflow run my-repo fix-typo quick-fix-pr --input ticket_id=PROJ-124

# Dry run (agents with can_commit = true will not commit or push):
conductor workflow run my-repo fix-login-bug ticket-to-pr --dry-run
```

**Choosing a workflow.** Use `quick-fix-pr` for changes to a few files with clear acceptance criteria, prompt and workflow edits, and small bugs. It has no plan step and runs one review plus at most one re-review, then hands off to a human if the PR still has blocking findings. Use `ticket-to-pr` (the default) for anything that needs a plan.

### Validate a workflow before running

```bash
# Against a local directory (no DB registration required):
conductor workflow validate publish-docs --path ~/path/to/repo

# Against a registered worktree:
conductor workflow validate ticket-to-pr my-repo fix-login-bug

# Every workflow in the repo, plus the embedded workflow templates:
conductor workflow validate --all --templates --path ~/path/to/repo

# Also smoke-start it: a real, recorded run that executes script steps up to the
# first agent or gate step, then cancels. No model calls.
conductor workflow validate ticket-to-pr my-repo fix-login-bug --dry-run --input qualify=false
```

Validation runs the same checks a run starts with: the DSL parses, agents, schemas and scripts exist (workflow-local files first, then shared ones), there are no cycles, and every gate type, executor and foreach item provider is registered. A `{{variable}}` that is not an input, a built-in or something a script exports is reported as a warning.

`--dry-run` then starts each workflow that passed, so registries, gate resolvers and input defaults are exercised for real. It reports `SMOKE PASS` with the agent or gate step it stopped at and the run ID, or `SMOKE FAIL` with the failing step, and exits non-zero on a failure. With `--all`, a workflow whose required inputs have no default is skipped. Script steps before the first agent run for real, so use a worktree you can afford to touch.

## Monitoring runs

```bash
# List runs for a repo:
conductor workflow runs my-repo

# List runs for a specific worktree:
conductor workflow runs my-repo fix-login-bug

# Show step-by-step detail for a run:
conductor workflow run-show <run-id>
```

To block until a run (and its child runs) finishes, use `wait`:

```bash
conductor workflow wait <run-id> [--timeout <secs>] [--stall-after <secs>] [--json]
```

It prints the final status, error, and last step. A non-terminal run with no lease or step activity for `--stall-after` seconds (default 60) is reported as stalled: its engine is gone, so resume it with `conductor workflow resume`. Agents can use the `conductor_wait_run` MCP tool instead.

| Exit code | Meaning |
|-----------|---------|
| 0 | Run completed |
| 1 | Run failed or was cancelled |
| 2 | Timeout |
| 3 | Stalled (no live engine) |

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
conductor workflow resume <run-id> --from-step implement
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
   conductor repo register https://github.com/your-org/your-repo --local-path .
   ```

3. **Run workflows** from anywhere:
   ```bash
   conductor workflow run publish-docs --pr https://github.com/your-org/your-repo/pull/99
   ```

For PR-targeted workflows using `--pr`, no registration is needed at all — conductor resolves everything from the PR URL.

## Claude Code integration

Conductor provides an MCP server that gives Claude Code access to repos, tickets, worktrees, and workflow runs.

```bash
# Register the MCP server in Claude Code's settings.json:
conductor setup install

# Unregister:
conductor setup uninstall
```

After installing, restart Claude Code. The conductor MCP server will be available for tool use.

### Session tracking plugin

To have interactive Claude Code sessions in conductor worktrees show up in conductor, install the plugin (`conductor setup install` prints these commands too):

```bash
claude plugin marketplace add devinrosen/conductor-ai
claude plugin install conductor@conductor-ai
```

See [plugin/README.md](../../plugin/README.md).

### Opening sessions from conductor

`conductor session open <repo> <worktree>` focuses the worktree's live Claude Code session in tmux. If none is live it resumes the newest past session, or starts a fresh one in a new window of the tmux session (default `work`, created detached if missing).

```bash
conductor session open my-repo feat-123-thing                 # focus / resume / start
conductor session open my-repo feat-123-thing --new           # fresh session even if one exists
conductor session open my-repo feat-123-thing --no-focus      # launch without switching the tmux client
echo "status?" | conductor session open my-repo feat-123-thing --prompt-file -
```

Other flags: `--prompt <text>`, `--window <name>` (default: the ticket key, or the worktree slug), `--no-focus` and `--json`. Inside tmux it switches to the window unless `--no-focus` is given (for scripts, or a manager session launching on someone's behalf); elsewhere, or with `--no-focus`, it prints `tmux attach -t work \; select-window -t <window>`. In the TUI, press `C` on a worktree. The `conductor_open_session` MCP tool does the same but never switches focus.

Configure it in `~/.conductor/config.toml`; a repo's `.conductor/config.toml` can override the two templates:

```toml
[sessions]
tmux_session = "work"
opener_template = "Work on {{key}} ({{title}}). Read {{url}} first."  # fresh session with a linked ticket
resume_template = "Where did we leave off?"                             # unset = no prompt on resume
```

Template variables: `key`, `title`, `url`, `source`, `branch`, `base`, `worktree_path`. Sessions launched this way always report to conductor: if the conductor plugin isn't enabled, conductor passes its hooks with `--settings`.

#### Taking over a headless run

When a workflow agent is stuck, step into its conversation instead of starting a fresh session:

```bash
conductor session take-over <run-id>            # [--window <name>] [--json]
```

This launches `claude --resume <session_id>` in the run's worktree, in a tmux window, the same way `session open` does. In the TUI, press `O` on a worktree's latest agent run, or on a workflow step's agent run. The `conductor_take_over_session` MCP tool does the same but never switches focus.

- **Finished runs only.** Only `completed`, `failed` and `cancelled` runs can be taken over. A running run is refused: stop it first (`x` in the TUI).
- **Your own permission mode.** The session starts in your normal interactive mode. It doesn't inherit `agent_permission_mode` or any flag the headless run used.
- **A new interactive run** records the takeover, with the same `session_id` and the headless run as its parent. The headless run keeps its own history, cost and status. The TUI shows "taken over → interactive session" on the headless run and "from step '<name>'" on the interactive one.
- **Headless never resumes it again.** Once a human has continued a session, no headless path (conversation chaining, the structured-output correction, finishing an unfinished turn, the TUI and web prompts) resumes that `session_id`. They start a fresh session instead.
- Taking over a step's run doesn't change the workflow's state. If the run was already taken over and that session is live, the command focuses it.

## Tips

- **Model override:** Pass `--model claude-opus-4-6` to any `workflow run` to use a specific model for all agent steps.
- **Step timeout:** Default is 12 hours per step. Override with `--step-timeout-secs 3600`.
- **Continue on failure:** Use `--no-fail-fast` to run remaining steps even if one fails.
- **Multiple inputs:** `--input` can be repeated: `--input ticket_id=123 --input skip_tests=true`
- **Agent logs:** Agent output is written to `~/.conductor/agent-logs/<run-id>.log` in real time.
