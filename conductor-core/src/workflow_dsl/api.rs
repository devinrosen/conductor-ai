use std::fs;

use crate::error::{ConductorError, Result};
use crate::text_util::{resolve_conductor_subdir, resolve_conductor_subdir_for_file};

use super::parser::parse_workflow_file;
use super::types::{collect_workflow_refs, WorkflowDef, WorkflowWarning};

// ---------------------------------------------------------------------------
// Public API / loaders
// ---------------------------------------------------------------------------

/// Load all workflow definitions from `.conductor/workflows/*.wf`.
///
/// Returns `(defs, warnings)` where `warnings` contains one [`WorkflowWarning`]
/// per file that failed to parse. Callers receive all successfully-parsed
/// definitions even when some files are broken.
pub fn load_workflow_defs(
    worktree_path: &str,
    repo_path: &str,
) -> Result<(Vec<WorkflowDef>, Vec<WorkflowWarning>)> {
    let workflows_dir = match resolve_conductor_subdir(worktree_path, repo_path, "workflows") {
        Some(dir) => dir,
        None => return Ok((Vec::new(), Vec::new())),
    };

    let mut entries: Vec<_> = fs::read_dir(&workflows_dir)
        .map_err(|e| {
            ConductorError::Workflow(format!("Failed to read {}: {e}", workflows_dir.display()))
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "wf"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let mut defs = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries {
        let path = entry.path();
        match parse_workflow_file(&path) {
            Ok(def) => defs.push(def),
            Err(e) => {
                let file = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                tracing::warn!("Failed to parse {file}: {e}");
                warnings.push(WorkflowWarning {
                    file,
                    message: e.to_string(),
                });
            }
        }
    }
    Ok((defs, warnings))
}

/// Validate that a workflow name is safe for use in filesystem paths.
///
/// Only alphanumeric characters, hyphens, and underscores are allowed.
/// This prevents path traversal when names are used to construct file paths.
pub fn validate_workflow_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConductorError::Workflow(
            "Workflow name must not be empty".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConductorError::Workflow(format!(
            "Invalid workflow name '{name}': only alphanumeric characters, hyphens, and underscores are allowed"
        )));
    }
    Ok(())
}

/// Load a single workflow definition by name.
pub fn load_workflow_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
) -> Result<WorkflowDef> {
    validate_workflow_name(name)?;

    let filename = format!("{name}.wf");
    let workflows_dir =
        resolve_conductor_subdir_for_file(worktree_path, repo_path, "workflows", &filename)
            .ok_or_else(|| {
                ConductorError::Workflow(format!(
                    "Workflow '{name}' not found in .conductor/workflows/"
                ))
            })?;

    parse_workflow_file(&workflows_dir.join(&filename))
}

/// Maximum allowed workflow nesting depth.
pub const MAX_WORKFLOW_DEPTH: u32 = 5;

/// Detect circular workflow references via static reachability analysis.
///
/// Returns `Ok(())` if no cycles exist, or an error naming the cycle path.
/// The `loader` callback loads a workflow by name — this keeps the function
/// testable without touching the filesystem.
pub fn detect_workflow_cycles<F>(root_name: &str, loader: &F) -> std::result::Result<(), String>
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    let mut visited = Vec::new();
    detect_cycles_inner(root_name, loader, &mut visited)
}

fn detect_cycles_inner<F>(
    name: &str,
    loader: &F,
    stack: &mut Vec<String>,
) -> std::result::Result<(), String>
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    if stack.contains(&name.to_string()) {
        stack.push(name.to_string());
        let cycle_path = stack.join(" -> ");
        return Err(format!("Circular workflow reference: {cycle_path}"));
    }

    if stack.len() >= MAX_WORKFLOW_DEPTH as usize {
        return Err(format!(
            "Workflow nesting depth exceeds maximum of {MAX_WORKFLOW_DEPTH}: {}",
            stack.join(" -> ")
        ));
    }

    stack.push(name.to_string());

    let def = loader(name)?;
    let mut child_refs = collect_workflow_refs(&def.body);
    child_refs.extend(collect_workflow_refs(&def.always));
    child_refs.sort();
    child_refs.dedup();

    for child_name in &child_refs {
        detect_cycles_inner(child_name, loader, stack)?;
    }

    stack.pop();
    Ok(())
}
