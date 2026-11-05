use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::agent_config::{self, AgentSpec};
use crate::prompt_config;
use crate::schema_config;
use runkon_flow::dsl::{
    default_skills_dir, detect_workflow_cycles, make_script_resolver, validate_script_steps,
    validate_workflow_semantics, AgentRef, ScriptNode, ValidationContext, ValidationError,
    WorkflowDef, WorkflowNode,
};

/// Suffix shared by the legacy `<<<CONDUCTOR_OUTPUT>>>` and
/// `<<<END_CONDUCTOR_OUTPUT>>>` markers, renamed to `FLOW_OUTPUT` in #2726.
/// Since runkon-flow 0.6.2-alpha a script that prints them fails its step.
const LEGACY_OUTPUT_MARKER: &str = "CONDUCTOR_OUTPUT>>>";

fn collect_script_nodes<'a>(nodes: &'a [WorkflowNode], out: &mut Vec<&'a ScriptNode>) {
    for node in nodes {
        match node {
            WorkflowNode::Script(s) => out.push(s),
            other => {
                if let Some(body) = other.body() {
                    collect_script_nodes(body, out);
                }
            }
        }
    }
}

/// Report script steps whose script file still prints the legacy
/// `<<<CONDUCTOR_OUTPUT>>>` block (#3042). Scripts that can't be resolved or
/// read are skipped: `validate_script_steps` already reports missing ones.
fn check_legacy_output_markers<F>(workflow: &WorkflowDef, resolver: &F) -> Vec<ValidationError>
where
    F: Fn(&str) -> std::result::Result<std::path::PathBuf, String>,
{
    let mut nodes = Vec::new();
    collect_script_nodes(&workflow.body, &mut nodes);
    collect_script_nodes(&workflow.always, &mut nodes);

    nodes
        .into_iter()
        .filter(|node| !node.run.contains("{{"))
        .filter_map(|node| {
            let path = resolver(&node.run).ok()?;
            let contents = std::fs::read_to_string(&path).ok()?;
            contents
                .contains(LEGACY_OUTPUT_MARKER)
                .then(|| ValidationError {
                    message: format!(
                        "Script step '{}': {} prints the legacy <<<CONDUCTOR_OUTPUT>>> block, \
                         which is no longer parsed; the step will fail at runtime",
                        node.name,
                        path.display()
                    ),
                    hint: Some(
                        "rename <<<CONDUCTOR_OUTPUT>>> to <<<FLOW_OUTPUT>>> and \
                         <<<END_CONDUCTOR_OUTPUT>>> to <<<END_FLOW_OUTPUT>>>"
                            .to_string(),
                    ),
                })
        })
        .collect()
}

const CONDUCTOR_TARGETS: &[&str] = &["worktree", "ticket", "repo", "pr", "workflow_run"];

/// Variables the engine itself sets for every run. Mirrors runkon-flow's
/// `prompt_builder::build_variable_map`; update when that list changes.
const ENGINE_BUILTIN_VARIABLES: &[&str] = &[
    "workflow_run_id",
    "prior_context",
    "prior_contexts",
    "prior_output",
    "prior_output_file",
    "gate_feedback",
    "dry_run",
];

/// Run the engine's own registry checks (gate types, action executors, item
/// providers) on one workflow, against the engine `workflow validate` builds.
///
/// `foreach '...'` errors are dropped: `validate_workflow_semantics` reports
/// unknown item providers against an identical registry, so keeping them would
/// list each one twice.
fn registry_errors(
    engine: &std::result::Result<runkon_flow::FlowEngine, String>,
    workflow: &WorkflowDef,
) -> Vec<ValidationError> {
    match engine {
        Err(e) => vec![ValidationError {
            message: format!("internal: could not build validation engine: {e}"),
            hint: None,
        }],
        Ok(engine) => match engine.validate(workflow) {
            Ok(()) => Vec::new(),
            Err(errs) => errs
                .into_iter()
                .filter(|e| !e.message.starts_with("foreach '"))
                .collect(),
        },
    }
}

