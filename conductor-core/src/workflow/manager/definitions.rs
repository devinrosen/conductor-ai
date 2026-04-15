use std::collections::HashSet;

use crate::error::Result;
use crate::workflow_dsl;

use super::WorkflowManager;

/// Invalid workflow entry: (name, error_message).
type InvalidWorkflowEntry = (String, String);

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

    /// Load workflow definitions and run full validation (parse + post-parse).
    ///
    /// Returns `(valid_defs, invalid_entries)` where:
    /// - `valid_defs` are successfully-parsed and validated definitions
    /// - `invalid_entries` are invalid workflows with their error messages
    ///
    /// Invalid entries include both parse failures (from `WorkflowWarning`) and
    /// validation errors from the batch validation pipeline.
    ///
    /// This is the authoritative method for loading and validating workflows
    /// for UI display — it handles both parse and post-parse validation in one call.
    pub fn list_defs_with_validation(
        wt_path: &str,
        repo_path: &str,
        known_bots: &HashSet<String>,
    ) -> Result<(
        Vec<crate::workflow_dsl::WorkflowDef>,
        Vec<InvalidWorkflowEntry>,
    )> {
        let (defs, warnings) = Self::list_defs(wt_path, repo_path).unwrap_or_default();

        // Convert parse failures to invalid entries.
        let mut invalid_entries: Vec<InvalidWorkflowEntry> = warnings
            .iter()
            .map(|w| {
                let name = std::path::Path::new(&w.file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&w.file)
                    .to_string();
                (name, w.message.clone())
            })
            .collect();

        // Run post-parse validation on successfully-parsed defs.
        let wt = wt_path.to_string();
        let rp = repo_path.to_string();
        let loader = |name: &str| -> std::result::Result<crate::workflow_dsl::WorkflowDef, String> {
            workflow_dsl::load_workflow_by_name(&wt, &rp, name).map_err(|e| e.to_string())
        };

        let validation = crate::workflow::batch_validate::validate_workflows_batch(
            &defs,
            &[],
            wt_path,
            repo_path,
            known_bots,
            &loader,
        );

        // Add validation errors to invalid_entries.
        for entry in &validation.entries {
            if !entry.errors.is_empty() {
                let msg = entry
                    .errors
                    .iter()
                    .map(|v| v.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                invalid_entries.push((entry.name.clone(), msg));
            }
        }

        // Filter out defs that failed validation.
        let valid_defs: Vec<_> = defs
            .into_iter()
            .filter(|d| !validation.entries.iter().any(|e| e.name == d.name && !e.errors.is_empty()))
            .collect();

        Ok((valid_defs, invalid_entries))
    }
}
