use std::collections::HashSet;
use std::fmt;

use super::types::{Condition, GateType, InputType, ScriptNode, WorkflowDef, WorkflowNode};

// ---------------------------------------------------------------------------
// Semantic validation
// ---------------------------------------------------------------------------

/// A single semantic validation error found during static analysis of a workflow.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub message: String,
    /// Optional hint to help the user fix the error.
    pub hint: Option<String>,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.hint {
            Some(h) => write!(f, "{} (hint: {h})", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

/// The result of running `validate_workflow_semantics`.
#[derive(Debug, Default)]
pub struct ValidationReport {
    pub errors: Vec<ValidationError>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a `WorkflowDef` semantically:
///
/// 1. Forward-pass dataflow analysis: every condition reference (`step.marker`)
///    must name a step key that has been "produced" before that point.
/// 2. Sub-workflow required-input satisfaction: every `required` input declared
///    by a called sub-workflow must be supplied at the call site.
/// 3. Sub-workflow existence: if the loader returns an error the missing workflow
///    is reported as a validation error.
///
/// The `loader` callback receives a workflow name and returns its parsed
/// `WorkflowDef`, allowing this function to be tested without touching the
/// filesystem.
pub fn validate_workflow_semantics<F>(def: &WorkflowDef, loader: &F) -> ValidationReport
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    let mut errors = Vec::new();
    let mut produced: HashSet<String> = HashSet::new();

    // Collect declared boolean input names for condition validation.
    let bool_inputs: HashSet<String> = def
        .inputs
        .iter()
        .filter(|i| i.input_type == InputType::Boolean)
        .map(|i| i.name.clone())
        .collect();

    validate_nodes(&def.body, &mut produced, &mut errors, loader, &bool_inputs);

    // The `always` block sees every step key produced anywhere in the main body.
    let mut always_produced = produced.clone();
    validate_nodes(
        &def.always,
        &mut always_produced,
        &mut errors,
        loader,
        &bool_inputs,
    );

    // Validate target values
    const VALID_TARGETS: &[&str] = &["worktree", "ticket", "repo", "pr", "workflow_run"];
    for target in &def.targets {
        if !VALID_TARGETS.contains(&target.as_str()) {
            errors.push(ValidationError {
                message: format!(
                    "Unknown target '{}' in workflow '{}'. Valid targets: {}",
                    target,
                    def.name,
                    VALID_TARGETS.join(", ")
                ),
                hint: Some(format!(
                    "Change '{}' to one of: {}",
                    target,
                    VALID_TARGETS.join(", ")
                )),
            });
        }
    }

    ValidationReport { errors }
}

fn validate_nodes<F>(
    nodes: &[WorkflowNode],
    produced: &mut HashSet<String>,
    errors: &mut Vec<ValidationError>,
    loader: &F,
    bool_inputs: &HashSet<String>,
) where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                produced.insert(n.agent.step_key());
            }
            WorkflowNode::CallWorkflow(n) => {
                // Check that required inputs are satisfied.
                match loader(&n.workflow) {
                    Ok(sub_def) => {
                        for input_decl in &sub_def.inputs {
                            if input_decl.required && !n.inputs.contains_key(&input_decl.name) {
                                errors.push(ValidationError {
                                    message: format!(
                                        "Sub-workflow '{}' requires input '{}' but it was not provided at the call site",
                                        n.workflow, input_decl.name
                                    ),
                                    hint: None,
                                });
                            }
                        }

                        // Mark inner steps of the sub-workflow as produced,
                        // matching the runtime's bubble-up behavior.
                        for sub_node in &sub_def.body {
                            for key in node_step_keys(sub_node) {
                                produced.insert(key);
                            }
                        }
                    }
                    Err(e) => {
                        errors.push(ValidationError {
                            message: format!(
                                "Sub-workflow '{}' could not be loaded: {}",
                                n.workflow, e
                            ),
                            hint: None,
                        });
                    }
                }
                produced.insert(n.workflow.clone());
            }
            WorkflowNode::Parallel(n) => {
                // Validate `if` condition references before inserting produced keys,
                // since conditions must reference steps produced *before* this block.
                for (step_name, _marker) in n.call_if.values() {
                    check_condition_reachable(step_name, produced, errors);
                }
                for call in &n.calls {
                    produced.insert(call.step_key());
                }
            }
            WorkflowNode::If(n) => {
                validate_conditional_branch(
                    &n.condition,
                    &n.body,
                    produced,
                    errors,
                    loader,
                    bool_inputs,
                );
            }
            WorkflowNode::Unless(n) => {
                validate_conditional_branch(
                    &n.condition,
                    &n.body,
                    produced,
                    errors,
                    loader,
                    bool_inputs,
                );
            }
            WorkflowNode::While(n) => {
                // Condition is checked before the first iteration.
                check_condition_reachable(&n.step, produced, errors);
                let mut body_produced = produced.clone();
                validate_nodes(&n.body, &mut body_produced, errors, loader, bool_inputs);
                produced.extend(body_produced);
            }
            WorkflowNode::DoWhile(n) => {
                // Body always executes at least once before the condition is checked.
                validate_nodes(&n.body, produced, errors, loader, bool_inputs);
                check_condition_reachable(&n.step, produced, errors);
            }
            WorkflowNode::Do(n) => {
                validate_nodes(&n.body, produced, errors, loader, bool_inputs);
            }
            WorkflowNode::Gate(n) => {
                // Quality gates require source and threshold fields.
                if n.gate_type == GateType::QualityGate {
                    if n.source.is_none() {
                        errors.push(ValidationError {
                            message: format!(
                                "Quality gate '{}' is missing required `source` field",
                                n.name
                            ),
                            hint: Some("Add `source = \"step_name\"` to reference the step whose structured output should be evaluated".to_string()),
                        });
                    }
                    if n.threshold.is_none() {
                        errors.push(ValidationError {
                            message: format!(
                                "Quality gate '{}' is missing required `threshold` field",
                                n.name
                            ),
                            hint: Some("Add `threshold = 70` (0-100) to set the minimum confidence score required to pass".to_string()),
                        });
                    }
                }
                // Quality gates reference a prior step's structured output.
                if let Some(ref source) = n.source {
                    if !produced.contains(source) {
                        errors.push(ValidationError {
                            message: format!(
                                "Quality gate '{}' references source step '{}' which has not been produced at this point in the workflow",
                                n.name, source
                            ),
                            hint: Some(format!(
                                "Ensure a call or script step named '{}' appears before this gate",
                                source
                            )),
                        });
                    }
                }
            }
            WorkflowNode::Script(n) => {
                produced.insert(n.name.clone());
            }
            WorkflowNode::Always(n) => {
                // An Always node nested inside a body block sees the current produced set.
                validate_nodes(&n.body, produced, errors, loader, bool_inputs);
            }
        }
    }
}

