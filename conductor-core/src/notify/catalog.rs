//! The registry of every event a `[[notify.hooks]]` entry can match.
//!
//! One table ([`EVENTS`]) feeds `conductor notify events`, the generated block in
//! `docs/notification-hooks.md`, the web hook × event matrix, the synthetic test events and
//! the config-load warning for an `on` arm that matches nothing. Drift tests in
//! `notify/tests.rs` build every event through the real producers and compare the kind and
//! field keys with this table, so a new event or field can't ship unlisted.

use serde::Serialize;

pub const WORKFLOW_RUN_COMPLETED: &str = "workflow_run.completed";
pub const WORKFLOW_RUN_FAILED: &str = "workflow_run.failed";
pub const WORKFLOW_RUN_ORPHAN_RESUMED: &str = "workflow_run.orphan_resumed";
pub const WORKFLOW_RUN_REAPED: &str = "workflow_run.reaped";
pub const WORKFLOW_RUN_COST_SPIKE: &str = "workflow_run.cost_spike";
pub const WORKFLOW_RUN_DURATION_SPIKE: &str = "workflow_run.duration_spike";
pub const AGENT_RUN_COMPLETED: &str = "agent_run.completed";
pub const AGENT_RUN_FAILED: &str = "agent_run.failed";
pub const GATE_WAITING: &str = "gate.waiting";
pub const GATE_PENDING_TOO_LONG: &str = "gate.pending_too_long";
pub const FEEDBACK_REQUESTED: &str = "feedback.requested";
pub const SESSION_IDLE: &str = "session.idle";

/// One field of an event, exposed to shell hooks as `RUNKON_NOTIFY_FIELD_<UPPER_NAME>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventField {
    pub name: &'static str,
    pub description: &'static str,
    /// `false` when a variant or condition can leave the field out (the description says which).
    pub always: bool,
}

/// An event a hook can match.
#[derive(Debug, Clone, Copy)]
pub struct EventSpec {
    pub name: &'static str,
    /// Short label for the web hook × event matrix.
    pub label: &'static str,
    /// One line: what fires it.
    pub description: &'static str,
    /// A `workflow_run.*` event (the web matrix offers the `:root` modifier for these).
    pub is_workflow: bool,
    /// Needs a threshold on the hook (`threshold_multiple` or `gate_pending_ms`), so the web
    /// matrix leaves it out.
    pub needs_threshold: bool,
    pub fields: &'static [EventField],
}

impl EventSpec {
    /// True when the event carries `is_root`, which `:root` filters on.
    pub fn supports_root(&self) -> bool {
        self.fields.iter().any(|f| f.name == "is_root")
    }

    /// The threshold setting a hook needs for this event, if any.
    pub fn threshold_setting(&self) -> Option<&'static str> {
        match self.name {
            WORKFLOW_RUN_COST_SPIKE | WORKFLOW_RUN_DURATION_SPIKE => Some("threshold_multiple"),
            GATE_PENDING_TOO_LONG => Some("gate_pending_ms"),
            _ => None,
        }
    }
}

const fn f(name: &'static str, description: &'static str) -> EventField {
    EventField {
        name,
        description,
        always: true,
    }
}

const fn maybe(name: &'static str, description: &'static str) -> EventField {
    EventField {
        name,
        description,
        always: false,
    }
}

const RUN_ID_WORKFLOW: EventField = f("run_id", "Workflow run ID");
const RUN_ID_AGENT: EventField = f("run_id", "Agent run ID");
const WORKFLOW_NAME: EventField = f("workflow_name", "Workflow name");
const PARENT_RUN_ID: EventField = f(
    "parent_workflow_run_id",
    "Parent workflow run ID; empty for root runs",
);
const REPO_SLUG: EventField = f("repo_slug", "Repository slug");
const BRANCH: EventField = f("branch", "Branch name");
const DURATION_MS: EventField = f("duration_ms", "Run duration in ms; empty when unknown");
const TICKET_URL: EventField = f("ticket_url", "Linked ticket URL; empty when none");
const URL: EventField = f("url", "Web UI deep link; empty when `[web] url` is not set");
const TIMESTAMP: EventField = f("timestamp", "RFC 3339 time the event was built");
const IS_ROOT: EventField = f("is_root", "`true` when the run has no parent, else `false`");
const MULTIPLE: EventField = f("multiple", "Multiple over the baseline, e.g. `3.5`");
const STEP_NAME_GATE: EventField = f("step_name", "Gate step name");
const PROMPT_PREVIEW: EventField = f("prompt_preview", "First ~100 chars of the prompt");

