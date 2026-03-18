use std::collections::{HashMap, HashSet};

use crate::agent_config::{self, AgentSpec};
use crate::prompt_config;
use crate::schema_config;

use super::api::detect_workflow_cycles;
use super::script_utils::{default_skills_dir, make_script_resolver};
use super::types::{
    collect_agent_names, AgentRef, Condition, InputType, ScriptNode, WorkflowDef, WorkflowNode,
};

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
            WorkflowNode::Gate(_) => {}
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
            hint: Some(
                "Note: inner steps of called sub-workflows are not available in this context. \
                 Use the sub-workflow's own name (the key produced by `call workflow`) as the condition step."
                    .to_string(),
            ),
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

// ---------------------------------------------------------------------------
// Batch workflow validation (orchestration)
// ---------------------------------------------------------------------------

/// Result of validating a single workflow.
#[derive(Debug)]
pub struct WorkflowValidationEntry {
    /// Workflow name.
    pub name: String,
    /// Blocking errors found during validation.
    pub errors: Vec<String>,
    /// Non-blocking warnings (e.g. unknown bot names).
    pub warnings: Vec<String>,
}

/// Result of validating a batch of workflows.
#[derive(Debug)]
pub struct BatchValidationResult {
    /// Per-workflow results (in order).
    pub entries: Vec<WorkflowValidationEntry>,
    /// Parse errors for files that could not be loaded at all.
    pub parse_errors: Vec<String>,
}

impl BatchValidationResult {
    /// Total number of workflows (including parse failures).
    pub fn total(&self) -> usize {
        self.entries.len() + self.parse_errors.len()
    }

    /// Number of workflows that failed validation.
    pub fn failed_count(&self) -> usize {
        let entry_failures = self.entries.iter().filter(|e| !e.errors.is_empty()).count();
        entry_failures + self.parse_errors.len()
    }
}