/// Extract step keys produced by a single workflow node.
///
/// Most nodes produce exactly one key; `Parallel` nodes produce one key per
/// inner call.  Structural nodes (`If`, `While`, etc.) don't directly produce
/// keys at the top level — their inner steps are handled recursively elsewhere.
fn node_step_keys(node: &WorkflowNode) -> Vec<String> {
    match node {
        WorkflowNode::Call(n) => vec![n.agent.step_key()],
        WorkflowNode::CallWorkflow(n) => vec![n.workflow.clone()],
        WorkflowNode::Script(n) => vec![n.name.clone()],
        WorkflowNode::Parallel(n) => n.calls.iter().map(|c| c.step_key()).collect(),
        _ => vec![],
    }
}

/// Emit a validation error if `step` has not yet been produced.
fn check_condition_reachable(
    step: &str,
    produced: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    if !produced.contains(step) {
        errors.push(ValidationError {
            message: format!(
                "Condition references step '{}' which has not been produced at this point in the workflow",
                step
            ),
            hint: None,
        });
    }
}

/// Emit a validation error if `input` is not a declared boolean input.
fn check_bool_input_declared(
    input: &str,
    bool_inputs: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    if !bool_inputs.contains(input) {
        errors.push(ValidationError {
            message: format!(
                "Condition references '{}' which is not a declared boolean input",
                input
            ),
            hint: Some(format!(
                "Declare it in the workflow inputs block: `{} boolean`",
                input
            )),
        });
    }
}

/// Validate a conditional branch (shared logic for `if` and `unless` nodes).
fn validate_conditional_branch<F>(
    condition: &Condition,
    body: &[WorkflowNode],
    produced: &mut HashSet<String>,
    errors: &mut Vec<ValidationError>,
    loader: &F,
    bool_inputs: &HashSet<String>,
) where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    match condition {
        Condition::StepMarker { step, .. } => {
            check_condition_reachable(step, produced, errors);
        }
        Condition::BoolInput { input } => {
            check_bool_input_declared(input, bool_inputs, errors);
        }
    }
    let mut branch_produced = produced.clone();
    validate_nodes(body, &mut branch_produced, errors, loader, bool_inputs);
    // Conservative union: optimistically assume branch steps are available downstream.
    produced.extend(branch_produced);
}

