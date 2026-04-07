use clap::{Parser, Subcommand};

/// Environment variable name used to pass the current agent run ID to subprocesses.
pub const CONDUCTOR_RUN_ID_ENV: &str = "CONDUCTOR_RUN_ID";

#[derive(Parser)]
#[command(name = "conductor", about = "Multi-repo orchestration tool", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage repositories
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },
    /// Manage worktrees
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommands,
    },
    /// Manage tickets
    Tickets {
        #[command(subcommand)]
        command: TicketCommands,
    },
    /// Run Claude agent (used internally by TUI tmux windows)
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// Multi-step workflow engine
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Set up Claude Code integration (MCP server registration)
    Setup {
        #[command(subcommand)]
        command: SetupCommands,
    },
    /// Manage features (multi-worktree coordination branches)
    Feature {
        #[command(subcommand)]
        command: FeatureCommands,
    },
    /// Model Context Protocol server (stdio transport for Claude Code integration)
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
    /// Development utilities
    Dev {
        #[command(subcommand)]
        command: DevCommands,
    },
}

#[derive(Subcommand)]
pub enum DevCommands {
    /// Populate the current database with realistic test fixtures
    Seed {
        /// Wipe and re-create the database before seeding
        #[arg(long)]
        reset: bool,
    },
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// Start the conductor MCP server on stdio
    Serve,
}

#[derive(Subcommand)]
pub enum SetupCommands {
    /// Register the conductor MCP server in Claude Code
    Install,
    /// Unregister the conductor MCP server from Claude Code
    Uninstall,
}