/// Validate a batch of workflows against agents, snippets, schemas, cycles,
/// semantic rules, script steps, and bot-name configuration.
///
/// This function encapsulates all the orchestration logic for the `workflow validate`
/// command: deduplication, global caching, per-workflow refinement, and result
/// collection.
///
/// The `loader` callback loads a workflow definition by name (used for cycle
/// detection and semantic sub-workflow validation). The `known_bots` set
/// contains bot names from `[github.apps]` config — any bot referenced in a
/// workflow but absent from this set produces a warning.
pub fn validate_workflows_batch<F>(
    workflows: &[WorkflowDef],
    parse_errors: &[String],
    wt_path: &str,
    repo_path: &str,
    known_bots: &HashSet<String>,
    loader: &F,
) -> BatchValidationResult
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    let script_resolver = make_script_resolver(
        wt_path.to_string(),
        repo_path.to_string(),
        default_skills_dir(),
    );

    let format_hint_error = |msg: &str, hint: &Option<String>| -> String {
        match hint {
            Some(h) => format!("{msg} (hint: {h})"),
            None => msg.to_string(),
        }
    };

    // Pre-collect unique refs across all workflows and batch-check
    // against global directories once, avoiding O(N) redundant
    // filesystem probes when multiple workflows share agents,
    // snippets, or schemas.
    let mut all_agent_refs: Vec<AgentRef> = Vec::new();
    let mut all_snippet_names: Vec<String> = Vec::new();
    let mut all_schema_names: Vec<String> = Vec::new();
    for wf in workflows {
        all_agent_refs.extend(collect_agent_names(&wf.body));
        all_agent_refs.extend(collect_agent_names(&wf.always));
        all_snippet_names.extend(wf.collect_all_snippet_refs());
        all_schema_names.extend(wf.collect_all_schema_refs());
    }
    all_agent_refs.sort();
    all_agent_refs.dedup();
    all_snippet_names.sort();
    all_snippet_names.dedup();
    all_schema_names.sort();
    all_schema_names.dedup();

    let global_agent_specs: Vec<AgentSpec> = all_agent_refs.iter().map(AgentSpec::from).collect();
    let globally_missing_agents: HashSet<String> =
        agent_config::find_missing_agents(wt_path, repo_path, &global_agent_specs, None)
            .into_iter()
            .collect();
    let globally_missing_snippets: HashSet<String> =
        prompt_config::find_missing_snippets(wt_path, repo_path, &all_snippet_names, None)
            .into_iter()
            .collect();
    let global_schema_issues: Vec<schema_config::SchemaIssue> =
        schema_config::check_schemas(wt_path, repo_path, &all_schema_names, None);
    let global_schema_issue_map: HashMap<String, &schema_config::SchemaIssue> =
        global_schema_issues
            .iter()
            .map(|i| {
                let name = match i {
                    schema_config::SchemaIssue::Missing(n) => n.clone(),
                    schema_config::SchemaIssue::Invalid { name, .. } => name.clone(),
                };
                (name, i)
            })
            .collect();

    let mut entries = Vec::new();
    for workflow in workflows {
        let wf_name = &workflow.name;
        let mut wf_errors: Vec<String> = Vec::new();

        // --- Agents: emit errors directly from pre-computed missing set ---
        let mut agent_refs = collect_agent_names(&workflow.body);
        agent_refs.extend(collect_agent_names(&workflow.always));
        agent_refs.sort();
        agent_refs.dedup();
        for r in &agent_refs {
            if globally_missing_agents.contains(r.label()) {
                wf_errors.push(format!("missing agent: {}", r.label()));
            }
        }

        // --- Snippets: emit errors directly from pre-computed missing set ---
        for snippet in workflow.collect_all_snippet_refs() {
            if globally_missing_snippets.contains(&snippet) {
                wf_errors.push(format!("missing prompt snippet: {snippet}"));
            }
        }

        // --- Schemas: emit errors directly from pre-computed issue map ---
        for schema in workflow.collect_all_schema_refs() {
            if let Some(issue) = global_schema_issue_map.get(&schema) {
                match issue {
                    schema_config::SchemaIssue::Missing(s) => {
                        wf_errors.push(format!("missing schema: {s}"));
                    }
                    schema_config::SchemaIssue::Invalid { name: s, error } => {
                        wf_errors.push(format!("invalid schema: {s} — {error}"));
                    }
                }
            }
        }

        // --- Bot names (warnings) ---
        let all_bots = workflow.collect_all_bot_names();
        let unknown_bots: Vec<String> = all_bots
            .into_iter()
            .filter(|b| !known_bots.contains(b.as_str()))
            .collect();

        // --- Cycle detection ---
        if let Err(cycle_msg) = detect_workflow_cycles(wf_name, loader) {
            wf_errors.push(format!("cycle detected: {cycle_msg}"));
        }

        // --- Semantic validation ---
        let report = validate_workflow_semantics(workflow, loader);
        for err in &report.errors {
            wf_errors.push(format_hint_error(&err.message, &err.hint));
        }

        // --- Script step validation ---
        let script_errors = validate_script_steps(workflow, &script_resolver);
        for err in &script_errors {
            wf_errors.push(format_hint_error(&err.message, &err.hint));
        }

        let warnings: Vec<String> = unknown_bots
            .iter()
            .map(|b| format!("unknown bot name '{b}' (not in [github.apps])"))
            .collect();

        entries.push(WorkflowValidationEntry {
            name: wf_name.clone(),
            errors: wf_errors,
            warnings,
        });
    }

    BatchValidationResult {
        entries,
        parse_errors: parse_errors.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // -----------------------------------------------------------------------
    // validate_workflows_batch unit tests
    // -----------------------------------------------------------------------

    /// Build a minimal WorkflowDef by parsing a .wf string.
    fn parse_wf(src: &str) -> WorkflowDef {
        super::super::parse_workflow_str(src, "<test>").expect("test workflow should parse")
    }

    /// A loader that always fails — useful when we don't care about sub-workflows.
    fn failing_loader(name: &str) -> std::result::Result<WorkflowDef, String> {
        Err(format!("workflow '{name}' not found"))
    }

    #[test]
    fn batch_missing_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        // Workflow references a prompt snippet that doesn't exist on disk.
        let wf = parse_wf(
            r#"workflow snip-test {
                meta { trigger = "manual" targets = ["worktree"] }
                call some-agent { with = ["nonexistent-snippet"] }
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        let errs = &result.entries[0].errors;
        assert!(
            errs.iter().any(|e| e.contains("missing prompt snippet")),
            "expected missing snippet error, got: {errs:?}"
        );
    }

    #[test]
    fn batch_missing_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let wf = parse_wf(
            r#"workflow schema-test {
                meta { trigger = "manual" targets = ["worktree"] }
                call some-agent { output = "nonexistent-schema" }
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        let errs = &result.entries[0].errors;
        assert!(
            errs.iter().any(|e| e.contains("missing schema")),
            "expected missing schema error, got: {errs:?}"
        );
    }

    #[test]
    fn batch_invalid_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        // Create a schema file with invalid YAML content.
        let schema_dir = dir.path().join(".conductor").join("schemas");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(
            schema_dir.join("bad-schema.yaml"),
            "not: valid: schema: {{{",
        )
        .unwrap();

        let wf = parse_wf(
            r#"workflow schema-test {
                meta { trigger = "manual" targets = ["worktree"] }
                call some-agent { output = "bad-schema" }
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        let errs = &result.entries[0].errors;
        assert!(
            errs.iter().any(|e| e.contains("invalid schema")),
            "expected invalid schema error, got: {errs:?}"
        );
    }

    #[test]
    fn batch_unknown_bot_name_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let wf = parse_wf(
            r#"workflow bot-test {
                meta { trigger = "manual" targets = ["worktree"] }
                call some-agent { as = "unknown-bot" }
            }"#,
        );

        // known_bots does NOT contain "unknown-bot".
        let known_bots = HashSet::from(["real-bot".to_string()]);
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        // Should be a warning, not an error.
        let warnings = &result.entries[0].warnings;
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("unknown bot name") && w.contains("unknown-bot")),
            "expected unknown bot warning, got: {warnings:?}"
        );
    }

    #[test]
    fn batch_self_referencing_workflow_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let wf = parse_wf(
            r#"workflow cyclic {
                meta { trigger = "manual" targets = ["worktree"] }
                call workflow cyclic {}
            }"#,
        );

        // Loader that returns the same workflow for "cyclic", creating a cycle.
        let wf_clone = wf.clone();
        let loader = move |name: &str| -> std::result::Result<WorkflowDef, String> {
            if name == "cyclic" {
                Ok(wf_clone.clone())
            } else {
                Err(format!("not found: {name}"))
            }
        };

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &loader);

        assert_eq!(result.entries.len(), 1);
        let errs = &result.entries[0].errors;
        assert!(
            errs.iter().any(|e| e.contains("cycle")),
            "expected cycle detection error, got: {errs:?}"
        );
    }
}
