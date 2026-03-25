use conductor_core::workflow::{MetadataEntry, WorkflowWarning};

use crate::action::Action;
use crate::state::{FormField, FormFieldType};

/// Maximum scroll offset for a text body (total lines minus one visible line).
pub(super) fn max_scroll(line_count: usize) -> u16 {
    line_count.saturating_sub(1) as u16
}

/// Increment `index` by one, clamping to `len - 1` (no-op when `len` is zero).
pub(super) fn clamp_increment(index: &mut usize, len: usize) {
    let max = len.saturating_sub(1);
    if *index < max {
        *index += 1;
    }
}

/// Increment `index` by one, wrapping back to 0 when reaching `len`.
pub(super) fn wrap_increment(index: &mut usize, len: usize) {
    if *index + 1 < len {
        *index += 1;
    } else {
        *index = 0;
    }
}

/// Decrement `index` by one, wrapping to `len - 1` when at 0.
pub(super) fn wrap_decrement(index: &mut usize, len: usize) {
    if *index > 0 {
        *index -= 1;
    } else {
        *index = len.saturating_sub(1);
    }
}

/// Find the nearest non-readonly field by traversing `fields` from `start`.
///
/// `forward` selects the traversal direction.  Returns `None` when every
/// field is readonly or the slice is empty, meaning no movement is possible.
pub(super) fn advance_form_field(
    fields: &[FormField],
    start: usize,
    forward: bool,
) -> Option<usize> {
    let len = fields.len();
    if len == 0 {
        return None;
    }
    let step = |idx: usize| -> usize {
        if forward {
            (idx + 1) % len
        } else if idx == 0 {
            len - 1
        } else {
            idx - 1
        }
    };
    let mut idx = step(start);
    while idx != start {
        if !fields[idx].readonly {
            return Some(idx);
        }
        idx = step(idx);
    }
    // No non-readonly field found other than start; check start itself.
    if !fields[start].readonly {
        Some(start)
    } else {
        None
    }
}

/// Build a status-bar message for workflow parse warnings, or `None` if there are none.
pub(super) fn workflow_parse_warning_message(warnings: &[WorkflowWarning]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }
    let count = warnings.len();
    let label = warnings
        .iter()
        .map(|w| w.file.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "⚠ {count} workflow file(s) failed to parse: {label}"
    ))
}

