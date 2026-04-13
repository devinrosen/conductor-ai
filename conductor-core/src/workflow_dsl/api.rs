use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::error::{ConductorError, Result};
use crate::text_util::resolve_conductor_subdir_for_file;

use super::parser::parse_workflow_file;
use super::types::{collect_workflow_refs, WorkflowDef, WorkflowWarning};

// ---------------------------------------------------------------------------
// Public API / loaders
// ---------------------------------------------------------------------------

/// Load all workflow definitions from `.conductor/workflows/*.wf`.
///
/// Merges definitions from both `repo_path` and `worktree_path`. Worktree
/// definitions override repo definitions when both define a workflow with
/// the same name (keyed by `def.name`, not filename).
///
/// Returns `(defs, warnings)` where `warnings` contains one [`WorkflowWarning`]
/// per file that failed to parse. Callers receive all successfully-parsed
/// definitions even when some files are broken.
pub fn load_workflow_defs(
    worktree_path: &str,
    repo_path: &str,
) -> Result<(Vec<WorkflowDef>, Vec<WorkflowWarning>)> {
    let mut map: HashMap<String, WorkflowDef> = HashMap::new();
    let mut all_warnings: Vec<WorkflowWarning> = Vec::new();

    // Load repo defs first (lower priority).
    if !repo_path.is_empty() {
        let repo_dir = Path::new(repo_path).join(".conductor").join("workflows");
        if repo_dir.is_dir() {
            let (defs, warnings) = scan_wf_dir(&repo_dir)?;
            for def in defs {
                map.insert(def.name.clone(), def);
            }
            all_warnings.extend(warnings);
        }
    }

    // Load worktree defs second (higher priority — overwrite repo defs on name conflict).
    // Guard: skip if worktree_path is empty or identical to repo_path (avoids double-counting).
    if !worktree_path.is_empty() && worktree_path != repo_path {
        let wt_dir = Path::new(worktree_path)
            .join(".conductor")
            .join("workflows");
        if wt_dir.is_dir() {
            let (defs, warnings) = scan_wf_dir(&wt_dir)?;
            for def in defs {
                map.insert(def.name.clone(), def);
            }
            all_warnings.extend(warnings);
        }
    }

    let mut defs: Vec<WorkflowDef> = map.into_values().collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok((defs, all_warnings))
}

/// Scan a single `.wf` directory and return parsed defs + parse warnings.
fn scan_wf_dir(dir: &Path) -> Result<(Vec<WorkflowDef>, Vec<WorkflowWarning>)> {
    let mut entries = filter_wf_dir_entries(
        fs::read_dir(dir).map_err(|e| {
            ConductorError::Workflow(format!("Failed to read {}: {e}", dir.display()))
        })?,
        dir,
    );

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

/// Collect valid `.wf` directory entries, skipping and logging any iterator errors.
///
/// This is extracted to allow unit testing of the DirEntry error-skip path without
/// requiring OS-level filesystem tricks.
pub(crate) fn filter_wf_dir_entries(
    iter: impl Iterator<Item = std::io::Result<fs::DirEntry>>,
    dir_path: &std::path::Path,
) -> Vec<fs::DirEntry> {
    iter.filter_map(|e| match e {
        Ok(entry) => Some(entry),
        Err(err) => {
            tracing::warn!(
                "Failed to read directory entry in {}: {err}",
                dir_path.display()
            );
            None
        }
    })
    .filter(|e| e.path().extension().is_some_and(|ext| ext == "wf"))
    .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_wf_file(dir: &std::path::Path, filename: &str, content: &str) {
        let workflows_dir = dir.join(".conductor").join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(workflows_dir.join(filename), content).unwrap();
    }

    // Minimal valid .wf files with distinct workflow names, using the simple
    // single-line DSL format verified by existing parser tests.
    const WF_SHARED: &str = "workflow shared { meta { targets = [\"worktree\"] } call build }";
    const WF_LOCAL: &str = "workflow local { meta { targets = [\"worktree\"] } call build }";
    const WF_DEPLOY_REPO: &str = "workflow deploy { meta { targets = [\"worktree\"] } call build }";
    // Worktree version of "deploy" — identical name but different source_path in assertion.
    const WF_DEPLOY_WT: &str = "workflow deploy { meta { targets = [\"worktree\"] } call release }";

    #[test]
    fn test_load_workflow_defs_repo_only() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_wf_file(repo.path(), "shared.wf", WF_SHARED);

        let (defs, warnings) = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "shared");
    }

    #[test]
    fn test_load_workflow_defs_worktree_only() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_wf_file(worktree.path(), "local.wf", WF_LOCAL);

        let (defs, warnings) = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "local");
    }

    #[test]
    fn test_load_workflow_defs_merge_no_conflict() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_wf_file(repo.path(), "shared.wf", WF_SHARED);
        write_wf_file(worktree.path(), "local.wf", WF_LOCAL);

        let (defs, warnings) = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"shared"));
        assert!(names.contains(&"local"));
    }

    #[test]
    fn test_load_workflow_defs_merge_worktree_wins() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        // Both define a workflow named "deploy" — worktree version should win.
        write_wf_file(repo.path(), "deploy.wf", WF_DEPLOY_REPO);
        write_wf_file(worktree.path(), "deploy.wf", WF_DEPLOY_WT);

        let (defs, warnings) = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        // Only one "deploy" workflow should survive — the worktree version.
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "deploy");
        assert!(defs[0]
            .source_path
            .contains(worktree.path().to_str().unwrap()));
    }

    #[test]
    fn test_validate_workflow_name_rejects_path_separators() {
        assert!(validate_workflow_name("../evil").is_err());
        assert!(validate_workflow_name("foo/bar").is_err());
        assert!(validate_workflow_name("..").is_err());
        assert!(validate_workflow_name(".").is_err());
    }

    #[test]
    fn test_validate_workflow_name_rejects_empty() {
        assert!(validate_workflow_name("").is_err());
    }

    #[test]
    fn test_validate_workflow_name_accepts_valid() {
        assert!(validate_workflow_name("deploy").is_ok());
        assert!(validate_workflow_name("my-workflow").is_ok());
        assert!(validate_workflow_name("build_release").is_ok());
        assert!(validate_workflow_name("ci-2024").is_ok());
    }
}