fn collect_step_names(nodes: &[WorkflowNode], out: &mut HashSet<String>) {
    for node in nodes {
        match node {
            WorkflowNode::Call(c) => {
                out.insert(c.agent.label().to_string());
            }
            WorkflowNode::CallWorkflow(c) => {
                out.insert(c.workflow.clone());
            }
            WorkflowNode::Script(s) => {
                out.insert(s.name.clone());
            }
            WorkflowNode::Gate(g) => {
                out.insert(g.name.clone());
            }
            WorkflowNode::ForEach(f) => {
                out.insert(f.name.clone());
            }
            WorkflowNode::Parallel(p) => {
                out.extend(p.calls.iter().map(|a| a.label().to_string()));
            }
            _ => {}
        }
        if let Some(body) = node.body() {
            collect_step_names(body, out);
        }
    }
}

/// Every word (run of `[A-Za-z0-9_]`) in the scripts the workflow runs.
/// Scripts export extra variables through `FLOW_OUTPUT`, which can't be seen
/// statically, so a variable named in a script counts as known.
fn script_words<F>(workflow: &WorkflowDef, resolver: &F) -> HashSet<String>
where
    F: Fn(&str) -> std::result::Result<std::path::PathBuf, String>,
{
    let mut nodes = Vec::new();
    collect_script_nodes(&workflow.body, &mut nodes);
    collect_script_nodes(&workflow.always, &mut nodes);
    let mut words = HashSet::new();
    for node in nodes {
        if node.run.contains("{{") {
            continue;
        }
        let Some(text) = resolver(&node.run)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
        else {
            continue;
        };
        words.extend(
            text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| !w.is_empty())
                .map(str::to_string),
        );
    }
    words
}

fn is_variable_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Warn about `{{key}}` references that are neither a declared input, an
/// engine or conductor variable, an `item.*` / `<step>.<field>` reference nor
/// a word in a script the workflow runs: the engine leaves such a reference
/// unsubstituted. `texts` are `(file label, text)` pairs: the `.wf` source,
/// agent briefs and prompt snippets. Each (key, file) is reported once.
fn unknown_variable_warnings(
    workflow: &WorkflowDef,
    texts: &[(String, String)],
    script_words: &HashSet<String>,
) -> Vec<BatchValidationWarning> {
    let mut step_names = HashSet::new();
    collect_step_names(&workflow.body, &mut step_names);
    collect_step_names(&workflow.always, &mut step_names);

    let is_known = |key: &str| -> bool {
        if key == "else"
            || workflow.inputs.iter().any(|i| i.name == key)
            || ENGINE_BUILTIN_VARIABLES.contains(&key)
            || super::coordinator::CONDUCTOR_INJECTED_VARIABLES.contains(&key)
            || script_words.contains(key)
        {
            return true;
        }
        key.split_once('.')
            .is_some_and(|(head, _)| head == "item" || step_names.contains(head))
    };

    let mut warnings = Vec::new();
    for (file, text) in texts {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut rest = text.as_str();
        while let Some(open) = rest.find("{{") {
            let after = &rest[open + 2..];
            let Some(close) = after.find("}}") else { break };
            let key = &after[..close];
            rest = &after[close + 2..];
            if is_variable_name(key) && !is_known(key) && seen.insert(key) {
                warnings.push(BatchValidationWarning {
                    message: format!(
                        "unknown variable '{{{{{key}}}}}' in {file} (not an input, built-in or \
                         script export); it is left unsubstituted at run time"
                    ),
                });
            }
        }
    }
    warnings
}

// ---------------------------------------------------------------------------
// Batch workflow validation (orchestration)
// ---------------------------------------------------------------------------

/// A non-blocking warning found during batch validation (e.g. unknown bot name).
#[derive(Debug, Clone)]
pub struct BatchValidationWarning {
    pub message: String,
}

impl std::fmt::Display for BatchValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Result of validating a single workflow.
#[derive(Debug)]
pub struct WorkflowValidationEntry {
    /// Workflow name.
    pub name: String,
    /// Blocking errors found during validation.
    pub errors: Vec<ValidationError>,
    /// Non-blocking warnings (e.g. unknown bot names).
    pub warnings: Vec<BatchValidationWarning>,
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
    let mut script_roots = vec![
        std::path::PathBuf::from(wt_path),
        std::path::PathBuf::from(wt_path).join(".conductor/scripts"),
        std::path::PathBuf::from(repo_path),
        std::path::PathBuf::from(repo_path).join(".conductor/scripts"),
    ];
    if let Some(skills) = default_skills_dir() {
        script_roots.push(skills);
    }
    let script_resolver = make_script_resolver(script_roots);

