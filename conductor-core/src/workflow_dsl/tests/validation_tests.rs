use crate::workflow_dsl::*;

fn no_loader(name: &str) -> std::result::Result<WorkflowDef, String> {
    Err(format!("no loader: {name}"))
}

fn make_script_def(run: &str) -> WorkflowDef {
    WorkflowDef {
        name: "test-wf".to_string(),
        description: String::new(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        inputs: vec![],
        body: vec![WorkflowNode::Script(ScriptNode {
            name: "my-step".to_string(),
            run: run.to_string(),
            env: std::collections::HashMap::new(),
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        })],
        always: vec![],
        source_path: "test.wf".to_string(),
    }
}

fn make_always_script_def(run: &str) -> WorkflowDef {
    WorkflowDef {
        name: "test-wf".to_string(),
        description: String::new(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        inputs: vec![],
        body: vec![],
        always: vec![WorkflowNode::Script(ScriptNode {
            name: "always-step".to_string(),
            run: run.to_string(),
            env: std::collections::HashMap::new(),
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        })],
        source_path: "test.wf".to_string(),
    }
}

fn parse_workflow_with_if(condition_str: &str) -> crate::error::Result<WorkflowDef> {
    let src = format!(
        r#"
workflow test {{
  meta {{ trigger = "manual" targets = ["worktree"] }}
  if {condition_str} {{
    call do_something
  }}
}}
"#
    );
    parse_workflow_str(&src, "test.wf")
}

fn parse_workflow_with_unless(condition_str: &str) -> crate::error::Result<WorkflowDef> {
    let src = format!(
        r#"
workflow test {{
  meta {{ trigger = "manual" targets = ["worktree"] }}
  unless {condition_str} {{
    call do_something
  }}
}}
"#
    );
    parse_workflow_str(&src, "test.wf")
}

#[test]
fn test_semantics_valid_simple() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan
    call implement
    while plan.has_issues {
        max_iterations = 3
        call fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "Expected no errors, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_semantics_condition_unreachable() {
    // `review-aggregator` was never produced — only `review-pr` was
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call workflow review-pr
    if review-aggregator.has_review_issues {
        call fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &|name| {
        if name == "review-pr" {
            parse_workflow_str(
                "workflow review-pr { meta { description = \"r\" trigger = \"manual\" targets = [\"worktree\"] } call review-aggregator }",
                "review-pr.wf",
            )
            .map_err(|e| e.to_string())
        } else {
            Err(format!("unknown: {name}"))
        }
    });
    assert!(!report.is_ok());
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].message.contains("review-aggregator"));
    assert!(report.errors[0].hint.is_some());
}

#[test]
fn test_semantics_condition_ok_from_do_while() {
    // check is produced inside do-while body; condition references it after body runs
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    do {
        max_iterations = 3
        call check
        call fix
    } while check.needs_retry
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "Expected no errors, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_semantics_condition_inner_step_hint() {
    // The step referenced in the condition is an inner step of a sub-workflow —
    // the error must mention the step name and include a hint.
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow review-pr
    while review-aggregator.has_review_issues {
        max_iterations = 3
        call fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &|name| {
        if name == "review-pr" {
            parse_workflow_str(
                "workflow review-pr { meta { description = \"r\" trigger = \"manual\" targets = [\"worktree\"] } call review-aggregator }",
                "review-pr.wf",
            )
            .map_err(|e| e.to_string())
        } else {
            Err(format!("unknown: {name}"))
        }
    });
    assert!(!report.is_ok());
    let err = &report.errors[0];
    assert!(err.message.contains("review-aggregator"));
    assert!(err.hint.is_some());
    let hint = err.hint.as_ref().unwrap();
    assert!(hint.contains("sub-workflow"));
}

#[test]
fn test_semantics_missing_required_input() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow child
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &|name| {
        if name == "child" {
            parse_workflow_str(
                r#"workflow child {
                    meta { description = "c" trigger = "manual" targets = ["worktree"] }
                    inputs { ticket_id required }
                    call do-work
                }"#,
                "child.wf",
            )
            .map_err(|e| e.to_string())
        } else {
            Err(format!("unknown: {name}"))
        }
    });
    assert!(!report.is_ok());
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].message.contains("ticket_id"));
    assert!(report.errors[0].message.contains("child"));
}

