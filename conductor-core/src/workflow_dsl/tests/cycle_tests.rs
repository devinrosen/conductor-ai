use crate::workflow_dsl::*;

fn no_loader(name: &str) -> std::result::Result<WorkflowDef, String> {
    Err(format!("no loader: {name}"))
}

#[test]
fn test_detect_workflow_cycles_no_cycle() {
    let result = detect_workflow_cycles("a", &|name| match name {
        "a" => parse_workflow_str(
            "workflow a { meta { targets = [\"worktree\"] } call workflow b }",
            "a.wf",
        )
        .map_err(|e| e.to_string()),
        "b" => parse_workflow_str(
            "workflow b { meta { targets = [\"worktree\"] } call agent }",
            "b.wf",
        )
        .map_err(|e| e.to_string()),
        other => Err(format!("Unknown workflow: {other}")),
    });
    assert!(result.is_ok());
}

#[test]
fn test_detect_workflow_cycles_direct_cycle() {
    let result = detect_workflow_cycles("a", &|name| match name {
        "a" => parse_workflow_str(
            "workflow a { meta { targets = [\"worktree\"] } call workflow b }",
            "a.wf",
        )
        .map_err(|e| e.to_string()),
        "b" => parse_workflow_str(
            "workflow b { meta { targets = [\"worktree\"] } call workflow a }",
            "b.wf",
        )
        .map_err(|e| e.to_string()),
        other => Err(format!("Unknown workflow: {other}")),
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("Circular workflow reference"));
    assert!(err.contains("a -> b -> a"));
}

#[test]
fn test_detect_workflow_cycles_self_reference() {
    let result = detect_workflow_cycles("a", &|name| match name {
        "a" => parse_workflow_str(
            "workflow a { meta { targets = [\"worktree\"] } call workflow a }",
            "a.wf",
        )
        .map_err(|e| e.to_string()),
        other => Err(format!("Unknown workflow: {other}")),
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("Circular workflow reference"));
    assert!(err.contains("a -> a"));
}

#[test]
fn test_detect_workflow_cycles_transitive() {
    let result = detect_workflow_cycles("a", &|name| match name {
        "a" => parse_workflow_str(
            "workflow a { meta { targets = [\"worktree\"] } call workflow b }",
            "a.wf",
        )
        .map_err(|e| e.to_string()),
        "b" => parse_workflow_str(
            "workflow b { meta { targets = [\"worktree\"] } call workflow c }",
            "b.wf",
        )
        .map_err(|e| e.to_string()),
        "c" => parse_workflow_str(
            "workflow c { meta { targets = [\"worktree\"] } call workflow a }",
            "c.wf",
        )
        .map_err(|e| e.to_string()),
        other => Err(format!("Unknown workflow: {other}")),
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("a -> b -> c -> a"));
}

#[test]
fn test_detect_workflow_cycles_depth_limit() {
    // Build a chain of 6 workflows (exceeds MAX_WORKFLOW_DEPTH of 5)
    let result = detect_workflow_cycles("w0", &|name| {
        let idx: usize = name[1..].parse().unwrap();
        if idx < 6 {
            let next = format!("w{}", idx + 1);
            let src = format!("workflow {name} {{ meta {{ targets = [\"worktree\"] }} call workflow {next} }}");
            parse_workflow_str(&src, &format!("{name}.wf")).map_err(|e| e.to_string())
        } else {
            let src =
                format!("workflow {name} {{ meta {{ targets = [\"worktree\"] }} call agent }}");
            parse_workflow_str(&src, &format!("{name}.wf")).map_err(|e| e.to_string())
        }
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("nesting depth exceeds"));
}

// Ensure the no_loader helper is used at least once to avoid dead_code warnings
#[allow(dead_code)]
fn _use_no_loader() {
    let _ = no_loader("test");
}