// ---------------------------------------------------------------------------
// Script step validation
// ---------------------------------------------------------------------------

/// Validate all `script` steps in a workflow: check that the `run` target
/// exists and is executable.
///
/// The `path_resolver` callback receives a script path string and returns:
/// - `Ok(PathBuf)` — the resolved, existing path (permissions will be checked)
/// - `Err(String)` — a human-readable "searched paths" string for the error message
///
/// Paths containing `{{` (template variables) are silently skipped because
/// they cannot be resolved statically.
pub fn validate_script_steps<F>(def: &WorkflowDef, path_resolver: &F) -> Vec<ValidationError>
where
    F: Fn(&str) -> Result<std::path::PathBuf, String>,
{
    let mut errors = Vec::new();
    let nodes: Vec<&ScriptNode> = collect_script_nodes(&def.body)
        .into_iter()
        .chain(collect_script_nodes(&def.always))
        .collect();

    for node in nodes {
        let run = &node.run;

        // Skip template-variable paths — can't resolve them statically.
        if run.contains("{{") {
            continue;
        }

        match path_resolver(run) {
            Err(searched) => {
                errors.push(ValidationError {
                    message: format!(
                        "Script step '{}': '{}' not found. Searched: {}",
                        node.name, run, searched
                    ),
                    hint: None,
                });
            }
            Ok(resolved) => {
                #[cfg(unix)]
                if let Some(err) = check_script_unix_permissions(&node.name, &resolved) {
                    errors.push(err);
                }
                #[cfg(not(unix))]
                {
                    let _ = resolved;
                }
            }
        }
    }

    errors
}

/// Check Unix execute permissions for a resolved script path.
///
/// Returns a `ValidationError` if `fs::metadata` fails or the file lacks the
/// execute bit; returns `None` if the file is executable.
#[cfg(unix)]
fn check_script_unix_permissions(
    step_name: &str,
    resolved: &std::path::Path,
) -> Option<ValidationError> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(resolved) {
        Err(e) => Some(ValidationError {
            message: format!(
                "Script step '{}': could not read metadata for '{}': {}",
                step_name,
                resolved.display(),
                e,
            ),
            hint: None,
        }),
        Ok(meta) => {
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 {
                Some(ValidationError {
                    message: format!(
                        "Script step '{}': '{}' is not executable (mode {:04o})",
                        step_name,
                        resolved.display(),
                        mode & 0o777,
                    ),
                    hint: Some(format!("Run: chmod +x {}", resolved.display())),
                })
            } else {
                None
            }
        }
    }
}

/// Recursively collect all `ScriptNode` references from a node list.
fn collect_script_nodes(nodes: &[WorkflowNode]) -> Vec<&ScriptNode> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Script(s) => out.push(s),
            WorkflowNode::If(n) => out.extend(collect_script_nodes(&n.body)),
            WorkflowNode::Unless(n) => out.extend(collect_script_nodes(&n.body)),
            WorkflowNode::While(n) => out.extend(collect_script_nodes(&n.body)),
            WorkflowNode::DoWhile(n) => out.extend(collect_script_nodes(&n.body)),
            WorkflowNode::Do(n) => out.extend(collect_script_nodes(&n.body)),
            WorkflowNode::Always(n) => out.extend(collect_script_nodes(&n.body)),
            WorkflowNode::Call(_)
            | WorkflowNode::CallWorkflow(_)
            | WorkflowNode::Gate(_)
            | WorkflowNode::Parallel(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_without_hint() {
        let err = ValidationError {
            message: "msg".into(),
            hint: None,
        };
        assert_eq!(err.to_string(), "msg");
    }

    #[test]
    fn test_display_with_hint() {
        let err = ValidationError {
            message: "msg".into(),
            hint: Some("fix it".into()),
        };
        assert_eq!(err.to_string(), "msg (hint: fix it)");
    }

    #[cfg(unix)]
    #[test]
    fn test_check_script_unix_permissions_metadata_error() {
        // A path that does not exist causes fs::metadata to fail, exercising the
        // Err(e) branch added in #889.
        let err = check_script_unix_permissions(
            "my-step",
            std::path::Path::new("/nonexistent/path/to/script.sh"),
        );
        assert!(
            err.is_some(),
            "missing path should produce a validation error"
        );
        let msg = &err.unwrap().message;
        assert!(
            msg.contains("could not read metadata"),
            "error should mention metadata failure, got: {msg}"
        );
        assert!(
            msg.contains("my-step"),
            "error should include the step name, got: {msg}"
        );
        assert!(
            msg.contains("/nonexistent/path/to/script.sh"),
            "error should include the path, got: {msg}"
        );
    }
}
