use runkon_flow::dsl::QUALITY_GATE_TYPE;

/// Whether a step is a gate. Rows written before runkon-flow 0.8.0 (conductor
/// < 0.18.0) have NULL `gate_type`, so a `gate` role also counts. Quality gates complete immediately and
/// never wait, so they are excluded when the type is unset.
///
/// Rust counterpart of [`gate_step_sql`]; keep the two in sync.
pub fn is_gate_step(step: &super::WorkflowRunStep) -> bool {
    step.gate_type.is_some() || (step.role == "gate" && step.step_name != QUALITY_GATE_TYPE)
}

/// Whether a step is a gate that is currently waiting for approval.
pub fn is_waiting_gate(step: &super::WorkflowRunStep) -> bool {
    step.status == super::WorkflowStepStatus::Waiting && is_gate_step(step)
}

/// The step's gate type, falling back to the step name (the DSL guarantees they
/// match) when `gate_type` was never recorded (rows written before runkon-flow
/// 0.8.0 / conductor 0.18.0).
pub fn gate_type_or_name(step: &super::WorkflowRunStep) -> &str {
    step.gate_type.as_deref().unwrap_or(step.step_name.as_str())
}

/// SQL predicate matching gate rows for a `workflow_run_steps` table aliased as
/// `alias`. SQL counterpart of [`is_gate_step`].
pub(crate) fn gate_step_sql(alias: &str) -> String {
    format!(
        "({alias}.gate_type IS NOT NULL OR ({alias}.role = 'gate' AND {alias}.step_name != '{QUALITY_GATE_TYPE}'))"
    )
}

/// Fill a missing `gate_type` from the step name (the DSL guarantees they match).
pub(crate) fn backfill_gate_type(mut step: super::WorkflowRunStep) -> super::WorkflowRunStep {
    if step.gate_type.is_none() && is_gate_step(&step) {
        step.gate_type = Some(gate_type_or_name(&step).to_string());
    }
    step
}

/// Parse a gate timeout string (e.g. `"1h"`, `"30m"`) into seconds.
///
/// Wraps `runkon_flow::dsl::parse_duration_str` so callers outside the bridge
/// layer do not import runkon-flow directly.
pub(crate) fn parse_gate_timeout_secs(s: &str) -> Option<i64> {
    match runkon_flow::dsl::parse_duration_str(s) {
        Ok(n) => i64::try_from(n).ok(),
        Err(_) => None,
    }
}

/// Format a list of gate selections into a human-readable context string.
pub fn format_gate_selection_context(items: &[String]) -> String {
    let mut out = String::from("User selected the following items:\n");
    for item in items {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
    out
}

/// Serialize an optional slice of gate selection strings to a JSON string.
///
/// Returns `Ok(None)` when `selections` is `None`, `Ok(Some(json))` on success,
/// or `Err(ConductorError::Workflow(...))` if serialization fails (should never
/// happen for `Vec<String>` but we propagate rather than panic).
pub(crate) fn serialize_gate_selections(
    selections: Option<&[String]>,
) -> crate::error::Result<Option<String>> {
    match selections {
        None => Ok(None),
        Some(s) => serde_json::to_string(s).map(Some).map_err(|e| {
            crate::error::ConductorError::Workflow(format!(
                "gate selections serialization failed: {e}"
            ))
        }),
    }
}

/// Parse a gate's stored `gate_options` JSON blob into a list of option strings.
///
/// The stored format is a JSON array of objects with a `"value"` key:
/// `[{"value": "opt1"}, {"value": "opt2"}]`
pub fn parse_gate_options(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .ok()
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.get("value").and_then(|s| s.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{WorkflowRunStep, WorkflowStepStatus};

    fn step(name: &str, role: &str, gate_type: Option<&str>) -> WorkflowRunStep {
        WorkflowRunStep {
            step_name: name.into(),
            role: role.into(),
            gate_type: gate_type.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn is_gate_step_cases() {
        assert!(is_gate_step(&step("a", "reviewer", Some("human_approval"))));
        assert!(is_gate_step(&step("human_approval", "gate", None)));
        assert!(!is_gate_step(&step("quality_gate", "gate", None)));
        assert!(!is_gate_step(&step("x", "actor", None)));
    }

    #[test]
    fn backfill_gate_type_cases() {
        let b = backfill_gate_type(step("human_approval", "gate", None));
        assert_eq!(b.gate_type.as_deref(), Some("human_approval"));
        let b = backfill_gate_type(step("n", "gate", Some("human_review")));
        assert_eq!(b.gate_type.as_deref(), Some("human_review"));
        assert_eq!(
            backfill_gate_type(step("quality_gate", "gate", None)).gate_type,
            None
        );
        assert_eq!(backfill_gate_type(step("x", "actor", None)).gate_type, None);
    }

    #[test]
    fn is_waiting_gate_requires_waiting_status() {
        let mut s = step("human_approval", "gate", None);
        s.status = WorkflowStepStatus::Waiting;
        assert!(is_waiting_gate(&s));
        s.status = WorkflowStepStatus::Completed;
        assert!(!is_waiting_gate(&s));
    }

    #[test]
    fn gate_type_or_name_falls_back_to_name() {
        assert_eq!(gate_type_or_name(&step("n", "gate", None)), "n");
        assert_eq!(gate_type_or_name(&step("n", "gate", Some("t"))), "t");
    }

    #[test]
    fn parse_gate_options_valid_array_returns_values() {
        let json = r#"[{"value": "opt1"}, {"value": "opt2"}, {"value": "opt3"}]"#;
        let result = parse_gate_options(json);
        assert_eq!(result, vec!["opt1", "opt2", "opt3"]);
    }

    #[test]
    fn parse_gate_options_invalid_json_returns_empty() {
        let result = parse_gate_options("not valid json at all");
        assert!(result.is_empty(), "invalid JSON should yield empty Vec");
    }

    #[test]
    fn parse_gate_options_drops_objects_missing_value_key() {
        let json = r#"[{"value": "keep"}, {"label": "no-value"}, {"value": "also-keep"}]"#;
        let result = parse_gate_options(json);
        assert_eq!(result, vec!["keep", "also-keep"]);
    }

    #[test]
    fn parse_gate_options_empty_array_returns_empty() {
        let result = parse_gate_options("[]");
        assert!(result.is_empty(), "empty array should yield empty Vec");
    }

    #[test]
    fn parse_gate_options_extra_fields_ignored() {
        let json = r#"[{"value": "a", "label": "A label", "extra": 42}]"#;
        let result = parse_gate_options(json);
        assert_eq!(result, vec!["a"]);
    }

    // ── format_gate_selection_context ────────────────────────────────────────

    #[test]
    fn format_gate_selection_context_empty_input() {
        let result = format_gate_selection_context(&[]);
        assert_eq!(result, "User selected the following items:\n");
    }

    #[test]
    fn format_gate_selection_context_single_item() {
        let result = format_gate_selection_context(&["alpha".to_string()]);
        assert_eq!(result, "User selected the following items:\n- alpha\n");
    }

    #[test]
    fn format_gate_selection_context_item_with_special_characters() {
        let result = format_gate_selection_context(&["say \"hello\"\nworld".to_string()]);
        assert_eq!(
            result,
            "User selected the following items:\n- say \"hello\"\nworld\n"
        );
    }

    #[test]
    fn format_gate_selection_context_multi_item_golden() {
        let items = vec!["foo".to_string(), "bar".to_string(), "baz qux".to_string()];
        let result = format_gate_selection_context(&items);
        assert_eq!(
            result,
            "User selected the following items:\n- foo\n- bar\n- baz qux\n"
        );
    }
}