const WORKFLOW_COMPLETED_FIELDS: &[EventField] = &[
    RUN_ID_WORKFLOW,
    WORKFLOW_NAME,
    PARENT_RUN_ID,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    TICKET_URL,
    URL,
    TIMESTAMP,
    IS_ROOT,
];

const WORKFLOW_FAILED_FIELDS: &[EventField] = &[
    RUN_ID_WORKFLOW,
    WORKFLOW_NAME,
    PARENT_RUN_ID,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    TICKET_URL,
    URL,
    TIMESTAMP,
    IS_ROOT,
    maybe(
        "cause",
        "Nested failing step, e.g. `workflow:review-pr \u{203a} submit-review: exited 1`; set only when one was found",
    ),
    maybe(
        "error",
        "Error message, ending with ` \u{2014} cause: <cause>` when a cause was found; set only when there is an error or cause",
    ),
];

const ORPHAN_RESUMED_FIELDS: &[EventField] = &[
    f("run_id", "First resumed workflow run ID"),
    WORKFLOW_NAME,
    REPO_SLUG,
    BRANCH,
    TIMESTAMP,
];

const REAPED_FIELDS: &[EventField] = &[
    RUN_ID_WORKFLOW,
    WORKFLOW_NAME,
    REPO_SLUG,
    BRANCH,
    f("error", "Why the run could not be resumed"),
    TIMESTAMP,
];

const COST_SPIKE_FIELDS: &[EventField] = &[
    RUN_ID_WORKFLOW,
    WORKFLOW_NAME,
    PARENT_RUN_ID,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    URL,
    TIMESTAMP,
    MULTIPLE,
    f("cost_usd", "Run cost in USD"),
    IS_ROOT,
];

const DURATION_SPIKE_FIELDS: &[EventField] = &[
    RUN_ID_WORKFLOW,
    WORKFLOW_NAME,
    PARENT_RUN_ID,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    URL,
    TIMESTAMP,
    MULTIPLE,
    IS_ROOT,
];

const AGENT_COMPLETED_FIELDS: &[EventField] = &[
    RUN_ID_AGENT,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    TICKET_URL,
    TIMESTAMP,
];

const AGENT_FAILED_FIELDS: &[EventField] = &[
    RUN_ID_AGENT,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    TICKET_URL,
    TIMESTAMP,
    maybe("error", "Error message; set only when the run recorded one"),
];

const GATE_WAITING_FIELDS: &[EventField] = &[
    f(
        "run_id",
        "Workflow step ID for a single gate, workflow run ID for a grouped notification",
    ),
    f(
        "step_name",
        "Gate step name, or `<n> gates pending` for a grouped notification",
    ),
    maybe(
        "repo_slug",
        "Repository slug; single-gate notifications only",
    ),
    maybe("branch", "Branch name; single-gate notifications only"),
    maybe(
        "ticket_url",
        "Linked ticket URL, empty when none; single-gate notifications only",
    ),
    maybe(
        "count",
        "Number of gates pending in the run; grouped notifications only",
    ),
    TIMESTAMP,
];

const GATE_PENDING_FIELDS: &[EventField] = &[
    RUN_ID_WORKFLOW,
    STEP_NAME_GATE,
    REPO_SLUG,
    BRANCH,
    DURATION_MS,
    URL,
    TIMESTAMP,
    f("pending_ms", "How long the gate has been waiting, in ms"),
];

const FEEDBACK_FIELDS: &[EventField] = &[
    f("run_id", "The agent run that asked"),
    f("feedback_id", "Feedback request ID"),
    PROMPT_PREVIEW,
    f("repo_slug", "Repository slug, resolved from the agent run"),
    f("branch", "Branch name; empty for repo-scoped agents"),
    f(
        "worktree_slug",
        "Worktree slug; empty for repo-scoped agents",
    ),
    TIMESTAMP,
];

