//! Embeds built-in `.wf` workflow files into the binary at compile time.
//!
//! Each file in `conductor-core/builtin_workflows/*.wf` is included via
//! `include_str!` and parsed on first access. Built-in workflows are
//! discoverable alongside repo-level definitions but cannot be deleted and
//! are always version-matched to the binary.

use crate::workflow_dsl::{parse_workflow_str, WorkflowDef, WorkflowSource, WorkflowWarning};

/// `(name, source_text)` pairs for every embedded `.wf` file.
///
/// Add a new entry here when shipping a new built-in workflow.
const BUILTIN_SOURCES: &[(&str, &str)] =
    &[("hello", include_str!("../builtin_workflows/hello.wf"))];

/// Parse all embedded workflow sources and return `(defs, warnings)`.
///
/// Mirrors the signature of `load_workflow_defs` so callers can merge the
/// results directly.
pub fn load_builtin_defs() -> (Vec<WorkflowDef>, Vec<WorkflowWarning>) {
    let mut defs = Vec::new();
    let mut warnings = Vec::new();

    for (name, source) in BUILTIN_SOURCES {
        match parse_workflow_str(source, "<builtin>") {
            Ok(mut def) => {
                def.source = WorkflowSource::BuiltIn;
                def.source_path = "<builtin>".to_string();
                defs.push(def);
            }
            Err(e) => {
                tracing::warn!("Failed to parse built-in workflow '{name}': {e}");
                warnings.push(WorkflowWarning {
                    file: format!("{name}.wf"),
                    message: e.to_string(),
                });
            }
        }
    }

    (defs, warnings)
}

/// Load a single built-in workflow by name, or `None` if not found.
pub fn load_builtin_by_name(name: &str) -> Option<WorkflowDef> {
    BUILTIN_SOURCES
        .iter()
        .find(|(n, _)| *n == name)
        .and_then(|(_, source)| {
            parse_workflow_str(source, "<builtin>").ok().map(|mut def| {
                def.source = WorkflowSource::BuiltIn;
                def.source_path = "<builtin>".to_string();
                def
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_builtin_defs_parses_all() {
        let (defs, warnings) = load_builtin_defs();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(!defs.is_empty(), "expected at least one built-in def");
        for def in &defs {
            assert_eq!(def.source, WorkflowSource::BuiltIn);
            assert_eq!(def.source_path, "<builtin>");
        }
    }

    #[test]
    fn test_load_builtin_by_name_found() {
        let def = load_builtin_by_name("hello");
        assert!(def.is_some());
        let def = def.unwrap();
        assert_eq!(def.name, "hello");
        assert_eq!(def.source, WorkflowSource::BuiltIn);
    }

    #[test]
    fn test_load_builtin_by_name_not_found() {
        assert!(load_builtin_by_name("nonexistent").is_none());
    }
}
