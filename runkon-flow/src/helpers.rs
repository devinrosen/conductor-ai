use crate::dsl::{WhileNode, WorkflowNode};
use crate::status::WorkflowStepStatus;

// Forward declaration for ExecutionState — defined in engine.rs
use crate::engine::ExecutionState;

// ---------------------------------------------------------------------------
// Conductor output block parsing
// ---------------------------------------------------------------------------

/// Parsed `<<<CONDUCTOR_OUTPUT>>>` block.
pub struct ConductorOutput {
    pub markers: Vec<String>,
    pub context: String,
}

/// Extract markers and context from a `<<<CONDUCTOR_OUTPUT>>> … <<<END_CONDUCTOR_OUTPUT>>>`
/// block embedded in `text`. Returns `None` when no valid block is found.
pub fn parse_conductor_output(text: &str) -> Option<ConductorOutput> {
    const START: &str = "<<<CONDUCTOR_OUTPUT>>>";
    const END: &str = "<<<END_CONDUCTOR_OUTPUT>>>";

    // Find the last START marker whose content starts with `{`, `[`, or a markdown
    // code fence (``` …) — guards against occurrences inside JSON string values or
    // plain prose where the marker appears as a literal string.
    let start_pos = text
        .rmatch_indices(START)
        .find(|(pos, _)| {
            let after = text[pos + START.len()..].trim_start();
            after.starts_with('{') || after.starts_with('[') || after.starts_with('`')
        })
        .map(|(pos, _)| pos)?;

    let after_start = &text[start_pos + START.len()..];
    let end_pos = after_start.find(END)?;
    let raw = after_start[..end_pos].trim();

    // Strip optional markdown code fences (```json … ``` or ``` … ```)
    let raw = if let Some(stripped) = raw.strip_prefix("```") {
        let stripped = stripped.trim_start_matches(|c: char| c.is_alphanumeric());
        stripped
            .trim_start_matches('\n')
            .trim_end_matches("```")
            .trim()
    } else {
        raw
    };

    // Best-effort cleanup common in LLM output: trailing commas and bad escapes.
    let cleaned = strip_trailing_commas(raw);
    let cleaned = fix_backslash_escapes(&cleaned);

    let value: serde_json::Value = serde_json::from_str(&cleaned)
        .map_err(|e| {
            tracing::warn!("parse_conductor_output: invalid JSON: {e}");
        })
        .ok()?;

    let markers = value
        .get("markers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let context = value
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(ConductorOutput { markers, context })
}

/// Strip trailing commas before `}` or `]` (common LLM JSON artifact).
fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            let rest = chars.clone().find(|ch| !ch.is_whitespace());
            if matches!(rest, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Replace `\` followed by non-JSON-escape characters with `\\`.
fn fix_backslash_escapes(s: &str) -> String {
    const VALID: &[char] = &['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(next) if VALID.contains(next) => {
                    out.push('\\');
                }
                _ => {
                    out.push('\\');
                    out.push('\\');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse a human-readable duration string into a `std::time::Duration`.
///
/// Supported suffixes: `ms` (milliseconds), `h` (hours), `m` (minutes), `s` (seconds).
/// A bare integer is treated as seconds.
pub(crate) fn parse_duration(s: &str) -> std::result::Result<std::time::Duration, String> {
    // "ms" must precede "s" so "3ms" is not partially matched by the "s" entry.
    for (suffix, millis_per_unit) in &[("ms", 1u64), ("h", 3_600_000), ("m", 60_000), ("s", 1_000)]
    {
        if let Some(n) = s.strip_suffix(suffix) {
            let count = n
                .parse::<u64>()
                .map_err(|e| format!("invalid timeout '{s}': {e}"))?;
            return Ok(std::time::Duration::from_millis(count * millis_per_unit));
        }
    }
    let sec = s
        .parse::<u64>()
        .map_err(|e| format!("invalid timeout '{s}': {e}"))?;
    Ok(std::time::Duration::from_secs(sec))
}

/// Serialize `v` to a JSON string; on failure log a warning with `ctx` and return `"[]"`.
pub fn serialize_or_empty_array<T: serde::Serialize>(v: &T, ctx: &str) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| {
        tracing::warn!("{ctx}: failed to serialize to JSON array: {e}");
        "[]".to_string()
    })
}

/// Build a human-readable summary of a workflow execution.
pub fn build_workflow_summary(state: &ExecutionState) -> String {
    let steps = match state.persistence.get_steps(&state.workflow_run_id) {
        Ok(steps) => steps,
        Err(e) => {
            tracing::warn!(
                "build_workflow_summary: failed to fetch steps for run {}: {e}",
                state.workflow_run_id
            );
            vec![]
        }
    };

    let total = steps.len();
    let (completed, failed, skipped, timed_out) = steps.iter().fold(
        (0usize, 0usize, 0usize, 0usize),
        |(c, f, sk, to), s| match s.status {
            WorkflowStepStatus::Completed => (c + 1, f, sk, to),
            WorkflowStepStatus::Failed => (c, f + 1, sk, to),
            WorkflowStepStatus::Skipped => (c, f, sk + 1, to),
            WorkflowStepStatus::TimedOut => (c, f, sk, to + 1),
            _ => (c, f, sk, to),
        },
    );

    let mut lines = Vec::new();
    lines.push(format!(
        "Workflow '{}': {completed}/{total} steps completed{}{}{}",
        state.workflow_name,
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        },
        if skipped > 0 {
            format!(", {skipped} skipped")
        } else {
            String::new()
        },
        if timed_out > 0 {
            format!(", {timed_out} timed out")
        } else {
            String::new()
        },
    ));

    for step in &steps {
        let marker = step.status.short_label();
        let iter_label = if step.iteration > 0 {
            format!(" (iter {})", step.iteration)
        } else {
            String::new()
        };
        let never_executed = step.status == WorkflowStepStatus::Failed && step.started_at.is_none();
        let step_note = if never_executed {
            " (never executed)"
        } else {
            ""
        };
        lines.push(format!(
            "  [{marker}] {}{iter_label}{step_note}",
            step.step_name
        ));
    }

    if state.all_succeeded {
        lines.push("Status: SUCCESS".to_string());
    } else {
        lines.push("Status: FAILED".to_string());
    }

    lines.join("\n")
}

/// Extract all leaf-node step keys from a workflow node.
///
/// Recurses into control-flow nodes (if/unless/while/always) and collects
/// keys from all trackable leaves: `Call`, `Parallel` agents, `Gate`, and
/// `CallWorkflow`.
pub fn collect_leaf_step_keys(node: &WorkflowNode) -> Vec<String> {
    match node {
        WorkflowNode::Call(c) => vec![c.agent.step_key()],
        WorkflowNode::Parallel(p) => p.calls.iter().map(|a| a.step_key()).collect(),
        WorkflowNode::Gate(g) => vec![g.name.clone()],
        WorkflowNode::CallWorkflow(cw) => vec![format!("workflow:{}", cw.workflow)],
        WorkflowNode::Script(s) => vec![s.name.clone()],
        WorkflowNode::If(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Unless(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::While(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::DoWhile(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Do(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Always(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::ForEach(n) => vec![format!("foreach:{}", n.name)],
    }
}

/// Find the starting iteration for a while loop on resume.
///
/// Looks at the skip_completed set for step keys that match body nodes of the
/// while loop. Returns the max iteration that has all body nodes completed,
/// so the loop resumes from the iteration where it failed.
pub fn find_max_completed_while_iteration(state: &ExecutionState, node: &WhileNode) -> u32 {
    let step_map = match state.resume_ctx {
        Some(ref ctx) => &ctx.step_map,
        None => return 0,
    };

    // Collect step keys from all trackable body nodes
    let body_keys: Vec<String> = node.body.iter().flat_map(collect_leaf_step_keys).collect();

    if body_keys.is_empty() {
        return 0;
    }

    // Find the highest iteration where all body nodes are completed
    let mut iter = 0u32;
    loop {
        let all_done = body_keys
            .iter()
            .all(|k| step_map.contains_key(&(k.clone(), iter)));
        if !all_done {
            break;
        }
        iter += 1;
    }
    // iter is now the first incomplete iteration — start there
    iter
}

#[cfg(test)]
mod parse_tests {
    use super::parse_conductor_output;

    #[test]
    fn parses_well_formed_block_with_markers_and_context() {
        let text = concat!(
            "some preamble\n",
            "<<<CONDUCTOR_OUTPUT>>>\n",
            r#"{"markers":["a","b"],"context":"hello world"}"#,
            "\n",
            "<<<END_CONDUCTOR_OUTPUT>>>\n",
            "some suffix"
        );
        let out = parse_conductor_output(text).unwrap();
        assert_eq!(out.markers, vec!["a", "b"]);
        assert_eq!(out.context, "hello world");
    }

    #[test]
    fn strips_trailing_commas_before_braces() {
        let text = concat!(
            "<<<CONDUCTOR_OUTPUT>>>\n",
            r#"{"markers":["x",],"context":"ctx",}"#,
            "\n",
            "<<<END_CONDUCTOR_OUTPUT>>>"
        );
        let out = parse_conductor_output(text).unwrap();
        assert_eq!(out.markers, vec!["x"]);
        assert_eq!(out.context, "ctx");
    }

    #[test]
    fn fixes_invalid_backslash_escapes() {
        // \p is not a valid JSON escape sequence; fix_backslash_escapes doubles it
        // so the JSON becomes parseable.
        let text = concat!(
            "<<<CONDUCTOR_OUTPUT>>>\n",
            r#"{"markers":[],"context":"C:\path\to\file"}"#,
            "\n",
            "<<<END_CONDUCTOR_OUTPUT>>>"
        );
        let out = parse_conductor_output(text).unwrap();
        // Successfully parsed despite the invalid backslash sequences.
        assert!(out.context.contains("path"));
    }

    #[test]
    fn strips_markdown_code_fence() {
        let text = concat!(
            "<<<CONDUCTOR_OUTPUT>>>\n",
            "```json\n",
            r#"{"markers":["fenced"],"context":"fenced context"}"#,
            "\n",
            "```\n",
            "<<<END_CONDUCTOR_OUTPUT>>>"
        );
        let out = parse_conductor_output(text).unwrap();
        assert_eq!(out.markers, vec!["fenced"]);
        assert_eq!(out.context, "fenced context");
    }

    #[test]
    fn returns_none_when_no_block_present() {
        assert!(parse_conductor_output("no conductor output block here").is_none());
        assert!(parse_conductor_output("").is_none());
    }

    #[test]
    fn markers_field_missing_defaults_to_empty_vec() {
        let text = concat!(
            "<<<CONDUCTOR_OUTPUT>>>\n",
            r#"{"context":"only context, no markers"}"#,
            "\n",
            "<<<END_CONDUCTOR_OUTPUT>>>"
        );
        let out = parse_conductor_output(text).unwrap();
        assert!(out.markers.is_empty());
        assert_eq!(out.context, "only context, no markers");
    }

    #[test]
    fn context_field_missing_defaults_to_empty_string() {
        let text = concat!(
            "<<<CONDUCTOR_OUTPUT>>>\n",
            r#"{"markers":["m1"]}"#,
            "\n",
            "<<<END_CONDUCTOR_OUTPUT>>>"
        );
        let out = parse_conductor_output(text).unwrap();
        assert_eq!(out.markers, vec!["m1"]);
        assert_eq!(out.context, "");
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_leaf_step_keys, parse_duration, serialize_or_empty_array};
    use crate::dsl::{
        AgentRef, AlwaysNode, CallNode, CallWorkflowNode, Condition, DoNode, ForEachNode, GateNode,
        GateType, IfNode, OnMaxIter, ParallelNode, ScriptNode, UnlessNode, WhileNode, WorkflowNode,
    };

    fn call_node(name: &str) -> WorkflowNode {
        WorkflowNode::Call(CallNode {
            agent: AgentRef::Name(name.to_string()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
            plugin_dirs: vec![],
            timeout: None,
        })
    }

    fn script_node(name: &str) -> WorkflowNode {
        WorkflowNode::Script(ScriptNode {
            name: name.to_string(),
            run: "echo hello".to_string(),
            env: Default::default(),
            timeout: None,
            retries: 0,
            on_fail: None,
            bot_name: None,
        })
    }

    // ---- collect_leaf_step_keys ----

    #[test]
    fn leaf_keys_from_call_node() {
        let node = call_node("plan");
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["plan".to_string()]);
    }

    #[test]
    fn leaf_keys_from_script_node() {
        let node = script_node("lint");
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["lint".to_string()]);
    }

    #[test]
    fn leaf_keys_from_call_workflow_node() {
        let node = WorkflowNode::CallWorkflow(CallWorkflowNode {
            workflow: "child-wf".to_string(),
            inputs: Default::default(),
            retries: 0,
            on_fail: None,
            bot_name: None,
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["workflow:child-wf".to_string()]);
    }

    #[test]
    fn leaf_keys_from_parallel_node() {
        let node = WorkflowNode::Parallel(ParallelNode {
            fail_fast: true,
            min_success: None,
            calls: vec![
                AgentRef::Name("agent_a".to_string()),
                AgentRef::Name("agent_b".to_string()),
            ],
            output: None,
            call_outputs: Default::default(),
            with: vec![],
            call_with: Default::default(),
            call_if: Default::default(),
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["agent_a".to_string(), "agent_b".to_string()]);
    }

    #[test]
    fn leaf_keys_from_gate_node() {
        let node = WorkflowNode::Gate(GateNode {
            name: "human_approval".to_string(),
            gate_type: GateType::HumanApproval,
            prompt: None,
            min_approvals: 1,
            approval_mode: Default::default(),
            timeout_secs: 0,
            on_timeout: crate::dsl::OnTimeout::Fail,
            bot_name: None,
            quality_gate: None,
            options: None,
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["human_approval".to_string()]);
    }

    #[test]
    fn leaf_keys_from_foreach_node() {
        let node = WorkflowNode::ForEach(ForEachNode {
            name: "fan".to_string(),
            over: "tickets".to_string(),
            scope: None,
            filter: Default::default(),
            ordered: false,
            on_cycle: crate::dsl::OnCycle::Fail,
            max_parallel: 4,
            workflow: "child".to_string(),
            inputs: Default::default(),
            on_child_fail: crate::dsl::OnChildFail::Continue,
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["foreach:fan".to_string()]);
    }

    #[test]
    fn leaf_keys_from_if_node_recurses_into_body() {
        let node = WorkflowNode::If(IfNode {
            condition: Condition::BoolInput {
                input: "flag".to_string(),
            },
            body: vec![call_node("inner_agent")],
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["inner_agent".to_string()]);
    }

    #[test]
    fn leaf_keys_from_unless_node_recurses_into_body() {
        let node = WorkflowNode::Unless(UnlessNode {
            condition: Condition::StepMarker {
                step: "s".to_string(),
                marker: "m".to_string(),
            },
            body: vec![call_node("fallback")],
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["fallback".to_string()]);
    }

    #[test]
    fn leaf_keys_from_while_node_recurses_into_body() {
        let node = WorkflowNode::While(WhileNode {
            step: "s".to_string(),
            marker: "m".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![call_node("loop_agent")],
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["loop_agent".to_string()]);
    }

    #[test]
    fn leaf_keys_from_do_node_recurses_into_body() {
        let node = WorkflowNode::Do(DoNode {
            output: None,
            with: vec![],
            body: vec![call_node("step_a"), script_node("step_b")],
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["step_a".to_string(), "step_b".to_string()]);
    }

    #[test]
    fn leaf_keys_from_always_node_recurses_into_body() {
        let node = WorkflowNode::Always(AlwaysNode {
            body: vec![call_node("cleanup")],
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["cleanup".to_string()]);
    }

    #[test]
    fn leaf_keys_empty_body_returns_empty() {
        let node = WorkflowNode::If(IfNode {
            condition: Condition::BoolInput {
                input: "x".to_string(),
            },
            body: vec![],
        });
        let keys = collect_leaf_step_keys(&node);
        assert!(keys.is_empty());
    }

    #[test]
    fn leaf_keys_path_agent_uses_file_stem() {
        let node = WorkflowNode::Call(CallNode {
            agent: AgentRef::Path(".claude/agents/plan.md".to_string()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
            plugin_dirs: vec![],
            timeout: None,
        });
        let keys = collect_leaf_step_keys(&node);
        assert_eq!(keys, vec!["plan".to_string()]);
    }

    // ---------------------------------------------------------------------------
    // parse_duration — all suffix branches and bare-integer fallback
    // ---------------------------------------------------------------------------

    #[test]
    fn parse_duration_milliseconds() {
        assert_eq!(
            parse_duration("250ms").unwrap(),
            std::time::Duration::from_millis(250)
        );
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(
            parse_duration("2h").unwrap(),
            std::time::Duration::from_secs(7200)
        );
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(
            parse_duration("30m").unwrap(),
            std::time::Duration::from_secs(1800)
        );
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(
            parse_duration("10s").unwrap(),
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn parse_duration_bare_integer_treated_as_seconds() {
        assert_eq!(
            parse_duration("5").unwrap(),
            std::time::Duration::from_secs(5)
        );
    }

    #[test]
    fn parse_duration_ms_not_matched_by_s_suffix() {
        // "3ms" must not be misinterpreted as "3m" + trailing "s"
        assert_eq!(
            parse_duration("3ms").unwrap(),
            std::time::Duration::from_millis(3)
        );
    }

    #[test]
    fn parse_duration_invalid_returns_err() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("1x").is_err());
    }

    // ---------------------------------------------------------------------------
    // serialize_or_empty_array — happy path and fallback
    // ---------------------------------------------------------------------------

    #[test]
    fn serialize_or_empty_array_serializes_vec() {
        let result = serialize_or_empty_array(&vec!["a", "b"], "test");
        assert_eq!(result, r#"["a","b"]"#);
    }

    #[test]
    fn serialize_or_empty_array_returns_bracket_pair_on_failure() {
        struct AlwaysFails;
        impl serde::Serialize for AlwaysFails {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }
        let result = serialize_or_empty_array(&AlwaysFails, "test");
        assert_eq!(result, "[]");
    }
}
