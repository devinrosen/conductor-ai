# Personas

## Developer

**Description:** A software engineer who uses Conductor day-to-day to manage worktrees, run workflows, and oversee AI agent work across one or more repos.
**Capabilities:**
- Register and browse repos and their issue sources
- Create, link, and delete worktrees tied to tickets
- Launch and monitor AI agent runs inside a worktree
- Run, resume, and inspect workflows (including approving human gates)
- Sync and triage tickets from GitHub or Jira
- Attach to live agent tmux windows for direct interaction
**Entry points:**
- TUI (`conductor-tui`) — primary interactive interface; keyboard-driven dashboard and worktree detail views
- CLI (`conductor`) — one-shot and scripted commands (`worktree create`, `workflow run`, `agent launch`, etc.)
- Web UI (`conductor-web`) — browser-based view with real-time SSE event stream
**Goals:**
- Move from ticket → branch → PR with minimal context-switching
- Delegate implementation and review tasks to AI agents while retaining final approval
- Maintain visibility into what all agents are doing across repos

---

## Claude Code User

**Description:** A developer who interacts with Conductor entirely through Claude Code's MCP integration, using conversational commands instead of a standalone TUI or CLI.
**Capabilities:**
- Access conductor state via MCP resources (`conductor://repos`, `conductor://tickets/{repo}`, etc.)
- Create worktrees from tickets conversationally
- Run and resume workflows without switching terminals
- Approve or reject human gates inline in the chat
- Monitor running workflow steps and agent status via the status line
**Entry points:**
- MCP tools exposed by the `conductor` MCP server (`conductor_create_worktree`, `conductor_run_workflow`, `conductor_approve_gate`, etc.)
- Claude Code skills (`/conductor:create-worktree`, `/conductor:run-workflow`, etc.)
- Ambient status line showing pending gates and active runs
**Goals:**
- Stay in Claude Code without needing to switch to a terminal for conductor operations
- Issue natural-language instructions that resolve to workflow or worktree actions
- Receive structured context about repo/ticket state to inform coding decisions

---

## Agent — Actor

**Description:** An AI agent (typically Claude) configured with write permissions to implement code changes, commit to branches, and push PRs on behalf of the user.
**Capabilities:**
- Read and write files within a worktree
- Execute shell commands (build, test, lint, format)
- Commit code (`can_commit: true`) and push to a feature branch
- Create or update pull requests via `gh`
- Emit structured output consumed by the workflow engine
**Entry points:**
- Workflow step with `role: actor` in a `.wf` file
- Direct agent launch via `conductor agent launch --role actor`
- Named bot identity (`bot_name`) for GitHub App persona
**Goals:**
- Complete a scoped implementation or fix task defined by a workflow step prompt
- Produce a PR or commit that passes CI and satisfies the ticket acceptance criteria

---

## Agent — Reviewer

**Description:** An AI agent configured in read-only mode to analyze code, review PRs, generate reports, or produce structured analysis without making any commits.
**Capabilities:**
- Read source files, git history, and PR diffs
- Run read-only commands (grep, tests, static analysis)
- Emit analysis output (JSON, Markdown) consumed by downstream workflow steps or human gates
- Cannot commit or push (`can_commit` defaults to false)
**Entry points:**
- Workflow step with `role: reviewer` (the default role) in a `.wf` file
- Direct agent launch via `conductor agent launch` (reviewer is default)
- Named bot identity for a specialized review persona (e.g., `security-bot`, `qa-bot`)
**Goals:**
- Produce accurate, structured feedback on code quality, security, UX, or correctness
- Surface issues that gate promotion to the next workflow step or human approval

---

## Workflow Orchestrator

**Description:** A team lead or platform engineer who authors and maintains `.wf` workflow definitions and agent configurations, shaping the automated pipelines that other personas use.
**Capabilities:**
- Author and validate `.conductor/workflows/*.wf` files (YAML-based DAGs)
- Define and configure agents in `.conductor/agents/*.md` with frontmatter (role, model, can_commit)
- Register repos and configure issue sources (GitHub, Jira)
- Set global model defaults and per-repo settings in `~/.conductor/config.toml`
- Approve or reject workflow gates that require human review
- Monitor workflow run history and step logs across all repos
**Entry points:**
- CLI (`conductor workflow validate`, `conductor workflow run`, `conductor repo add`)
- TUI workflow column — browse, run, and inspect workflow definitions and runs
- Direct file editing of `.conductor/` directory in the repo
**Goals:**
- Define reliable, repeatable automated pipelines for common development tasks (ticket-to-PR, diagram generation, UX analysis, etc.)
- Ensure AI agents have appropriate permissions and model selection for each task type
- Reduce manual toil for the developer persona by encoding team processes as workflows