#[test]
fn test_semantics_provided_required_input_ok() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow child {
        inputs { ticket_id = "{{ticket_id}}" }
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &|name| {
        if name == "child" {
            parse_workflow_str(
                r#"workflow child {
                    meta { description = "c" trigger = "manual" targets = ["worktree"] }
                    inputs { ticket_id required }
                    call do-work
                }"#,
                "child.wf",
            )
            .map_err(|e| e.to_string())
        } else {
            Err(format!("unknown: {name}"))
        }
    });
    assert!(
        report.is_ok(),
        "Expected no errors, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_semantics_sub_workflow_not_found() {
    let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow missing-workflow
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(!report.is_ok());
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].message.contains("missing-workflow"));
}

#[test]
fn test_semantics_always_block_sees_full_produced() {
    // `plan` and `implement` are produced in the body; `always` can reference `plan`
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan
    call implement
    always {
        if plan.has_issues {
            call notify
        }
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "Expected no errors, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_semantics_parallel_produces_step_keys() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    parallel {
        call reviewer-security
        call reviewer-style
    }
    if reviewer-security.has_issues {
        call fix
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "Expected no errors, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_validate_known_targets_accepted() {
    for target in &["worktree", "ticket", "repo", "pr", "workflow_run"] {
        let input = format!("workflow test {{ meta {{ targets = [\"{target}\"] }} call step }}",);
        let def = parse_workflow_str(&input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            report.is_ok(),
            "target '{target}' should be valid, got errors: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_validate_unknown_target_rejected() {
    let input = r#"workflow test { meta { targets = ["foobar"] } call step }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        !report.is_ok(),
        "unknown target 'foobar' should produce a validation error"
    );
    let msg = &report.errors[0].message;
    assert!(
        msg.contains("foobar"),
        "error should mention the bad target, got: {msg}"
    );
}

#[test]
fn test_validate_multiple_targets_with_one_unknown() {
    let input = r#"workflow test { meta { targets = ["worktree", "badtarget"] } call step }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        !report.is_ok(),
        "mixed valid/invalid targets should produce a validation error"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("badtarget")),
        "error should name the unknown target"
    );
}

#[test]
fn test_parallel_if_validation_step_not_produced() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        parallel {
            call review-db-migrations { if = "detect-db-migrations.has_db_migrations" }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let no_loader = |_: &str| Err("not found".to_string());
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        !report.is_ok(),
        "Expected validation error for unreachable step"
    );
    assert!(
        report.errors[0].message.contains("detect-db-migrations"),
        "Expected error mentioning detect-db-migrations, got: {}",
        report.errors[0].message
    );
}

#[test]
fn test_parallel_if_validation_step_produced() {
    let input = r#"workflow test {
        meta { targets = ["worktree"] }
        call detect-db-migrations
        parallel {
            call review-db-migrations { if = "detect-db-migrations.has_db_migrations" }
        }
    }"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let no_loader = |_: &str| Err("not found".to_string());
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "Expected no validation errors, got: {:?}",
        report.errors
    );
}

#[test]
fn test_validate_script_steps_run_not_found() {
    let def = make_script_def("nonexistent-script.sh");
    let resolver = make_script_resolver("/tmp/wt".to_string(), "/tmp/repo".to_string(), None);
    let errors = validate_script_steps(&def, &resolver);
    assert_eq!(errors.len(), 1, "expected one error, got: {:?}", errors);
    let msg = &errors[0].message;
    assert!(
        msg.contains("nonexistent-script.sh"),
        "error should mention the script path, got: {msg}"
    );
    assert!(
        msg.contains("not found"),
        "error should say 'not found', got: {msg}"
    );
    assert!(
        msg.contains("/tmp/wt"),
        "error should list working_dir search path, got: {msg}"
    );
    assert!(
        msg.contains("/tmp/repo"),
        "error should list repo_path search path, got: {msg}"
    );
}

#[test]
fn test_validate_script_steps_skips_variable_paths() {
    let def = make_script_def("{{script_path}}");
    let errors = validate_script_steps(&def, &|_| Err("not found".to_string()));
    assert!(
        errors.is_empty(),
        "template-variable paths should be skipped, got: {:?}",
        errors
    );
}

