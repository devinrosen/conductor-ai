use crate::workflow_dsl::*;
use std::fs;

#[test]
fn test_load_from_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("simple.wf"),
        "workflow simple { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    let (defs, warnings) =
        load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "simple");
    assert!(warnings.is_empty());
}

#[test]
fn test_load_partial_failure_returns_successes_and_warnings() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    // Valid workflow
    fs::write(
        wf_dir.join("good.wf"),
        "workflow good { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();
    // Invalid workflow (syntax error)
    fs::write(
        wf_dir.join("bad.wf"),
        "this is not valid workflow syntax !!!",
    )
    .unwrap();

    let (defs, warnings) =
        load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
    // The good workflow is returned despite the bad one failing
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "good");
    // One warning for the bad file
    assert_eq!(warnings.len(), 1);
    // Warning carries the filename in the structured `file` field
    assert_eq!(warnings[0].file, "bad.wf");
    assert!(!warnings[0].message.is_empty());
}

#[test]
fn test_validate_workflow_name_valid() {
    assert!(validate_workflow_name("ticket-to-pr").is_ok());
    assert!(validate_workflow_name("test_coverage").is_ok());
    assert!(validate_workflow_name("simple").is_ok());
    assert!(validate_workflow_name("A-Z_0-9").is_ok());
}

#[test]
fn test_validate_workflow_name_empty() {
    assert!(validate_workflow_name("").is_err());
}

#[test]
fn test_validate_workflow_name_path_traversal() {
    assert!(validate_workflow_name("..").is_err());
    assert!(validate_workflow_name("../etc/passwd").is_err());
    assert!(validate_workflow_name("foo/bar").is_err());
    assert!(validate_workflow_name("foo\\bar").is_err());
}

#[test]
fn test_validate_workflow_name_special_chars() {
    assert!(validate_workflow_name("name with spaces").is_err());
    assert!(validate_workflow_name("name.wf").is_err());
    assert!(validate_workflow_name("name;rm -rf").is_err());
    assert!(validate_workflow_name("name\0null").is_err());
}

#[test]
fn test_load_workflow_by_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("deploy.wf"),
        "workflow deploy { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    let def =
        load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "deploy").unwrap();
    assert_eq!(def.name, "deploy");
}

#[test]
fn test_load_workflow_by_name_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("deploy.wf"),
        "workflow deploy { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    let result = load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "nonexistent");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"));
}

#[test]
fn test_load_workflow_by_name_rejects_invalid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = load_workflow_by_name(
        tmp.path().to_str().unwrap(),
        "/nonexistent",
        "../etc/passwd",
    );
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Invalid workflow name"));
}

#[test]
fn test_load_workflow_by_name_falls_back_to_repo_path() {
    let repo = tempfile::TempDir::new().unwrap();
    let wf_dir = repo.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    fs::write(
        wf_dir.join("deploy.wf"),
        "workflow deploy { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    // worktree has no .conductor/workflows/, should fall back to repo_path
    let worktree = tempfile::TempDir::new().unwrap();
    let def = load_workflow_by_name(
        worktree.path().to_str().unwrap(),
        repo.path().to_str().unwrap(),
        "deploy",
    )
    .unwrap();
    assert_eq!(def.name, "deploy");
}

#[test]
fn test_load_workflow_by_name_no_workflows_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = load_workflow_by_name(
        tmp.path().to_str().unwrap(),
        tmp.path().to_str().unwrap(),
        "deploy",
    );
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"));
}

/// Regression test for #1195: DirEntry errors silently dropped during workflow directory scan.
///
/// The original code used `.filter_map(|e| e.ok())`, which silently discarded DirEntry errors.
/// The fix emits a `tracing::warn!` and skips the bad entry so callers receive all successfully
/// parsed definitions.
///
/// This test exercises the `parse_workflow_file` error path: a `.wf` file with mode 000 causes
/// parsing to fail, which is collected as a `WorkflowWarning`. The sibling test
/// `test_filter_wf_dir_entries_skips_io_errors` directly exercises the DirEntry iterator-error
/// path (api.rs lines 31–39).
#[cfg(unix)]
#[test]
fn test_load_workflow_defs_skips_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let wf_dir = tmp.path().join(".conductor").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();

    // A valid workflow that should always be returned.
    fs::write(
        wf_dir.join("good.wf"),
        "workflow good { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();

    // A `.wf` file made unreadable — simulates a permission-denied scenario.
    let bad_path = wf_dir.join("unreadable.wf");
    fs::write(
        &bad_path,
        "workflow unreadable { meta { targets = [\"worktree\"] } call build }",
    )
    .unwrap();
    fs::set_permissions(&bad_path, fs::Permissions::from_mode(0o000)).unwrap();

    let result = load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent");

    // Restore permissions so TempDir cleanup doesn't fail.
    fs::set_permissions(&bad_path, fs::Permissions::from_mode(0o644)).unwrap();

    let (defs, warnings) = result.unwrap();
    // The readable workflow is returned.
    assert_eq!(defs.len(), 1, "expected exactly one parseable workflow");
    assert_eq!(defs[0].name, "good");
    // The unreadable file produces a warning (file-read error path), not a panic.
    assert_eq!(
        warnings.len(),
        1,
        "expected one warning for the unreadable file"
    );
    assert_eq!(warnings[0].file, "unreadable.wf");
}

/// Directly tests the DirEntry iterator-error path in `filter_wf_dir_entries` (api.rs lines 31–39).
///
/// Feeds synthetic `io::Error` values (which cannot be constructed from real filesystem calls in
/// tests) directly into the helper to confirm they are skipped rather than panicking or returning
/// an `Err`. Valid `.wf` DirEntries read from a temporary directory are passed through correctly.
#[test]
fn test_filter_wf_dir_entries_skips_io_errors() {
    use crate::workflow_dsl::api::filter_wf_dir_entries;
    use std::io;

    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();

    // Write a real .wf file and a non-.wf file to the temp dir.
    fs::write(dir.join("real.wf"), "content").unwrap();
    fs::write(dir.join("ignored.txt"), "content").unwrap();

    // Collect the real DirEntries first so we can chain them with synthetic errors.
    let real_entries: Vec<io::Result<fs::DirEntry>> = fs::read_dir(dir).unwrap().collect();

    // Prepend two synthetic DirEntry errors — these exercise the Err arm of the filter_map.
    let mixed = vec![
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "synthetic bad entry 1",
        )),
        Err(io::Error::other("synthetic bad entry 2")),
    ]
    .into_iter()
    .chain(real_entries);

    let result = filter_wf_dir_entries(mixed, dir);

    // Only the real .wf file survives; errors and non-.wf files are dropped.
    assert_eq!(
        result.len(),
        1,
        "errors and non-.wf entries must be skipped"
    );
    assert_eq!(
        result[0].path().file_name().unwrap().to_str().unwrap(),
        "real.wf"
    );
}