    let make_error = |message: String, hint: Option<String>| -> ValidationError {
        ValidationError { message, hint }
    };

    // Pre-collect unique refs across all workflows and batch-check
    // against global directories once, avoiding O(N) redundant
    // filesystem probes when multiple workflows share agents,
    // snippets, or schemas.
    //
    // We cache per-workflow refs to avoid traversing each workflow's
    // AST twice (once for global dedup, once for per-workflow checks).
    struct WorkflowRefs {
        agents: Vec<AgentRef>,
        snippets: Vec<String>,
        schemas: Vec<String>,
        bot_names: Vec<String>,
    }
    let per_wf_refs: Vec<WorkflowRefs> = workflows
        .iter()
        .map(|wf| WorkflowRefs {
            agents: wf.collect_all_agent_refs(),
            snippets: wf.collect_all_snippet_refs(),
            schemas: wf.collect_all_schema_refs(),
            bot_names: wf.collect_all_as_identities(),
        })
        .collect();

    let mut all_agent_refs_set: HashSet<AgentRef> = HashSet::new();
    let mut all_snippet_names_set: HashSet<String> = HashSet::new();
    let mut all_schema_names_set: HashSet<String> = HashSet::new();
    for refs in &per_wf_refs {
        all_agent_refs_set.extend(refs.agents.iter().cloned());
        all_snippet_names_set.extend(refs.snippets.iter().cloned());
        all_schema_names_set.extend(refs.schemas.iter().cloned());
    }
    let all_agent_refs: Vec<AgentRef> = all_agent_refs_set.into_iter().collect();
    let all_snippet_names: Vec<String> = all_snippet_names_set.into_iter().collect();
    let all_schema_names: Vec<String> = all_schema_names_set.into_iter().collect();

    // Collect all plugin_dirs from call nodes for agent resolution.
    let all_plugin_dirs: Vec<String> = workflows
        .iter()
        .flat_map(|wf| wf.collect_all_plugin_dirs())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let global_agent_specs: Vec<AgentSpec> = all_agent_refs.iter().map(AgentSpec::from).collect();
    let globally_missing_agents: HashSet<String> = agent_config::find_missing_agents(
        wt_path,
        repo_path,
        &global_agent_specs,
        None,
        &all_plugin_dirs,
    )
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

    // Cache sub-workflow loads so each workflow is parsed from disk at most once
    // across the entire batch (cycle detection + semantic validation both call
    // the loader for the same sub-workflow names).
    let loader_cache: RefCell<
        HashMap<String, std::sync::Arc<std::result::Result<WorkflowDef, String>>>,
    > = RefCell::new(HashMap::new());
    let cached_loader = |name: &str| -> std::result::Result<WorkflowDef, String> {
        if let Some(cached) = loader_cache.borrow().get(name) {
            return (**cached).clone();
        }
        let result = loader(name);
        loader_cache
            .borrow_mut()
            .insert(name.to_string(), std::sync::Arc::new(result.clone()));
        result
    };

    // Build a validation-only registry and context once for the whole batch.
    let validation_registry = super::runkon_bridge::build_rk_validation_registry();
    let validation_ctx = ValidationContext {
        registry: &validation_registry,
        valid_targets: CONDUCTOR_TARGETS,
    };

    // The engine a run validates against, built once for the batch.
    let validation_engine =
        super::coordinator::build_validation_engine().map_err(|e| e.to_string());