#[derive(Subcommand)]
pub enum AgentCommands {
    /// Run a Claude agent for a worktree (spawned by TUI in tmux)
    Run {
        /// Agent run ID (from agent_runs table)
        #[arg(long)]
        run_id: String,
        /// Path to the worktree directory
        #[arg(long)]
        worktree_path: String,
        /// Prompt for Claude (use --prompt-file for large prompts)
        #[arg(long)]
        prompt: Option<String>,
        /// Read prompt from a file (avoids OS arg length limits for large diffs)
        #[arg(long)]
        prompt_file: Option<String>,
        /// Resume a previous Claude session
        #[arg(long)]
        resume: Option<String>,
        /// Model to use (e.g. "sonnet", "claude-opus-4-6"). Overrides per-worktree and global defaults.
        #[arg(long)]
        model: Option<String>,
        /// Named GitHub App bot identity to use (matches [github.apps.<name>] in config).
        #[arg(long)]
        bot_name: Option<String>,
        /// Override the permission mode for this run. Valid values: "plan", "repo-safe" (read-only repo agents using --allowedTools restriction).
        #[arg(long)]
        permission_mode: Option<String>,
        /// Additional plugin directories to pass to the Claude CLI
        #[arg(long = "plugin-dir")]
        plugin_dirs: Vec<String>,
    },
    /// Orchestrate child agents: spawn a child run for each plan step
    Orchestrate {
        /// Parent agent run ID (must have plan steps)
        #[arg(long)]
        run_id: String,
        /// Path to the worktree directory
        #[arg(long)]
        worktree_path: String,
        /// Model to use for child agents
        #[arg(long)]
        model: Option<String>,
        /// Stop on first child failure
        #[arg(long)]
        fail_fast: bool,
        /// Child run timeout in seconds (default: 1800 = 30 min)
        #[arg(long, default_value = "1800")]
        child_timeout_secs: u64,
    },
    /// Create a new GitHub issue (called by agents during a run)
    CreateIssue {
        /// Issue title
        #[arg(long)]
        title: String,
        /// Issue body
        #[arg(long)]
        body: String,
        /// Agent run ID (defaults to $CONDUCTOR_RUN_ID env var)
        #[arg(long)]
        run_id: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum WorkflowCommands {
    /// List active workflow runs across all repos (optionally notify Slack)
    Active {
        /// Post the summary to the configured Slack webhook
        #[arg(long)]
        slack: bool,
    },
    /// List workflow run history for a repository
    Runs {
        /// Repository slug
        repo: String,
        /// Filter by worktree slug
        worktree: Option<String>,
    },
    /// List available workflow definitions for a repo/worktree
    List {
        /// Repo slug (required unless --path is given)
        #[arg(required_unless_present = "path")]
        repo: Option<String>,
        /// Worktree slug (required unless --path is given)
        #[arg(required_unless_present = "path")]
        worktree: Option<String>,
        /// Path to a repo root directory; skips DB lookup
        #[arg(long, conflicts_with_all = &["repo", "worktree"])]
        path: Option<String>,
    },
    /// Run a workflow
    #[command(
        after_help = "Examples:\n  conductor workflow run ticket-to-pr my-repo my-worktree\n  conductor workflow run ticket-to-pr my-repo my-worktree --input key=value\n  conductor workflow run draft-release-notes --pr https://github.com/org/repo/pull/42\n  conductor workflow run publish-docs --pr org/repo#42 --input force=true\n  conductor workflow run draft-release-notes --repo my-repo\n  conductor workflow run workflow-postmortem --workflow-run 01ABC123\n  conductor workflow run ticket-to-pr --ticket 01KKFYDVE7F0X5KPR9Q7CX6SJ3"
    )]
    Run {
        /// Workflow name (must match a .conductor/workflows/<name>.wf file)
        name: String,
        /// Repo slug (required unless --pr, --repo, --workflow-run, or --ticket is used)
        #[arg(required_unless_present_any = &["pr", "repo_flag", "workflow_run", "ticket"])]
        repo: Option<String>,
        /// Worktree slug (required unless --pr, --repo, --workflow-run, or --ticket is used)
        #[arg(required_unless_present_any = &["pr", "repo_flag", "workflow_run", "ticket"])]
        worktree: Option<String>,
        /// Run the workflow against a GitHub PR URL or reference (e.g. https://github.com/owner/repo/pull/123)
        #[arg(long, conflicts_with_all = &["repo", "worktree", "repo_flag", "workflow_run", "ticket"])]
        pr: Option<String>,
        /// Run a repo-targeted workflow without a worktree (conflicts with positional repo/worktree and --pr)
        #[arg(long = "repo", conflicts_with_all = &["repo", "worktree", "pr", "workflow_run", "ticket"])]
        repo_flag: Option<String>,
        /// Run the workflow targeting a prior workflow run (e.g. for postmortem workflows)
        #[arg(long, conflicts_with_all = &["repo", "worktree", "pr", "repo_flag", "ticket"])]
        workflow_run: Option<String>,
        /// Run the workflow against a ticket (ULID from `conductor ticket list`)
        #[arg(long, conflicts_with_all = &["repo", "worktree", "pr", "repo_flag", "workflow_run"])]
        ticket: Option<String>,
        /// Model to use for agent steps
        #[arg(long)]
        model: Option<String>,
        /// Dry-run mode: show what would be committed without doing it
        #[arg(long)]
        dry_run: bool,
        /// Continue past step failures
        #[arg(long)]
        no_fail_fast: bool,
        /// Step timeout in seconds (default: 43200 = 12 hours)
        #[arg(long, default_value = "43200")]
        step_timeout_secs: u64,
        /// Input variables (key=value pairs)
        #[arg(long = "input", value_name = "KEY=VALUE")]
        inputs: Vec<String>,
        /// Feature name — sets the feature context for the workflow run.
        /// When provided, feature_name and feature_branch become available as
        /// template variables. Auto-detected from the ticket when omitted.
        #[arg(long)]
        feature: Option<String>,
        /// Run the workflow in the background: print the run ID and exit immediately
        #[arg(long)]
        background: bool,
        /// Additional plugin directories to pass to agent sessions (appended to repo-level plugin_dirs)
        #[arg(long = "plugin-dir")]
        plugin_dirs: Vec<String>,
    },
    /// Show details of a workflow run
    #[command(name = "run-show", alias = "show")]
    RunShow {
        /// Workflow run ID
        id: String,
    },
    /// Validate a workflow definition (check all agents exist)
    Validate {
        /// Workflow name (required unless --all is set)
        name: Option<String>,
        /// Validate all workflows in .conductor/workflows/
        #[arg(long, conflicts_with = "name")]
        all: bool,
        /// Repo slug (optional; auto-detected from CWD when omitted)
        repo: Option<String>,
        /// Worktree slug (optional; auto-detected from CWD when omitted)
        worktree: Option<String>,
        /// Path to a repo root directory; skips DB lookup
        #[arg(long, conflicts_with_all = &["repo", "worktree"])]
        path: Option<String>,
    },
    /// Resume a failed or stalled workflow run
    #[command(
        after_help = "Examples:\n  conductor workflow resume 01ABC123 --restart\n  conductor workflow resume 01ABC123 --from-step run-tests\n  # Find run IDs: conductor workflow runs my-repo"
    )]
    Resume {
        /// Workflow run ID
        id: String,
        /// Resume from a specific step name (re-runs from that step onward)
        #[arg(long)]
        from_step: Option<String>,
        /// Model override for agent steps
        #[arg(long)]
        model: Option<String>,
        /// Restart from the beginning (reuse same run record)
        #[arg(long)]
        restart: bool,
    },
    /// Cancel a running or waiting workflow
    Cancel {
        /// Workflow run ID
        id: String,
    },
    /// Approve a pending human gate
    #[command(
        after_help = "A gate pauses the workflow at a checkpoint waiting for human approval.\nFind the RUN_ID with: conductor workflow runs <repo>"
    )]
    GateApprove {
        /// Workflow run ID — find it with: conductor workflow runs <repo>
        #[arg(value_name = "RUN_ID")]
        run_id: String,
    },
    /// Reject a pending human gate (fails the workflow)
    #[command(
        after_help = "A gate pauses the workflow at a checkpoint waiting for human approval.\nFind the RUN_ID with: conductor workflow runs <repo>"
    )]
    GateReject {
        /// Workflow run ID — find it with: conductor workflow runs <repo>
        #[arg(value_name = "RUN_ID")]
        run_id: String,
    },
    /// Provide feedback and approve a pending human gate
    #[command(
        after_help = "A gate pauses the workflow at a checkpoint waiting for human approval.\nFind the RUN_ID with: conductor workflow runs <repo>"
    )]
    GateFeedback {
        /// Workflow run ID — find it with: conductor workflow runs <repo>
        #[arg(value_name = "RUN_ID")]
        run_id: String,
        /// Feedback text
        feedback: String,
    },
    /// Delete completed, failed, and cancelled workflow runs
    Purge {
        /// Only purge runs for this repo slug
        #[arg(long)]
        repo: Option<String>,
        /// Filter by status: completed, failed, cancelled, all (default: all terminal)
        #[arg(long)]
        status: Option<String>,
        /// Print what would be deleted without deleting
        #[arg(long)]
        dry_run: bool,
    },
    /// List available workflow templates (built-in scaffolding)
    #[command(name = "template-list")]
    TemplateList,
    /// Show details of a workflow template
    #[command(name = "template-show")]
    TemplateShow {
        /// Template name
        name: String,
    },
    /// Scaffold a new workflow from a built-in template (agent-assisted)
    #[command(
        name = "from-template",
        after_help = "Examples:\n  conductor workflow from-template create-issue my-repo\n  conductor workflow from-template create-issue my-repo my-worktree"
    )]
    FromTemplate {
        /// Template name (see `workflow template-list`)
        template: String,
        /// Repo slug
        repo: String,
        /// Worktree slug (optional; uses repo root if omitted)
        worktree: Option<String>,
    },
    /// Upgrade a workflow to match a newer template version (agent-assisted)
    #[command(
        name = "upgrade-from-template",
        after_help = "Examples:\n  conductor workflow upgrade-from-template create-issue my-repo"
    )]
    UpgradeFromTemplate {
        /// Template name
        template: String,
        /// Repo slug
        repo: String,
        /// Worktree slug (optional)
        worktree: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RepoCommands {
    /// Register a repository
    Register {
        /// Git remote URL
        remote_url: String,
        /// Short slug for the repo
        #[arg(long)]
        slug: Option<String>,
        /// Local path to existing checkout (skips clone)
        #[arg(long)]
        local_path: Option<String>,
        /// Workspace directory for worktrees
        #[arg(long)]
        workspace: Option<String>,
    },
    /// List all repositories
    List,
    /// Discover repos from your GitHub account or an org (requires gh CLI).
    /// Omit <owner> to list orgs; pass an org name to list its repos.
    Discover {
        /// GitHub org login, or omit to list available orgs
        owner: Option<String>,
    },
    /// Unregister a repository
    Unregister {
        /// Repo slug
        slug: String,
    },
    /// Set (or clear) the per-repo default model for agent runs
    SetModel {
        /// Repo slug
        slug: String,
        /// Model alias or ID (e.g. "sonnet", "claude-opus-4-6"). Omit to clear.
        model: Option<String>,
    },
    /// Manage issue sources for a repository
    Sources {
        #[command(subcommand)]
        command: SourceCommands,
    },
    /// Allow or disallow agents to create issues for a repository
    AllowAgentIssues {
        /// Repo slug
        slug: String,
        /// Set to true to allow, false to disallow
        #[arg(long, default_value = "true")]
        allow: bool,
    },
}

#[derive(Subcommand)]
pub enum SourceCommands {
    /// Add an issue source
    Add {
        /// Repo slug
        slug: String,
        /// Source type (github or jira)
        #[arg(long = "type")]
        source_type: String,
        /// JSON config (auto-inferred for github from remote URL if omitted)
        #[arg(long)]
        config: Option<String>,
    },
    /// List issue sources for a repo
    List {
        /// Repo slug
        slug: String,
    },
    /// Remove an issue source
    Remove {
        /// Repo slug
        slug: String,
        /// Source type to remove (github or jira)
        #[arg(long = "type")]
        source_type: String,
    },
}

#[derive(Subcommand)]
pub enum WorktreeCommands {
    /// Create a new worktree
    #[command(
        after_help = "Examples:\n  conductor worktree create my-repo --ticket PROJ-42\n  conductor worktree create my-repo --from main\n  conductor worktree create my-repo --ticket PROJ-42 --auto-agent\n  conductor worktree create my-repo pr-42-fix --from-pr 42"
    )]
    Create {
        /// Repo slug
        repo: String,
        /// Worktree name (e.g., smart-playlists, fix-scan-crash)
        name: String,
        /// Base branch
        #[arg(long, short, conflicts_with_all = &["from_pr", "feature"])]
        from: Option<String>,
        /// Checkout an existing PR branch by PR number
        #[arg(long, conflicts_with_all = &["from", "feature"])]
        from_pr: Option<u32>,
        /// Create worktree based on a feature branch
        #[arg(long, conflicts_with_all = &["from", "from_pr"])]
        feature: Option<String>,
        /// Link to a ticket ID
        #[arg(long)]
        ticket: Option<String>,
        /// Auto-start an agent after creation (requires --ticket)
        #[arg(long)]
        auto_agent: bool,
        /// Proceed even if the base branch has uncommitted changes
        #[arg(long)]
        force: bool,
    },
    /// List worktrees
    List {
        /// Filter by repo slug
        repo: Option<String>,
    },
    /// Delete a worktree (soft-delete: marks as merged or abandoned)
    Delete {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
    },
    /// Permanently remove completed worktree records
    Purge {
        /// Repo slug
        repo: String,
        /// Specific worktree slug (purges all completed if omitted)
        name: Option<String>,
    },
    /// Push worktree branch to origin
    Push {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
    },
    /// Create a pull request for the worktree branch
    Pr {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
        /// Create as draft PR
        #[arg(long)]
        draft: bool,
    },
    /// Set (or clear) the per-worktree default model for agent runs
    SetModel {
        /// Repo slug
        repo: String,
        /// Worktree slug
        name: String,
        /// Model alias or full ID (e.g. "sonnet", "claude-opus-4-6"). Omit to clear.
        model: Option<String>,
    },
    /// Detect merged PRs and clean up their worktrees (branch + directory)
    Cleanup {
        /// Repo slug (cleans all repos if omitted)
        repo: Option<String>,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum TicketCommands {
    /// Sync tickets from configured sources
    Sync {
        /// Repo slug (syncs all if omitted)
        repo: Option<String>,
    },
    /// List cached tickets
    List {
        /// Filter by repo slug
        repo: Option<String>,
    },
    /// Get a single ticket by ID (ULID or source_id)
    Get {
        /// Ticket ID — internal ULID or source_id (falls back to source_id search)
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Output format: "text" (default) or "json"
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Link a ticket to a worktree
    Link {
        /// Ticket source ID (e.g., GitHub issue number)
        ticket: String,
        /// Repo slug
        repo: String,
        /// Worktree slug
        worktree: String,
    },
    /// Show aggregate agent cost/turns/time per ticket
    Stats {
        /// Filter by repo slug
        repo: Option<String>,
    },
    /// Delete a ticket by its source key
    Delete {
        /// Repo slug
        repo: String,
        /// Source type (e.g. "github", "jira", "linear")
        #[arg(long)]
        source_type: String,
        /// Source ID (e.g. issue number or key)
        #[arg(long)]
        source_id: String,
    },
    /// Create or update a ticket from an external source
    Upsert {
        /// Repo slug
        repo: String,
        /// Source type (e.g. "github", "jira", "linear")
        #[arg(long)]
        source_type: String,
        /// Source ID (e.g. issue number or key)
        #[arg(long)]
        source_id: String,
        /// Ticket title
        #[arg(long)]
        title: String,
        /// Ticket state: open, in_progress, or closed
        #[arg(long)]
        state: String,
        /// Ticket body/description
        #[arg(long, default_value = "")]
        body: String,
        /// URL to the ticket
        #[arg(long, default_value = "")]
        url: String,
        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,
        /// Assignee
        #[arg(long)]
        assignee: Option<String>,
        /// Priority
        #[arg(long)]
        priority: Option<String>,
        /// Workflow name override (bypasses routing heuristics)
        #[arg(long)]
        workflow: Option<String>,
        /// Agent map JSON (pre-resolved agent assignments)
        #[arg(long)]
        agent_map: Option<String>,
        /// Source ID of the parent ticket within the same source_type (replaces any existing parent)
        #[arg(long)]
        parent: Option<String>,
    },
    /// Update a ticket's state, workflow, or agent_map
    Update {
        /// Ticket ID (ULID from `conductor tickets list`)
        id: String,
        /// Set state: open, in_progress, or closed
        #[arg(long)]
        state: Option<String>,
        /// Set workflow name (empty string clears)
        #[arg(long)]
        workflow: Option<String>,
        /// Set agent map JSON (empty string clears)
        #[arg(long)]
        agent_map: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum FeatureCommands {
    /// Create a new feature branch
    #[command(
        after_help = "Examples:\n  conductor feature create my-repo notification-improvements\n  conductor feature create my-repo notification-improvements --from develop\n  conductor feature create my-repo notification-improvements --tickets 1262,1263"
    )]
    Create {
        /// Repo slug
        repo: String,
        /// Feature name (e.g., notification-improvements)
        name: String,
        /// Base branch (defaults to repo's default branch)
        #[arg(long)]
        from: Option<String>,
        /// Comma-separated ticket source IDs to link
        #[arg(long)]
        tickets: Option<String>,
    },
    /// List features for a repo
    List {
        /// Repo slug
        repo: String,
    },
    /// Link tickets to a feature
    Link {
        /// Repo slug
        repo: String,
        /// Feature name
        name: String,
        /// Comma-separated ticket source IDs
        tickets: String,
    },
    /// Unlink tickets from a feature
    Unlink {
        /// Repo slug
        repo: String,
        /// Feature name
        name: String,
        /// Comma-separated ticket source IDs
        tickets: String,
    },
    /// Create a pull request for the feature branch
    Pr {
        /// Repo slug
        repo: String,
        /// Feature name
        name: String,
        /// Create as draft PR
        #[arg(long)]
        draft: bool,
    },
    /// Close a feature (marks as merged if branch was merged, otherwise closed)
    Close {
        /// Repo slug
        repo: String,
        /// Feature name
        name: String,
    },
    /// Permanently delete a closed feature (removes DB record, feature_tickets, and git branch)
    Delete {
        /// Repo slug
        repo: String,
        /// Feature name
        name: String,
    },
}
