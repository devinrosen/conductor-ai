use super::*;

#[test]
fn test_build_variable_map_includes_inputs_and_prior_context() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    state
        .inputs
        .insert("branch".to_string(), "main".to_string());
    state.contexts.push(ContextEntry {
        step: "step-a".to_string(),
        iteration: 0,
        context: "previous output".to_string(),
        markers: vec![],
        structured_output: None,
        output_file: None,
    });

    let vars = build_variable_map(&state);
    assert_eq!(vars.get("branch").unwrap(), "main");
    assert_eq!(vars.get("prior_context").unwrap(), "previous output");
    assert!(vars.get("prior_contexts").unwrap().contains("step-a"));
}

#[test]
fn test_parallel_contexts_included_in_prior_contexts() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Simulate multiple parallel agents completing and pushing contexts
    // (this is the pattern now used in execute_parallel's success branch)
    state.contexts.push(ContextEntry {
        step: "reviewer-a".to_string(),
        iteration: 0,
        context: "LGTM from reviewer A".to_string(),
        markers: vec![],
        structured_output: None,
        output_file: None,
    });
    state.contexts.push(ContextEntry {
        step: "reviewer-b".to_string(),
        iteration: 0,
        context: "Needs changes from reviewer B".to_string(),
        markers: vec!["has_review_issues".to_string()],
        structured_output: None,
        output_file: None,
    });

    let vars = build_variable_map(&state);

    // prior_context should be the last context pushed
    assert_eq!(
        vars.get("prior_context").unwrap(),
        "Needs changes from reviewer B"
    );

    // prior_contexts should contain both parallel agent entries
    let prior_contexts = vars.get("prior_contexts").unwrap();
    assert!(prior_contexts.contains("reviewer-a"));
    assert!(prior_contexts.contains("reviewer-b"));
    assert!(prior_contexts.contains("LGTM from reviewer A"));
    assert!(prior_contexts.contains("Needs changes from reviewer B"));
}

#[test]
fn test_build_variable_map_includes_gate_feedback() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    state.last_gate_feedback = Some("looks good".to_string());

    let vars = build_variable_map(&state);
    assert_eq!(vars.get("gate_feedback").unwrap(), "looks good");
}

#[test]
fn test_build_variable_map_no_gate_feedback() {
    let conn = setup_db();
    let state = make_test_state(&conn);
    let vars = build_variable_map(&state);
    assert!(!vars.contains_key("gate_feedback"));
    // prior_context should be empty string when no contexts
    assert_eq!(vars.get("prior_context").unwrap(), "");
    // prior_output should be absent when no structured output
    assert!(!vars.contains_key("prior_output"));
}

#[test]
fn test_build_variable_map_includes_prior_output() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    let json = r#"{"approved":true,"summary":"All clear"}"#.to_string();
    state.contexts.push(crate::workflow::types::ContextEntry {
        step: "test_step".to_string(),
        iteration: 0,
        context: String::new(),
        markers: Vec::new(),
        structured_output: Some(json.clone()),
        output_file: None,
    });

    let vars = build_variable_map(&state);
    assert_eq!(vars.get("prior_output").unwrap(), &json);
}

#[test]
fn test_build_variable_map_includes_dry_run() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    // Default exec_config has dry_run = false
    let vars = build_variable_map(&state);
    assert_eq!(vars.get("dry_run").unwrap(), "false");

    // Set dry_run = true
    state.exec_config.dry_run = true;
    let vars = build_variable_map(&state);
    assert_eq!(vars.get("dry_run").unwrap(), "true");
}