    let mut entries = Vec::new();
    for (workflow, wf_refs) in workflows.iter().zip(per_wf_refs.iter()) {
        let wf_name = &workflow.name;
        let mut wf_errors: Vec<ValidationError> = Vec::new();

        // --- Agents: emit errors directly from pre-computed missing set ---
        for r in &wf_refs.agents {
            // A global miss may still resolve in the workflow's own
            // `.conductor/workflows/<name>/agents/`, as it does at run time.
            if globally_missing_agents.contains(r.label())
                && !agent_config::find_missing_agents(
                    wt_path,
                    repo_path,
                    &[AgentSpec::from(r)],
                    Some(wf_name),
                    &all_plugin_dirs,
                )
                .is_empty()
            {
                wf_errors.push(make_error(format!("missing agent: {}", r.label()), None));
            }
        }

        // --- Snippets: emit errors directly from pre-computed missing set ---
        for snippet in &wf_refs.snippets {
            if globally_missing_snippets.contains(snippet) {
                wf_errors.push(make_error(
                    format!("missing prompt snippet: {snippet}"),
                    None,
                ));
            }
        }

        // --- Schemas: emit errors directly from pre-computed issue map ---
        for schema in &wf_refs.schemas {
            let Some(issue) = global_schema_issue_map.get(schema) else {
                continue;
            };
            // As for agents, re-check a global miss with the workflow-local
            // schemas directory. An invalid global file is reported as is.
            let local_issues;
            let issues: &[schema_config::SchemaIssue] = match issue {
                schema_config::SchemaIssue::Missing(_) => {
                    local_issues = schema_config::check_schemas(
                        wt_path,
                        repo_path,
                        std::slice::from_ref(schema),
                        Some(wf_name),
                    );
                    &local_issues
                }
                invalid => std::slice::from_ref(invalid),
            };
            for issue in issues {
                match issue {
                    schema_config::SchemaIssue::Missing(s) => {
                        wf_errors.push(make_error(format!("missing schema: {s}"), None));
                    }
                    schema_config::SchemaIssue::Invalid { name: s, error } => {
                        wf_errors.push(make_error(
                            format!("invalid schema: {s}"),
                            Some(error.clone()),
                        ));
                    }
                }
            }
        }

        // --- Bot names (warnings) ---
        let unknown_bots: Vec<&String> = wf_refs
            .bot_names
            .iter()
            .filter(|b| !known_bots.contains(b.as_str()))
            .collect();

        // --- Cycle detection ---
        if let Err(cycle_msg) = detect_workflow_cycles(wf_name, &cached_loader) {
            wf_errors.push(make_error(format!("cycle detected: {cycle_msg}"), None));
        }

        // --- Semantic validation ---
        let report = validate_workflow_semantics(workflow, &cached_loader, &validation_ctx);
        for err in report.errors {
            wf_errors.push(err);
        }
        let mut warnings: Vec<BatchValidationWarning> = report
            .warnings
            .into_iter()
            .map(|msg| BatchValidationWarning { message: msg })
            .collect();

        // --- Script step validation ---
        let script_errors = validate_script_steps(workflow, &script_resolver);
        for err in script_errors {
            wf_errors.push(err);
        }
        wf_errors.extend(check_legacy_output_markers(workflow, &script_resolver));

        // --- Engine registry checks (gate types, executors, item providers) ---
        wf_errors.extend(registry_errors(&validation_engine, workflow));

        // --- Unknown {{variables}} (warnings) ---
        let mut texts: Vec<(String, String)> = Vec::new();
        if let Ok(src) = std::fs::read_to_string(&workflow.source_path) {
            texts.push((workflow.source_path.clone(), src));
        }
        for r in &wf_refs.agents {
            if let Ok(agent) = agent_config::load_agent(
                wt_path,
                repo_path,
                &AgentSpec::from(r),
                Some(wf_name),
                &all_plugin_dirs,
            ) {
                texts.push((format!("agent {}", r.label()), agent.prompt));
            }
        }
        for snippet in &wf_refs.snippets {
            if let Ok(text) = prompt_config::load_prompt_snippet(
                wt_path,
                repo_path,
                &prompt_config::PromptSnippetRef::from_str_value(snippet),
                Some(wf_name),
            ) {
                texts.push((format!("snippet {snippet}"), text));
            }
        }
        warnings.extend(unknown_variable_warnings(
            workflow,
            &texts,
            &script_words(workflow, &script_resolver),
        ));

        warnings.extend(unknown_bots.iter().map(|b| BatchValidationWarning {
            message: format!("unknown bot name '{b}' (not in [github.apps])"),
        }));

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

/// Validate workflow templates (`(file name, raw .wft content)` pairs).
///
/// Checks what doesn't depend on a repo: the template parses, its body parses,
/// cycles, semantic rules, the engine registry checks and unknown variables.
/// Agent, snippet, schema and script files are not checked: a template's files
/// are created in the target repo when it is instantiated. `repo_path` is only
/// used to find the scripts a template runs, to know which variables they
/// export. Entries are named `template:<name>`.
pub fn validate_template_sources(
    sources: &[(String, String)],
    repo_path: &str,
) -> BatchValidationResult {
    let mut parse_errors = Vec::new();
    let mut defs: Vec<(String, String, WorkflowDef)> = Vec::new();
    for (file, content) in sources {
        let template = match crate::workflow_template::parse_wft(content, file) {
            Ok(t) => t,
            Err(e) => {
                parse_errors.push(format!("template {file}: {e}"));
                continue;
            }
        };
        match runkon_flow::dsl::parse_workflow_str(&template.body, &format!("template:{file}")) {
            Ok(def) => defs.push((template.metadata.name, template.body, def)),
            Err(e) => parse_errors.push(format!("template {file}: {e}")),
        }
    }

    let loader = |name: &str| -> std::result::Result<WorkflowDef, String> {
        defs.iter()
            .find(|(_, _, d)| d.name == name)
            .map(|(_, _, d)| d.clone())
            .ok_or_else(|| format!("workflow '{name}' not found"))
    };
    let registry = super::runkon_bridge::build_rk_validation_registry();
    let ctx = ValidationContext {
        registry: &registry,
        valid_targets: CONDUCTOR_TARGETS,
    };
    let engine = super::coordinator::build_validation_engine().map_err(|e| e.to_string());
    let repo = std::path::PathBuf::from(repo_path);
    let mut roots = vec![repo.clone(), repo.join(".conductor/scripts")];
    if let Some(skills) = default_skills_dir() {
        roots.push(skills);
    }
    let script_resolver = make_script_resolver(roots);

    let entries = defs
        .iter()
        .map(|(name, body, def)| {
            let mut errors = Vec::new();
            if let Err(cycle) = detect_workflow_cycles(&def.name, &loader) {
                errors.push(ValidationError {
                    message: format!("cycle detected: {cycle}"),
                    hint: None,
                });
            }
            let report = validate_workflow_semantics(def, &loader, &ctx);
            // A template may call sub-workflows that live in the repo it is
            // instantiated into, so an unresolved one is not an error here.
            errors.extend(
                report
                    .errors
                    .into_iter()
                    .filter(|e| !e.message.starts_with("Sub-workflow '")),
            );
            let mut warnings: Vec<BatchValidationWarning> = report
                .warnings
                .into_iter()
                .map(|message| BatchValidationWarning { message })
                .collect();
            errors.extend(registry_errors(&engine, def));
            warnings.extend(unknown_variable_warnings(
                def,
                &[(format!("template {name}"), body.clone())],
                &script_words(def, &script_resolver),
            ));
            WorkflowValidationEntry {
                name: format!("template:{name}"),
                errors,
                warnings,
            }
        })
        .collect();

    BatchValidationResult {
        entries,
        parse_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runkon_flow::dsl::parse_workflow_str;

    /// Build a minimal WorkflowDef by parsing a .wf string.
    fn parse_wf(src: &str) -> WorkflowDef {
        parse_workflow_str(src, "<test>").expect("test workflow should parse")
    }

    /// A loader that always fails — useful when we don't care about sub-workflows.
    fn failing_loader(name: &str) -> std::result::Result<WorkflowDef, String> {
        Err(format!("workflow '{name}' not found"))
    }

    #[test]
    fn batch_missing_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let wf = parse_wf(
            r#"workflow agent-test {
                meta { trigger = "manual" targets = ["worktree"] }
                call nonexistent-agent {}
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        let errs = &result.entries[0].errors;
        assert!(
            errs.iter().any(|e| e.message.contains("missing agent")),
            "expected missing agent error, got: {errs:?}"
        );
    }

    #[test]
    fn batch_missing_snippet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

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
            errs.iter()
                .any(|e| e.message.contains("missing prompt snippet")),
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
            errs.iter().any(|e| e.message.contains("missing schema")),
            "expected missing schema error, got: {errs:?}"
        );
    }

    #[test]
    fn batch_invalid_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

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
            errs.iter().any(|e| e.message.contains("invalid schema")),
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

        let known_bots = HashSet::from(["real-bot".to_string()]);
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        let warnings = &result.entries[0].warnings;
        assert!(
            warnings.iter().any(
                |w| w.message.contains("unknown bot name") && w.message.contains("unknown-bot")
            ),
            "expected unknown bot warning, got: {warnings:?}"
        );
    }

