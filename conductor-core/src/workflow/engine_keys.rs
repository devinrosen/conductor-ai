/// Conductor host-domain key constants for values injected into workflow runs.
///
/// Conductor owns and injects these keys; runkon-flow (the engine) treats
/// injected variables as opaque and does not reference these names.
pub const REPO_PATH: &str = "repo_path";
pub const WORKTREE_ID: &str = "worktree_id";
pub const TICKET_ID: &str = "ticket_id";
pub const REPO_ID: &str = "repo_id";
pub const WORKING_DIR: &str = "working_dir";
pub const WORKFLOW_RUN_ID: &str = "workflow_run_id";

/// Keys that conductor injects automatically from run context. Consumers can
/// use this slice to identify inputs that are read-only from the user's perspective.
pub const ENGINE_INJECTED_KEYS: &[&str] = &[
    "ticket_id",
    "ticket_source_id",
    "ticket_source_type",
    "ticket_title",
    "ticket_body",
    "ticket_url",
    "ticket_raw_json",
    "repo_id",
    "repo_path",
    "repo_name",
    "workflow_run_id",
];