const SESSION_IDLE_FIELDS: &[EventField] = &[
    f("run_id", "The agent run"),
    f("session_id", "Claude Code session ID; empty when unknown"),
    REPO_SLUG,
    BRANCH,
    f("worktree_slug", "Worktree slug; empty when none"),
    f("prompt_preview", "First ~100 chars of the turn's prompt"),
    f(
        "last_message_preview",
        "First ~100 chars of the last assistant message",
    ),
    f(
        "background_tasks",
        "Background tasks still running at `Stop` (`0` when none)",
    ),
    f(
        "turn_duration_ms",
        "Prompt to `Stop` in ms; empty when unknown",
    ),
    f("idle_since", "RFC 3339 time of the `Stop`"),
    TIMESTAMP,
];

/// Every event `[[notify.hooks]]` can match, in display order.
pub const EVENTS: &[EventSpec] = &[
    EventSpec {
        name: WORKFLOW_RUN_COMPLETED,
        label: "Workflow completed",
        description: "A workflow run finished successfully",
        is_workflow: true,
        needs_threshold: false,
        fields: WORKFLOW_COMPLETED_FIELDS,
    },
    EventSpec {
        name: WORKFLOW_RUN_FAILED,
        label: "Workflow failed",
        description: "A workflow run failed",
        is_workflow: true,
        needs_threshold: false,
        fields: WORKFLOW_FAILED_FIELDS,
    },
    EventSpec {
        name: WORKFLOW_RUN_REAPED,
        label: "Dead workflow detected",
        description: "A stuck workflow run was reaped and could not be auto-resumed",
        is_workflow: true,
        needs_threshold: false,
        fields: REAPED_FIELDS,
    },
    EventSpec {
        name: WORKFLOW_RUN_ORPHAN_RESUMED,
        label: "Orphaned workflows resumed",
        description: "Orphaned or stuck workflow runs were automatically resumed on startup",
        is_workflow: true,
        needs_threshold: false,
        fields: ORPHAN_RESUMED_FIELDS,
    },
    EventSpec {
        name: WORKFLOW_RUN_COST_SPIKE,
        label: "Workflow cost spike",
        description: "A completed workflow run cost a multiple of its baseline",
        is_workflow: true,
        needs_threshold: true,
        fields: COST_SPIKE_FIELDS,
    },
    EventSpec {
        name: WORKFLOW_RUN_DURATION_SPIKE,
        label: "Workflow duration spike",
        description: "A completed workflow run took a multiple of its baseline duration",
        is_workflow: true,
        needs_threshold: true,
        fields: DURATION_SPIKE_FIELDS,
    },
    EventSpec {
        name: AGENT_RUN_COMPLETED,
        label: "Agent completed",
        description:
            "A standalone agent run finished successfully (not fired for interactive sessions)",
        is_workflow: false,
        needs_threshold: false,
        fields: AGENT_COMPLETED_FIELDS,
    },
    EventSpec {
        name: AGENT_RUN_FAILED,
        label: "Agent failed",
        description: "A standalone agent run failed",
        is_workflow: false,
        needs_threshold: false,
        fields: AGENT_FAILED_FIELDS,
    },
    EventSpec {
        name: GATE_WAITING,
        label: "Gate waiting",
        description: "A workflow gate is waiting for a person (approval, review, CI)",
        is_workflow: false,
        needs_threshold: false,
        fields: GATE_WAITING_FIELDS,
    },
    EventSpec {
        name: GATE_PENDING_TOO_LONG,
        label: "Gate pending too long",
        description: "A gate has been waiting longer than `gate_pending_ms` (default 30 minutes)",
        is_workflow: false,
        needs_threshold: true,
        fields: GATE_PENDING_FIELDS,
    },
    EventSpec {
        name: FEEDBACK_REQUESTED,
        label: "Feedback requested",
        description: "An agent asked for feedback and is blocked on the answer",
        is_workflow: false,
        needs_threshold: false,
        fields: FEEDBACK_FIELDS,
    },
    EventSpec {
        name: SESSION_IDLE,
        label: "Session idle",
        description: "An interactive session finished a turn and has stayed idle for `delay_secs`",
        is_workflow: false,
        needs_threshold: false,
        fields: SESSION_IDLE_FIELDS,
    },
];

