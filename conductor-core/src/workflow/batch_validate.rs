use std::collections::{HashMap, HashSet};

use crate::agent_config::{self, AgentSpec};
use crate::prompt_config;
use crate::schema_config;
use crate::workflow_dsl::{
    default_skills_dir, detect_workflow_cycles, make_script_resolver, validate_script_steps,
    validate_workflow_semantics, AgentRef, WorkflowDef,
};

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
    //
    // We cache per-workflow refs to avoid traversing each workflow's
    // AST twice (once for global dedup, once for per-workflow checks).
    struct WorkflowRefs {
        agents: Vec<AgentRef>,
        snippets: Vec<String>,
        schemas: Vec<String>,
    }
    let per_wf_refs: Vec<WorkflowRefs> = workflows
        .iter()
        .map(|wf| WorkflowRefs {
            agents: wf.collect_all_agent_refs(),
            snippets: wf.collect_all_snippet_refs(),
            schemas: wf.collect_all_schema_refs(),
        })
        .collect();

    let mut all_agent_refs = Vec::new();
    let mut all_snippet_names = Vec::new();
    let mut all_schema_names = Vec::new();
    for refs in &per_wf_refs {
        all_agent_refs.extend(refs.agents.iter().cloned());
        all_snippet_names.extend(refs.snippets.iter().cloned());
        all_schema_names.extend(refs.schemas.iter().cloned());
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
    for (workflow, wf_refs) in workflows.iter().zip(per_wf_refs.iter()) {
        let wf_name = &workflow.name;
        let mut wf_errors: Vec<String> = Vec::new();

        // --- Agents: emit errors directly from pre-computed missing set ---
        for r in &wf_refs.agents {
            if globally_missing_agents.contains(r.label()) {
                wf_errors.push(format!("missing agent: {}", r.label()));
            }
        }

        // --- Snippets: emit errors directly from pre-computed missing set ---
        for snippet in &wf_refs.snippets {
            if globally_missing_snippets.contains(snippet) {
                wf_errors.push(format!("missing prompt snippet: {snippet}"));
            }
        }

        // --- Schemas: emit errors directly from pre-computed issue map ---
        for schema in &wf_refs.schemas {
            if let Some(issue) = global_schema_issue_map.get(schema) {
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
    use crate::workflow_dsl::parse_workflow_str;

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
            errs.iter().any(|e| e.contains("missing agent")),
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

        let known_bots = HashSet::from(["real-bot".to_string()]);
        let result = validate_workflows_batch(&[wf], &[], path, path, &known_bots, &failing_loader);

        assert_eq!(result.entries.len(), 1);
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