/// Format structured [`MetadataEntry`] values into a fixed-width text block
/// suitable for the TUI modal.
pub(super) fn format_metadata_entries(entries: &[MetadataEntry]) -> String {
    let pad = entries
        .iter()
        .filter_map(|e| match e {
            MetadataEntry::Field { label, .. } => Some(label.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let mut parts: Vec<String> = Vec::new();
    for entry in entries {
        match entry {
            MetadataEntry::Field { label, value } => {
                parts.push(format!(
                    "{:<pad$}  {}",
                    format!("{label}:"),
                    value,
                    pad = pad + 1
                ));
            }
            MetadataEntry::Section { heading, body } => {
                parts.push(String::new());
                parts.push(format!("── {heading} ──"));
                parts.push(body.clone());
            }
        }
    }
    parts.join("\n")
}

/// Derive a worktree slug from a ticket's source_id and title.
/// Format: `{source_id}-{slugified-title}`, e.g. `15-tui-create-worktree`.
/// Title portion is truncated to keep the total slug under ~40 chars.
pub(super) fn derive_worktree_slug(source_id: &str, title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive dashes
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }
    let title_slug = collapsed.trim_matches('-');

    // Budget: 40 chars total, minus source_id and separator
    let budget = 40_usize.saturating_sub(source_id.len() + 1);
    let truncated = if title_slug.len() <= budget {
        title_slug
    } else {
        match title_slug[..budget].rfind('-') {
            Some(pos) => &title_slug[..pos],
            None => &title_slug[..budget],
        }
    };

    if truncated.is_empty() {
        source_id.to_string()
    } else {
        format!("{}-{}", source_id, truncated)
    }
}

/// Send a workflow execution result through the background channel.
///
/// Shared by all `spawn_*_workflow_in_background` helpers to avoid
/// duplicating the success/failure dispatch logic.
///
/// When `target` is `Some(t)`, messages read `"Workflow 'X' on {t} completed …"`;
/// when `None`, they read `"Workflow 'X' completed …"`.
pub(super) fn send_workflow_result(
    bg_tx: &Option<crate::event::BackgroundSender>,
    workflow_name: &str,
    target: Option<&str>,
    result: conductor_core::error::Result<conductor_core::workflow::WorkflowResult>,
) {
    if let Some(ref tx) = bg_tx {
        let on_label = target.map(|t| format!(" on {t}")).unwrap_or_default();
        match result {
            Ok(res) => {
                let msg = if res.all_succeeded {
                    format!("Workflow '{workflow_name}'{on_label} completed successfully")
                } else {
                    format!("Workflow '{workflow_name}'{on_label} completed with failures")
                };
                tx.send(Action::BackgroundSuccess { message: msg });
            }
            Err(e) => {
                tx.send(Action::BackgroundError {
                    message: format!("Workflow '{workflow_name}'{on_label} failed: {e}"),
                });
            }
        }
    }
}

/// Build `FormField`s from workflow `InputDecl`s.
pub(super) fn build_form_fields(inputs: &[conductor_core::workflow::InputDecl]) -> Vec<FormField> {
    use conductor_core::workflow::InputType;
    inputs
        .iter()
        .map(|inp| {
            let (value, field_type) = if inp.input_type == InputType::Boolean {
                (
                    inp.default.clone().unwrap_or_else(|| "false".to_string()),
                    FormFieldType::Boolean,
                )
            } else {
                (inp.default.clone().unwrap_or_default(), FormFieldType::Text)
            };
            FormField {
                label: inp.name.clone(),
                value,
                placeholder: if inp.required {
                    "(required)".to_string()
                } else {
                    String::new()
                },
                manually_edited: false,
                required: inp.required,
                readonly: false,
                field_type,
            }
        })
        .collect()
}

/// Filter `steps` to only those from the latest iteration for each `step_name`.
/// Keeps `workflow_step_index` valid since it's an index into the filtered list.
pub(super) fn collapse_loop_iterations(
    mut steps: Vec<conductor_core::workflow::WorkflowRunStep>,
) -> Vec<conductor_core::workflow::WorkflowRunStep> {
    let max_iter = crate::state::max_iter_by_step_name(&steps);
    steps.retain(|s| s.iteration == *max_iter.get(&s.step_name).unwrap_or(&0));
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::workflow::{InputDecl, InputType, MetadataEntry, WorkflowWarning};

    // ── max_scroll ──────────────────────────────────────────────────────────

    #[test]
    fn max_scroll_zero_lines() {
        assert_eq!(max_scroll(0), 0);
    }

    #[test]
    fn max_scroll_one_line() {
        assert_eq!(max_scroll(1), 0);
    }

    #[test]
    fn max_scroll_many_lines() {
        assert_eq!(max_scroll(100), 99);
    }

    // ── workflow_parse_warning_message ───────────────────────────────────────

    #[test]
    fn warning_message_empty() {
        assert!(workflow_parse_warning_message(&[]).is_none());
    }

    #[test]
    fn warning_message_single() {
        let warnings = vec![WorkflowWarning {
            file: "bad.wf".into(),
            message: "syntax error".into(),
        }];
        let msg = workflow_parse_warning_message(&warnings).unwrap();
        assert!(msg.contains("1 workflow file(s)"));
        assert!(msg.contains("bad.wf"));
    }

    #[test]
    fn warning_message_multiple() {
        let warnings = vec![
            WorkflowWarning {
                file: "a.wf".into(),
                message: "err".into(),
            },
            WorkflowWarning {
                file: "b.wf".into(),
                message: "err".into(),
            },
        ];
        let msg = workflow_parse_warning_message(&warnings).unwrap();
        assert!(msg.contains("2 workflow file(s)"));
        assert!(msg.contains("a.wf, b.wf"));
    }

    // ── format_metadata_entries ─────────────────────────────────────────────

    #[test]
    fn format_metadata_field_entries() {
        let entries = vec![
            MetadataEntry::Field {
                label: "Status",
                value: "completed".into(),
            },
            MetadataEntry::Field {
                label: "ID",
                value: "abc123".into(),
            },
        ];
        let result = format_metadata_entries(&entries);
        // "Status:" is padded to align with the longest label
        assert!(result.contains("Status:"));
        assert!(result.contains("completed"));
        assert!(result.contains("ID:"));
        assert!(result.contains("abc123"));
    }

    #[test]
    fn format_metadata_section_entry() {
        let entries = vec![MetadataEntry::Section {
            heading: "Details",
            body: "Some long text here".into(),
        }];
        let result = format_metadata_entries(&entries);
        assert!(result.contains("── Details ──"));
        assert!(result.contains("Some long text here"));
    }

    #[test]
    fn format_metadata_mixed() {
        let entries = vec![
            MetadataEntry::Field {
                label: "Name",
                value: "test".into(),
            },
            MetadataEntry::Section {
                heading: "Body",
                body: "content".into(),
            },
        ];
        let result = format_metadata_entries(&entries);
        assert!(result.contains("Name:"));
        assert!(result.contains("── Body ──"));
    }

    // ── derive_worktree_slug ────────────────────────────────────────────────

    #[test]
    fn derive_slug_normal() {
        let slug = derive_worktree_slug("123", "Add login flow");
        assert_eq!(slug, "123-add-login-flow");
    }

    #[test]
    fn derive_slug_special_chars() {
        let slug = derive_worktree_slug("42", "Fix: null-ptr crash!!");
        assert_eq!(slug, "42-fix-null-ptr-crash");
    }

    #[test]
    fn derive_slug_consecutive_dashes() {
        let slug = derive_worktree_slug("7", "hello---world   test");
        assert_eq!(slug, "7-hello-world-test");
    }

    #[test]
    fn derive_slug_long_title_truncation() {
        let long_title = "a".repeat(100);
        let slug = derive_worktree_slug("99", &long_title);
        // Total should be ≤ 40 chars
        assert!(slug.len() <= 40, "slug too long: {} chars", slug.len());
        assert!(slug.starts_with("99-"));
    }

    #[test]
    fn derive_slug_empty_title() {
        assert_eq!(derive_worktree_slug("123", ""), "123");
    }

    #[test]
    fn derive_slug_all_special_chars() {
        assert_eq!(derive_worktree_slug("42", "!!@@##"), "42");
    }

    #[test]
    fn derive_slug_whitespace_only() {
        assert_eq!(derive_worktree_slug("7", "   "), "7");
    }

    // ── build_form_fields ───────────────────────────────────────────────────

    #[test]
    fn build_form_fields_text_required() {
        let inputs = vec![InputDecl {
            name: "pr_url".into(),
            required: true,
            default: None,
            description: None,
            input_type: InputType::String,
        }];
        let fields = build_form_fields(&inputs);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].label, "pr_url");
        assert_eq!(fields[0].placeholder, "(required)");
        assert!(fields[0].required);
        assert!(fields[0].value.is_empty());
        assert!(matches!(fields[0].field_type, FormFieldType::Text));
    }

    #[test]
    fn build_form_fields_boolean_with_default() {
        let inputs = vec![InputDecl {
            name: "dry_run".into(),
            required: false,
            default: Some("true".into()),
            description: None,
            input_type: InputType::Boolean,
        }];
        let fields = build_form_fields(&inputs);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].value, "true");
        assert!(matches!(fields[0].field_type, FormFieldType::Boolean));
        assert!(!fields[0].required);
    }

    #[test]
    fn build_form_fields_boolean_no_default() {
        let inputs = vec![InputDecl {
            name: "verbose".into(),
            required: false,
            default: None,
            description: None,
            input_type: InputType::Boolean,
        }];
        let fields = build_form_fields(&inputs);
        assert_eq!(fields[0].value, "false");
    }

    #[test]
    fn build_form_fields_text_with_default() {
        let inputs = vec![InputDecl {
            name: "branch".into(),
            required: false,
            default: Some("main".into()),
            description: None,
            input_type: InputType::String,
        }];
        let fields = build_form_fields(&inputs);
        assert_eq!(fields[0].value, "main");
        assert!(fields[0].placeholder.is_empty());
    }
}