/// Look up an event by exact name.
pub fn event_spec(name: &str) -> Option<&'static EventSpec> {
    EVENTS.iter().find(|e| e.name == name)
}

/// The events the web hook × event matrix offers: every one without a threshold.
pub fn matrix_events() -> impl Iterator<Item = &'static EventSpec> {
    EVENTS.iter().filter(|e| !e.needs_threshold)
}

/// Comma-separated list of every event name, for error and warning messages.
pub fn event_names_csv() -> String {
    EVENTS.iter().map(|e| e.name).collect::<Vec<_>>().join(", ")
}

/// The environment variable a shell hook reads a field from.
pub fn field_env_var(field: &str) -> String {
    format!("RUNKON_NOTIFY_FIELD_{}", field.to_uppercase())
}

/// The arms of a comma-separated `on` pattern that match no registered event.
///
/// Uses the matcher dispatch uses, so `*`, `prefix.*`, exact names and a `:root` suffix
/// behave the same. An empty `on` is one unmatched (empty) arm.
pub fn unmatched_on_arms(on: &str) -> Vec<String> {
    on.split(',')
        .map(str::trim)
        .filter(|arm| {
            let pat = arm.strip_suffix(":root").unwrap_or(arm);
            !EVENTS
                .iter()
                .any(|e| runkon_notify::hooks::on_pattern_matches(pat, e.name))
        })
        .map(str::to_string)
        .collect()
}

fn event_notes(e: &EventSpec) -> Vec<String> {
    let mut notes = Vec::new();
    if e.supports_root() {
        notes.push("supports `:root`".to_string());
    }
    if let Some(setting) = e.threshold_setting() {
        notes.push(format!("needs `{setting}` on the hook"));
    }
    notes
}

/// Plain-text listing for `conductor notify events`.
pub fn events_text() -> String {
    let mut out = String::new();
    for e in EVENTS {
        let notes = event_notes(e);
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; ").replace('`', ""))
        };
        out.push_str(&format!("{} \u{2014} {}{}\n", e.name, e.description, notes));
        let width = e
            .fields
            .iter()
            .map(|fl| field_env_var(fl.name).len())
            .max()
            .unwrap_or(0);
        for fl in e.fields {
            let marker = if fl.always { "" } else { " (conditional)" };
            out.push_str(&format!(
                "  {:<width$}  {}{}\n",
                field_env_var(fl.name),
                fl.description,
                marker,
                width = width
            ));
        }
        out.push('\n');
    }
    out.push_str(
        "Every event also sets RUNKON_NOTIFY_KIND, RUNKON_NOTIFY_TITLE, RUNKON_NOTIFY_BODY and\n\
         RUNKON_NOTIFY_SEVERITY. HTTP hooks get the same data as a JSON body with a `fields` map.\n\
         `on` takes a comma-separated list of names, `prefix.*` globs or `*`.\n",
    );
    out
}

/// Serialisable view of the registry for `conductor notify events --json`.
#[derive(Debug, Serialize)]
pub struct EventJson {
    pub name: &'static str,
    pub description: &'static str,
    pub workflow_event: bool,
    pub supports_root: bool,
    pub needs_threshold: bool,
    pub threshold_setting: Option<&'static str>,
    pub fields: Vec<FieldJson>,
}

#[derive(Debug, Serialize)]
pub struct FieldJson {
    pub name: &'static str,
    pub env: String,
    pub description: &'static str,
    pub always: bool,
}

/// The registry as a JSON-serialisable list.
pub fn events_json() -> Vec<EventJson> {
    EVENTS
        .iter()
        .map(|e| EventJson {
            name: e.name,
            description: e.description,
            workflow_event: e.is_workflow,
            supports_root: e.supports_root(),
            needs_threshold: e.needs_threshold,
            threshold_setting: e.threshold_setting(),
            fields: e
                .fields
                .iter()
                .map(|fl| FieldJson {
                    name: fl.name,
                    env: field_env_var(fl.name),
                    description: fl.description,
                    always: fl.always,
                })
                .collect(),
        })
        .collect()
}