#[test]
fn test_validate_script_steps_run_valid() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tmpdir");
    let script = dir.path().join("my-script.sh");
    let mut f = std::fs::File::create(&script).unwrap();
    f.write_all(b"#!/bin/sh\n").unwrap();
    let mut perms = f.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let def = make_script_def("my-script.sh");
    let wt_str = dir.path().to_str().unwrap().to_string();
    let resolver = make_script_resolver(wt_str.clone(), "/tmp/no-repo".to_string(), None);
    let errors = validate_script_steps(&def, &resolver);
    assert!(
        errors.is_empty(),
        "valid executable script should produce no errors, got: {:?}",
        errors
    );
}

#[cfg(unix)]
#[test]
fn test_validate_script_steps_run_not_executable() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tmpdir");
    let script = dir.path().join("noexec.sh");
    let mut f = std::fs::File::create(&script).unwrap();
    f.write_all(b"#!/bin/sh\n").unwrap();
    let mut perms = f.metadata().unwrap().permissions();
    perms.set_mode(0o644); // no execute bit
    std::fs::set_permissions(&script, perms).unwrap();

    let def = make_script_def("noexec.sh");
    let wt_str = dir.path().to_str().unwrap().to_string();
    let resolver = make_script_resolver(wt_str.clone(), "/tmp/no-repo".to_string(), None);
    let errors = validate_script_steps(&def, &resolver);
    assert_eq!(
        errors.len(),
        1,
        "non-executable script should produce one error, got: {:?}",
        errors
    );
    let msg = &errors[0].message;
    assert!(
        msg.contains("not executable"),
        "error should mention not executable, got: {msg}"
    );
    assert!(errors[0].hint.is_some(), "should include a chmod hint");
}

// test_check_script_unix_permissions_metadata_error is kept inline in validation.rs
// because it accesses the private check_script_unix_permissions function directly.

