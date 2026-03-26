use std::path::{Path, PathBuf};

use super::loader::{find_schema_path, parse_schema_file, validate_name_segment};
use super::types::SchemaRef;

/// An issue detected when checking a schema reference during validation.
#[derive(Debug)]
pub enum SchemaIssue {
    /// Schema file was not found at any of the search paths.
    Missing(String),
    /// Schema file was found but could not be parsed or is structurally invalid.
    Invalid { name: String, error: String },
}

/// Locate the filesystem path for a schema short-name without parsing it.
///
/// Returns `None` if the file does not exist at any expected location.
fn locate_schema_name_path(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Option<PathBuf> {
    let filename = format!("{name}.yaml");
    let bases = [worktree_path, repo_path];
    if let Some(wf_name) = workflow_name {
        let subdir = Path::new(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("schemas");
        if let Some(path) = find_schema_path(&bases, &subdir, &filename) {
            return Some(path);
        }
    }
    find_schema_path(&bases, Path::new(".conductor/schemas"), &filename)
}

/// Check all referenced output schemas and return any issues found.
///
/// Unlike [`find_missing_schemas`], this function distinguishes between schemas
/// that cannot be found and schemas that exist but contain invalid content.
pub fn check_schemas(
    worktree_path: &str,
    repo_path: &str,
    schema_names: &[String],
    workflow_name: Option<&str>,
) -> Vec<SchemaIssue> {
    schema_names
        .iter()
        .filter_map(|name| {
            let schema_ref = SchemaRef::from_str_value(name);
            let path = match &schema_ref {
                SchemaRef::Name(n) => {
                    if let Err(e) = validate_name_segment(n, "Schema name") {
                        return Some(SchemaIssue::Invalid {
                            name: name.clone(),
                            error: e.to_string(),
                        });
                    }
                    if let Some(wf) = workflow_name {
                        if let Err(e) = validate_name_segment(wf, "Workflow name") {
                            return Some(SchemaIssue::Invalid {
                                name: name.clone(),
                                error: e.to_string(),
                            });
                        }
                    }
                    match locate_schema_name_path(worktree_path, repo_path, n, workflow_name) {
                        None => return Some(SchemaIssue::Missing(name.clone())),
                        Some(p) => p,
                    }
                }
                SchemaRef::Path(rel) => {
                    if Path::new(rel).is_absolute() {
                        return Some(SchemaIssue::Invalid {
                            name: name.clone(),
                            error: format!(
                                "Explicit schema path '{rel}' must be relative, not absolute"
                            ),
                        });
                    }
                    let repo_root = PathBuf::from(repo_path);
                    let joined = repo_root.join(rel);
                    if !joined.is_file() {
                        return Some(SchemaIssue::Missing(name.clone()));
                    }
                    // Enforce the same path-traversal guard as load_schema_by_path
                    if let (Ok(canonical), Ok(canonical_repo)) =
                        (joined.canonicalize(), repo_root.canonicalize())
                    {
                        if !canonical.starts_with(&canonical_repo) {
                            return Some(SchemaIssue::Invalid {
                                name: name.clone(),
                                error: format!(
                                    "Schema path '{rel}' escapes the repository root — path traversal is not allowed"
                                ),
                            });
                        }
                        canonical
                    } else {
                        joined
                    }
                }
            };
            match parse_schema_file(&path) {
                Ok(_) => None,
                Err(e) => Some(SchemaIssue::Invalid {
                    name: name.clone(),
                    error: e.to_string(),
                }),
            }
        })
        .collect()
}

/// Check whether all referenced output schemas exist (for validation).
///
/// Returns a list of schema names/paths that are truly missing (not found at
/// any search path). Schemas that exist but contain invalid YAML are **not**
/// included; use [`check_schemas`] to surface those separately.
pub fn find_missing_schemas(
    worktree_path: &str,
    repo_path: &str,
    schema_names: &[String],
    workflow_name: Option<&str>,
) -> Vec<String> {
    check_schemas(worktree_path, repo_path, schema_names, workflow_name)
        .into_iter()
        .filter_map(|issue| match issue {
            SchemaIssue::Missing(name) => Some(name),
            SchemaIssue::Invalid { .. } => None,
        })
        .collect()
}