/// Marker lines around the generated block in `docs/notification-hooks.md`.
pub const DOCS_BEGIN_MARKER: &str = "<!-- BEGIN GENERATED: notify-events -->";
pub const DOCS_END_MARKER: &str = "<!-- END GENERATED: notify-events -->";

/// The generated per-event field reference for `docs/notification-hooks.md`
/// (the text between the markers, without them).
pub fn events_markdown() -> String {
    let mut out = String::new();
    for e in EVENTS {
        out.push_str(&format!("#### `{}`\n\n{}.", e.name, e.description));
        let notes = event_notes(e);
        if !notes.is_empty() {
            out.push_str(&format!(" ({})", notes.join("; ")));
        }
        out.push_str("\n\n| Variable | Always set | Description |\n|---|---|---|\n");
        for fl in e.fields {
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                field_env_var(fl.name),
                if fl.always { "yes" } else { "no" },
                fl.description
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_unique_and_fields_are_unique_per_event() {
        for (i, e) in EVENTS.iter().enumerate() {
            assert!(
                EVENTS[i + 1..].iter().all(|o| o.name != e.name),
                "{}",
                e.name
            );
            for (j, fl) in e.fields.iter().enumerate() {
                assert!(
                    e.fields[j + 1..].iter().all(|o| o.name != fl.name),
                    "{}.{}",
                    e.name,
                    fl.name
                );
            }
        }
    }

    #[test]
    fn stale_is_not_an_event() {
        assert!(event_spec("workflow_run.stale").is_none());
    }

    #[test]
    fn matrix_events_exclude_threshold_events() {
        let names: Vec<_> = matrix_events().map(|e| e.name).collect();
        assert!(names.contains(&GATE_WAITING));
        assert!(!names.contains(&WORKFLOW_RUN_COST_SPIKE));
        assert!(!names.contains(&GATE_PENDING_TOO_LONG));
    }

    #[test]
    fn field_env_var_uppercases() {
        assert_eq!(field_env_var("step_name"), "RUNKON_NOTIFY_FIELD_STEP_NAME");
    }

    #[test]
    fn unmatched_on_arms_cases() {
        assert!(unmatched_on_arms("*").is_empty());
        assert!(unmatched_on_arms("workflow_run.*").is_empty());
        assert!(unmatched_on_arms("gate.waiting").is_empty());
        assert!(unmatched_on_arms("workflow_run.completed:root").is_empty());
        assert_eq!(
            unmatched_on_arms("gate.waiting, bogus.thing"),
            vec!["bogus.thing".to_string()]
        );
        assert_eq!(
            unmatched_on_arms("worktree.*"),
            vec!["worktree.*".to_string()]
        );
        assert_eq!(
            unmatched_on_arms("workflow_run.stale"),
            vec!["workflow_run.stale".to_string()]
        );
        assert_eq!(unmatched_on_arms(""), vec![String::new()]);
    }

    #[test]
    fn text_and_json_list_every_event_and_field() {
        let text = events_text();
        let json = serde_json::to_value(events_json()).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), EVENTS.len());
        for (e, j) in EVENTS.iter().zip(arr) {
            assert_eq!(j["name"], e.name);
            assert!(text.contains(e.name));
            assert_eq!(j["fields"].as_array().unwrap().len(), e.fields.len());
            for fl in e.fields {
                assert!(text.contains(&field_env_var(fl.name)));
            }
        }
        assert!(text.contains("RUNKON_NOTIFY_KIND"));
    }

    #[test]
    fn docs_event_table_matches_registry() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/notification-hooks.md");
        let doc = std::fs::read_to_string(path).expect("read docs/notification-hooks.md");
        let begin = doc
            .find(DOCS_BEGIN_MARKER)
            .unwrap_or_else(|| panic!("missing {DOCS_BEGIN_MARKER} in docs/notification-hooks.md"));
        let end = doc
            .find(DOCS_END_MARKER)
            .unwrap_or_else(|| panic!("missing {DOCS_END_MARKER} in docs/notification-hooks.md"));
        let block = doc[begin + DOCS_BEGIN_MARKER.len()..end].trim_start_matches('\n');
        let expected = events_markdown();
        assert_eq!(
            block, expected,
            "docs/notification-hooks.md is out of date. Replace the text between the markers \
             with:\n\n{expected}"
        );
    }
}
