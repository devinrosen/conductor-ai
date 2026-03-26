use std::collections::HashSet;

use crate::error::Result;
use crate::workflow_dsl;

use super::WorkflowManager;

impl WorkflowManager<'_> {
    /// Load workflow definitions from the filesystem for a worktree.
    ///
    /// Wraps `workflow_dsl::load_workflow_defs` so consumers don't need to
    /// reach into the low-level DSL module directly.
    ///
    /// Returns `(defs, warnings)` — warnings contain one [`WorkflowWarning`]
    /// per `.wf` file that failed to parse. Successfully-parsed definitions are
    /// always returned even when some files are broken.
    pub fn list_defs(
        worktree_path: &str,
        repo_path: &str,
    ) -> Result<(
        Vec<crate::workflow_dsl::WorkflowDef>,
        Vec<crate::workflow_dsl::WorkflowWarning>,
    )> {
        workflow_dsl::load_workflow_defs(worktree_path, repo_path)
    }

    /// Load a single workflow definition by name.
    pub fn load_def_by_name(
        worktree_path: &str,
        repo_path: &str,
        name: &str,
    ) -> Result<crate::workflow_dsl::WorkflowDef> {
        workflow_dsl::load_workflow_by_name(worktree_path, repo_path, name)
    }

    /// Validate a single workflow definition using the full batch validation
    /// pipeline (agents, snippets, schemas, cycles, semantics, scripts, bot names).
    ///
    /// This is a convenience wrapper around [`validate_workflows_batch`] for callers
    /// that already have a loaded `WorkflowDef` — e.g. the MCP tool.
    pub fn validate_single(
        wt_path: &str,
        repo_path: &str,
        workflow: &crate::workflow_dsl::WorkflowDef,
        known_bots: &HashSet<String>,
    ) -> crate::workflow::batch_validate::WorkflowValidationEntry {
        let wt = wt_path.to_string();
        let rp = repo_path.to_string();
        let loader = |name: &str| -> std::result::Result<crate::workflow_dsl::WorkflowDef, String> {
            workflow_dsl::load_workflow_by_name(&wt, &rp, name).map_err(|e| e.to_string())
        };
        let result = crate::workflow::batch_validate::validate_workflows_batch(
            std::slice::from_ref(workflow),
            &[],
            wt_path,
            repo_path,
            known_bots,
            &loader,
        );
        // validate_workflows_batch produces exactly one entry per input workflow,
        // so with a single-item slice this always yields one element.
        // Use unwrap_or_else to avoid a bare expect() in library code.
        result.entries.into_iter().next().unwrap_or_else(|| {
            crate::workflow::batch_validate::WorkflowValidationEntry {
                name: workflow.name.clone(),
                errors: vec![crate::workflow_dsl::ValidationError {
                    message: "internal error: batch validation returned no entries".to_string(),
                    hint: None,
                }],
                warnings: vec![],
            }
        })
    }
}