#[test]
fn test_resolve_child_inputs_substitutes_variables() {
    use crate::workflow_dsl::InputDecl;

    let mut raw = HashMap::new();
    raw.insert("msg".to_string(), "Hello {{name}}!".to_string());

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("name", "World".to_string());

    let decls = vec![InputDecl {
        name: "msg".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("msg").unwrap(), "Hello World!");
}

#[test]
fn test_resolve_child_inputs_preserves_unresolved_braces() {
    // Regression test for #1907: when a substituted value itself contains
    // {{…}} text (e.g. JSON with template-like keys), those patterns must
    // not be stripped from the child input.
    use crate::workflow_dsl::InputDecl;

    let json_value =
        r#"{"risks":["{{deterministic-review.score}}","other"]}"#.to_string();

    let mut raw = HashMap::new();
    raw.insert("prior_output".to_string(), "{{prior_output}}".to_string());

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("prior_output", json_value.clone());

    let decls = vec![InputDecl {
        name: "prior_output".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    // The embedded {{…}} inside the JSON value must survive intact.
    assert_eq!(result.get("prior_output").unwrap(), &json_value);
}

#[test]
fn test_resolve_child_inputs_applies_defaults() {
    use crate::workflow_dsl::InputDecl;

    let raw = HashMap::new(); // no inputs provided

    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "mode".to_string(),
        required: false,
        default: Some("fast".to_string()),
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("mode").unwrap(), "fast");
}

#[test]
fn test_resolve_child_inputs_missing_required() {
    use crate::workflow_dsl::InputDecl;

    let raw = HashMap::new();
    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "pr_url".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let err = resolve_child_inputs(&raw, &vars, &decls).unwrap_err();
    assert_eq!(err, "pr_url");
}

#[test]
fn test_resolve_child_inputs_provided_overrides_default() {
    use crate::workflow_dsl::InputDecl;

    let mut raw = HashMap::new();
    raw.insert("mode".to_string(), "slow".to_string());

    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "mode".to_string(),
        required: false,
        default: Some("fast".to_string()),
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("mode").unwrap(), "slow");
}

#[test]
fn test_resolve_child_inputs_optional_without_default_omitted() {
    use crate::workflow_dsl::InputDecl;

    let raw = HashMap::new();
    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "optional_field".to_string(),
        required: false,
        default: None,
        description: None,
        input_type: Default::default(),
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert!(!result.contains_key("optional_field"));
}

#[test]
fn test_resolve_child_inputs_boolean_defaults_to_false() {
    use crate::workflow_dsl::{InputDecl, InputType};

    let raw = HashMap::new(); // boolean input not explicitly passed
    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "flag".to_string(),
        required: false,
        default: None,
        description: None,
        input_type: InputType::Boolean,
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("flag").map(|s| s.as_str()), Some("false"));
}

#[test]
fn test_resolve_child_inputs_boolean_provided_value_not_overwritten() {
    use crate::workflow_dsl::{InputDecl, InputType};

    let mut raw = HashMap::new();
    raw.insert("flag".to_string(), "true".to_string());

    let vars: HashMap<&str, String> = HashMap::new();
    let decls = vec![InputDecl {
        name: "flag".to_string(),
        required: false,
        default: None,
        description: None,
        input_type: InputType::Boolean,
    }];

    let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
    assert_eq!(result.get("flag").map(|s| s.as_str()), Some("true"));
}

#[test]
fn test_apply_workflow_input_defaults_fills_missing_default() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "skip_tests".to_string(),
        required: false,
        default: Some("false".to_string()),
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("false"));
}

#[test]
fn test_apply_workflow_input_defaults_does_not_overwrite_provided_value() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "skip_tests".to_string(),
        required: false,
        default: Some("false".to_string()),
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    inputs.insert("skip_tests".to_string(), "true".to_string());
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    // Provided value must not be replaced by the default.
    assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("true"));
}

#[test]
fn test_apply_workflow_input_defaults_errors_on_missing_required() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "ticket_id".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    let result = apply_workflow_input_defaults(&workflow, &mut inputs);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("ticket_id"),
        "error message should name the missing input, got: {msg}"
    );
}

#[test]
fn test_apply_workflow_input_defaults_required_input_provided_succeeds() {
    use crate::workflow_dsl::InputDecl;

    let workflow = make_workflow_def_with_inputs(vec![InputDecl {
        name: "ticket_id".to_string(),
        required: true,
        default: None,
        description: None,
        input_type: Default::default(),
    }]);

    let mut inputs = HashMap::new();
    inputs.insert("ticket_id".to_string(), "TKT-1".to_string());
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    assert_eq!(inputs.get("ticket_id").map(String::as_str), Some("TKT-1"));
}

#[test]
fn test_apply_workflow_input_defaults_no_inputs_is_noop() {
    let workflow = make_workflow_def_with_inputs(vec![]);
    let mut inputs = HashMap::new();
    apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
    assert!(inputs.is_empty());
}
