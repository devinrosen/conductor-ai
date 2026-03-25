use crate::workflow::load_workflow_by_name;
use crate::workflow_dsl::WorkflowSource;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_load_workflow_by_name_falls_back_to_builtin() {
    let tmp = TempDir::new().unwrap();
    // No .conductor/workflows/ directory exists — should fall back to built-in.
    let def = load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "hello").unwrap();
    assert_eq!(def.name, "hello");
    assert_eq!(def.source, WorkflowSource::BuiltIn);
}

#[test]
fn test_load_workflow_by_name_not_found_anywhere() {
    let tmp = TempDir::new().unwrap();
    let result = load_workflow_by_name(
        tmp.path().to_str().unwrap(),
        "/nonexistent",
        "no-such-workflow",
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not found"),
        "expected 'not found' in error: {msg}"
    );
}

#[test]
fn test_load_workflow_by_name_propagates_parse_error() {
    let tmp = TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("broken.wf"),
        "this is not valid workflow syntax {{{{",
    )
    .unwrap();

    let result = load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "broken");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    // Parse errors should NOT be silently swallowed — they must propagate,
    // not fall back to a built-in.
    assert!(
        !msg.contains("not found in .conductor/workflows/ or built-in"),
        "parse error should propagate, not trigger built-in fallback: {msg}"
    );
}