#[test]
fn test_validate_script_steps_nested_in_if_block() {
    use std::collections::HashMap;

    let def = WorkflowDef {
        name: "test-wf".to_string(),
        description: String::new(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        inputs: vec![],
        body: vec![WorkflowNode::If(IfNode {
            condition: Condition::StepMarker {
                step: "some-step".to_string(),
                marker: "done".to_string(),
            },
            body: vec![WorkflowNode::Script(ScriptNode {
                name: "nested-script".to_string(),
                run: "deeply-nested.sh".to_string(),
                env: HashMap::new(),
                timeout: None,
                retries: 0,
                on_fail: None,
                bot_name: None,
            })],
        })],
        always: vec![],
        source_path: "test.wf".to_string(),
    };
    let errors = validate_script_steps(&def, &|_| Err("not found".to_string()));
    assert_eq!(errors.len(), 1, "nested script error should be propagated");
    assert!(
        errors[0].message.contains("nested-script"),
        "error should name the nested step"
    );
}

#[test]
fn test_validate_script_steps_in_always_block() {
    let def = make_always_script_def("always-step.sh");
    let resolver = make_script_resolver("/tmp/wt".to_string(), "/tmp/repo".to_string(), None);
    let errors = validate_script_steps(&def, &resolver);
    assert_eq!(
        errors.len(),
        1,
        "expected one error for always-block script, got: {:?}",
        errors
    );
    let msg = &errors[0].message;
    assert!(
        msg.contains("always-step.sh"),
        "error should mention the script path, got: {msg}"
    );
    assert!(
        msg.contains("not found"),
        "error should say 'not found', got: {msg}"
    );
    assert!(
        msg.contains("/tmp/wt"),
        "error should list working_dir search path for relative paths, got: {msg}"
    );
}

#[test]
fn test_validate_script_steps_absolute_path_not_found() {
    let def = make_script_def("/nonexistent/absolute/path.sh");
    let resolver = make_script_resolver("/tmp/wt".to_string(), "/tmp/repo".to_string(), None);
    let errors = validate_script_steps(&def, &resolver);
    assert_eq!(
        errors.len(),
        1,
        "expected one error for absolute path, got: {:?}",
        errors
    );
    let msg = &errors[0].message;
    assert!(
        msg.contains("/nonexistent/absolute/path.sh"),
        "error should mention the absolute path, got: {msg}"
    );
    assert!(
        msg.contains("not found"),
        "error should say 'not found', got: {msg}"
    );
    assert!(
        !msg.contains("/tmp/wt"),
        "error should not list working_dir for absolute paths, got: {msg}"
    );
    assert!(
        !msg.contains("/tmp/repo"),
        "error should not list repo_path for absolute paths, got: {msg}"
    );
}

#[test]
fn test_parse_condition_bare_identifier_is_bool_input() {
    let def = parse_workflow_with_if("my_flag").unwrap();
    let WorkflowNode::If(if_node) = &def.body[0] else {
        panic!("expected If node");
    };
    let Condition::BoolInput { input } = &if_node.condition else {
        panic!("expected BoolInput condition, got {:?}", if_node.condition);
    };
    assert_eq!(input, "my_flag");
}

#[test]
fn test_parse_condition_dot_notation_is_step_marker() {
    let def = parse_workflow_with_if("build.success").unwrap();
    let WorkflowNode::If(if_node) = &def.body[0] else {
        panic!("expected If node");
    };
    let Condition::StepMarker { step, marker } = &if_node.condition else {
        panic!("expected StepMarker condition, got {:?}", if_node.condition);
    };
    assert_eq!(step, "build");
    assert_eq!(marker, "success");
}

#[test]
fn test_parse_unless_bare_identifier_is_bool_input() {
    let def = parse_workflow_with_unless("skip_tests").unwrap();
    let WorkflowNode::Unless(unless_node) = &def.body[0] else {
        panic!("expected Unless node");
    };
    let Condition::BoolInput { input } = &unless_node.condition else {
        panic!(
            "expected BoolInput condition, got {:?}",
            unless_node.condition
        );
    };
    assert_eq!(input, "skip_tests");
}

#[test]
fn test_semantics_bool_input_condition_undeclared_rejected() {
    // `my_flag` is used as a condition but not declared as a boolean input.
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    if my_flag {
        call do_something
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        !report.is_ok(),
        "undeclared boolean input should produce a validation error"
    );
    assert_eq!(report.errors.len(), 1);
    assert!(
        report.errors[0].message.contains("my_flag"),
        "error should mention the undeclared input, got: {}",
        report.errors[0].message
    );
    assert!(report.errors[0].hint.is_some());
}

#[test]
fn test_semantics_bool_input_condition_declared_ok() {
    // `my_flag` is declared as a boolean input — should pass validation.
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    inputs {
        my_flag boolean
    }
    if my_flag {
        call do_something
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "declared boolean input should pass validation, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_semantics_unless_bool_input_undeclared_rejected() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    unless skip_lint {
        call run_lint
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(!report.is_ok());
    assert!(report.errors[0].message.contains("skip_lint"));
}

#[test]
fn test_semantics_unless_bool_input_declared_ok() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    inputs {
        skip_lint boolean
    }
    unless skip_lint {
        call run_lint
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        report.is_ok(),
        "declared boolean input in unless should pass, got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_semantics_string_input_not_usable_as_bool_condition() {
    // A string input by the same name should NOT satisfy a BoolInput condition.
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    inputs {
        my_flag string
    }
    if my_flag {
        call do_something
    }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    let report = validate_workflow_semantics(&def, &no_loader);
    assert!(
        !report.is_ok(),
        "string input should not satisfy a boolean condition"
    );
    assert!(report.errors[0].message.contains("my_flag"));
}

#[test]
fn test_parse_boolean_input_declaration() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    inputs {
        dry_run boolean
        label
    }
    call step
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    assert_eq!(def.inputs.len(), 2);
    let dry_run = def.inputs.iter().find(|i| i.name == "dry_run").unwrap();
    assert_eq!(dry_run.input_type, InputType::Boolean);
    let label = def.inputs.iter().find(|i| i.name == "label").unwrap();
    assert_eq!(label.input_type, InputType::String);
}

#[test]
fn test_parse_while_rejects_bare_identifier() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    while my_flag {
        max_iterations = 3
        call step
    }
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err(), "while with bare identifier should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("my_flag") || err.contains("step.marker"),
        "error should mention the bare identifier or step.marker requirement, got: {err}"
    );
}

#[test]
fn test_parse_do_while_rejects_bare_identifier() {
    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    do {
        max_iterations = 3
        call step
    } while my_flag
}
"#;
    let result = parse_workflow_str(input, "test.wf");
    assert!(result.is_err(), "do-while with bare identifier should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("my_flag") || err.contains("step.marker"),
        "error should mention the bare identifier or step.marker requirement, got: {err}"
    );
}

// resolve_script_path and script_search_paths tests live in
// conductor-core/src/workflow_dsl/script_utils.rs