    #[test]
    fn batch_semantic_warnings_are_surfaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        // A foreach over worktrees without a scope triggers the "base_branch will be inferred" warning
        let wf = parse_wf(
            r#"workflow semantic-warn-test {
                meta { trigger = "manual" targets = ["worktree"] }
                foreach fan-out {
                    over = "worktrees"
                    max_parallel = 4
                    workflow = "child-workflow"
                }
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
        let warnings = &result.entries[0].warnings;
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("base_branch will be inferred")),
            "expected semantic warning about base_branch inference, got: {warnings:?}"
        );
    }

    #[test]
    fn batch_parse_errors_affect_total_and_failed_count() {
        let result = BatchValidationResult {
            entries: vec![WorkflowValidationEntry {
                name: "good".to_string(),
                errors: vec![],
                warnings: vec![],
            }],
            parse_errors: vec![
                "broken.wf: unexpected token".to_string(),
                "garbage.wf: parse error".to_string(),
            ],
        };
        // total = 1 entry + 2 parse errors = 3
        assert_eq!(result.total(), 3);
        // failed = 0 entry failures + 2 parse errors = 2
        assert_eq!(result.failed_count(), 2);
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
            errs.iter().any(|e| e.message.contains("cycle")),
            "expected cycle detection error, got: {errs:?}"
        );
    }

    /// Write an executable script under `<dir>/.conductor/scripts/`.
    fn write_script(dir: &std::path::Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let scripts = dir.join(".conductor/scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let file = scripts.join(name);
        std::fs::write(&file, body).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn batch_legacy_output_marker_in_script() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        write_script(
            dir.path(),
            "detect.sh",
            "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>'\necho '{}'\necho '<<<END_CONDUCTOR_OUTPUT>>>'\n",
        );

        let wf = parse_wf(
            r#"workflow legacy {
                meta { trigger = "manual" targets = ["worktree"] }
                if prior.done {
                    script detect { run = ".conductor/scripts/detect.sh" }
                }
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        let errs = &result.entries[0].errors;
        let legacy: Vec<_> = errs
            .iter()
            .filter(|e| e.message.contains("CONDUCTOR_OUTPUT"))
            .collect();
        assert_eq!(
            legacy.len(),
            1,
            "expected one legacy marker error, got: {errs:?}"
        );
        assert!(legacy[0].message.contains("'detect'"));
        assert!(legacy[0].message.contains("detect.sh"));
        assert!(legacy[0]
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("FLOW_OUTPUT"));
    }

    #[test]
    fn batch_legacy_output_marker_in_always_block_script() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        write_script(
            dir.path(),
            "cleanup.sh",
            "#!/bin/sh\necho '<<<CONDUCTOR_OUTPUT>>>'\necho '{}'\necho '<<<END_CONDUCTOR_OUTPUT>>>'\n",
        );

        let wf = parse_wf(
            r#"workflow legacy-always {
                meta { trigger = "manual" targets = ["worktree"] }
                always {
                    script cleanup { run = ".conductor/scripts/cleanup.sh" }
                }
            }"#,
        );
        assert_eq!(
            wf.always.len(),
            1,
            "script should parse into the always block"
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        let errs = &result.entries[0].errors;
        assert!(
            errs.iter()
                .any(|e| e.message.contains("'cleanup'") && e.message.contains("CONDUCTOR_OUTPUT")),
            "expected legacy marker error for always-block script, got: {errs:?}"
        );
    }

    #[test]
    fn batch_flow_output_script_has_no_legacy_marker_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        write_script(
            dir.path(),
            "detect.sh",
            "#!/bin/sh\necho '<<<FLOW_OUTPUT>>>'\necho '{}'\necho '<<<END_FLOW_OUTPUT>>>'\n",
        );

        let wf = parse_wf(
            r#"workflow current {
                meta { trigger = "manual" targets = ["worktree"] }
                script detect { run = ".conductor/scripts/detect.sh" }
            }"#,
        );

        let known_bots = HashSet::new();
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        let errs = &result.entries[0].errors;
        assert!(errs.is_empty(), "expected no errors, got: {errs:?}");
    }

    // -----------------------------------------------------------------
    // Registry checks, workflow-local resolution, variables, templates
    // -----------------------------------------------------------------

    fn run_batch(dir: &std::path::Path, wf: WorkflowDef) -> WorkflowValidationEntry {
        let path = dir.to_str().unwrap();
        let known_bots = HashSet::new();
        let mut result =
            validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);
        result.entries.remove(0)
    }

    fn gate_wf(gate_type: &str, extra: &str) -> WorkflowDef {
        parse_wf(&format!(
            r#"workflow gate-test {{
                meta {{ trigger = "manual" targets = ["worktree"] }}
                gate {gate_type} {{ {extra} timeout = "1h" on_timeout = fail }}
            }}"#
        ))
    }

    fn write_file(dir: &std::path::Path, rel: &str, body: &str) {
        let file = dir.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }

    #[test]
    fn batch_unknown_gate_type_fails() {
        let dir = tempfile::tempdir().unwrap();
        let entry = run_batch(dir.path(), gate_wf("bogus_gate", r#"prompt = "x""#));
        assert!(
            entry.errors.iter().any(|e| e.message.contains("bogus_gate")
                && e.message.contains("no registered GateResolver")),
            "expected unknown gate error naming the gate, got: {:?}",
            entry.errors
        );
    }

    #[test]
    fn batch_known_gate_types_pass() {
        let dir = tempfile::tempdir().unwrap();
        for (gate_type, extra) in [
            ("human_approval", r#"prompt = "x""#),
            ("human_review", r#"prompt = "x""#),
            ("pr_approval", r#"mode = "review_decision""#),
            ("pr_checks", ""),
        ] {
            let entry = run_batch(dir.path(), gate_wf(gate_type, extra));
            assert!(
                entry.errors.is_empty(),
                "gate type {gate_type} should pass, got: {:?}",
                entry.errors
            );
        }
    }

    #[test]
    fn batch_call_passes_registry_check() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".conductor/agents/helper.md",
            "---\nrole: actor\n---\nDo it.\n",
        );
        let wf = parse_wf(
            r#"workflow call-test {
                meta { trigger = "manual" targets = ["worktree"] }
                call helper {}
            }"#,
        );
        let entry = run_batch(dir.path(), wf);
        assert!(entry.errors.is_empty(), "got: {:?}", entry.errors);
    }

    #[test]
    fn batch_workflow_local_agent_resolves() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".conductor/workflows/local-agent/agents/only-here.md",
            "---\nrole: actor\n---\nDo it.\n",
        );
        let wf = parse_wf(
            r#"workflow local-agent {
                meta { trigger = "manual" targets = ["worktree"] }
                call only-here {}
            }"#,
        );
        let entry = run_batch(dir.path(), wf);
        assert!(
            !entry
                .errors
                .iter()
                .any(|e| e.message.contains("missing agent")),
            "got: {:?}",
            entry.errors
        );
    }

    #[test]
    fn batch_workflow_local_schema_resolves() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".conductor/agents/helper.md",
            "---\nrole: actor\n---\nDo it.\n",
        );
        write_file(
            dir.path(),
            ".conductor/workflows/local-schema/schemas/only-here.yaml",
            "fields:\n  summary: string\n",
        );
        let wf = parse_wf(
            r#"workflow local-schema {
                meta { trigger = "manual" targets = ["worktree"] }
                call helper { output = "only-here" }
            }"#,
        );
        let entry = run_batch(dir.path(), wf);
        assert!(
            !entry.errors.iter().any(|e| e.message.contains("schema")),
            "got: {:?}",
            entry.errors
        );
    }

    #[test]
    fn batch_missing_script_fails() {
        let dir = tempfile::tempdir().unwrap();
        let wf = parse_wf(
            r#"workflow script-test {
                meta { trigger = "manual" targets = ["worktree"] }
                script gone { run = ".conductor/scripts/gone.sh" }
            }"#,
        );
        let entry = run_batch(dir.path(), wf);
        assert!(
            entry.errors.iter().any(|e| e.message.contains("'gone'")),
            "got: {:?}",
            entry.errors
        );
    }

    #[test]
    fn batch_non_executable_script_fails() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            ".conductor/scripts/plain.sh",
            "#!/bin/sh\necho hi\n",
        );
        let file = dir.path().join(".conductor/scripts/plain.sh");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let wf = parse_wf(
            r#"workflow script-test {
                meta { trigger = "manual" targets = ["worktree"] }
                script plain { run = ".conductor/scripts/plain.sh" }
            }"#,
        );
        let entry = run_batch(dir.path(), wf);
        assert!(
            entry
                .errors
                .iter()
                .any(|e| e.message.contains("not executable")),
            "got: {:?}",
            entry.errors
        );
    }

    #[test]
    fn batch_unknown_variable_warns() {
        let dir = tempfile::tempdir().unwrap();
        write_script(
            dir.path(),
            "emit.sh",
            "#!/bin/sh\necho '{\"exported_key\": \"v\"}'\n",
        );
        write_file(
            dir.path(),
            ".conductor/agents/helper.md",
            "---\nrole: actor\n---\nBrief {{bogus}} {{prior_context}} {{#if x}}y{{/if}}\n",
        );
        let src = r#"workflow vars {
            meta { trigger = "manual" targets = ["worktree"] }
            inputs { declared }
            script emit { run = ".conductor/scripts/emit.sh" }
            script consume {
                run = ".conductor/scripts/emit.sh"
                env = { A = "{{nope}}" B = "{{declared}}" C = "{{prior_context}}" D = "{{exported_key}}" E = "{{emit.thing}}" F = "{{ticket_id}}" }
            }
            call helper {}
        }"#;
        let wf_path = dir.path().join(".conductor/workflows/vars.wf");
        write_file(dir.path(), ".conductor/workflows/vars.wf", src);
        let wf = parse_workflow_str(src, wf_path.to_str().unwrap()).unwrap();
        let entry = run_batch(dir.path(), wf);
        assert!(entry.errors.is_empty(), "got: {:?}", entry.errors);
        let msgs: Vec<&str> = entry.warnings.iter().map(|w| w.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("'{{nope}}'")),
            "got: {msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("'{{bogus}}'") && m.contains("agent helper")),
            "brief variable should warn: {msgs:?}"
        );
        for known in [
            "declared",
            "prior_context",
            "exported_key",
            "emit.thing",
            "ticket_id",
            "#if",
            "/if",
        ] {
            assert!(
                !msgs
                    .iter()
                    .any(|m| m.contains(&format!("'{{{{{known}}}}}'"))),
                "{known} must not warn: {msgs:?}"
            );
        }
    }

    fn template_source(name: &str, body: &str) -> (String, String) {
        (
            format!("{name}.wft"),
            format!("---\nname: {name}\ndescription: test\nversion: \"1.0.0\"\n---\n{body}\n"),
        )
    }

    #[test]
    fn templates_broken_gate_fails() {
        let body = r#"workflow broken {
            meta { trigger = "manual" targets = ["worktree"] }
            gate bogus_gate { prompt = "x" timeout = "1h" on_timeout = fail }
        }"#;
        let result = validate_template_sources(&[template_source("broken", body)], ".");
        assert_eq!(result.failed_count(), 1);
        assert_eq!(result.entries[0].name, "template:broken");
        assert!(
            result.entries[0]
                .errors
                .iter()
                .any(|e| e.message.contains("bogus_gate")),
            "got: {:?}",
            result.entries[0].errors
        );
    }

    #[test]
    fn templates_parse_error_reported() {
        let sources = vec![("bad.wft".to_string(), "no frontmatter here".to_string())];
        let result = validate_template_sources(&sources, ".");
        assert_eq!(result.parse_errors.len(), 1);
        assert_eq!(result.failed_count(), 1);
    }

    #[test]
    fn embedded_templates_all_pass() {
        let sources = crate::workflow_template::embedded_template_sources();
        assert!(!sources.is_empty(), "no embedded templates found");
        let result = validate_template_sources(&sources, ".");
        let failures: Vec<String> = result
            .entries
            .iter()
            .filter(|e| !e.errors.is_empty())
            .map(|e| format!("{}: {:?}", e.name, e.errors))
            .chain(result.parse_errors.iter().cloned())
            .collect();
        assert!(failures.is_empty(), "template failures: {failures:#?}");
    }
}
