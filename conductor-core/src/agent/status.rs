use serde::{Deserialize, Serialize};

/// Default error message used when the agent reports an error without a message.
pub const DEFAULT_AGENT_ERROR_MSG: &str = "Claude reported an error";

/// Protocol marker that agents emit to request human feedback.
pub const FEEDBACK_MARKER: &str = "[NEEDS_FEEDBACK] ";

/// Maximum allowed length (in bytes) for feedback prompts and responses.
pub const FEEDBACK_MAX_LEN: usize = 10_240; // 10 KB

/// If `text` is a feedback request line, return the prompt portion.
pub fn parse_feedback_marker(text: &str) -> Option<&str> {
    text.strip_prefix(FEEDBACK_MARKER)
}

/// Parsed result of a `[NEEDS_FEEDBACK]` marker that may contain structured JSON.
#[derive(Debug, Clone)]
pub struct ParsedFeedbackMarker {
    pub prompt: String,
    pub feedback_type: FeedbackType,
    pub options: Option<Vec<super::types::FeedbackOption>>,
    pub timeout_secs: Option<i64>,
}

/// Parse a `[NEEDS_FEEDBACK]` line into a structured result.
///
/// If the text after the marker is valid JSON with a `type` field, it is treated
/// as a structured feedback request. Otherwise it is treated as plain text
/// (backward-compatible).
pub fn parse_feedback_marker_structured(text: &str) -> Option<ParsedFeedbackMarker> {
    let payload = text.strip_prefix(FEEDBACK_MARKER)?;
    let trimmed = payload.trim();

    // Try to parse as structured JSON
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(type_str) = v.get("type").and_then(|t| t.as_str()) {
                let feedback_type = type_str.parse::<FeedbackType>().unwrap_or_default();
                let prompt = v
                    .get("prompt")
                    .and_then(|p| p.as_str())
                    .unwrap_or(payload)
                    .to_string();
                let options: Option<Vec<super::types::FeedbackOption>> = v
                    .get("options")
                    .and_then(|o| serde_json::from_value(o.clone()).ok());
                let timeout_secs = v.get("timeout_secs").and_then(|t| t.as_i64());
                return Some(ParsedFeedbackMarker {
                    prompt,
                    feedback_type,
                    options,
                    timeout_secs,
                });
            }
        }
    }

    // Plain text fallback
    Some(ParsedFeedbackMarker {
        prompt: payload.to_string(),
        feedback_type: FeedbackType::Text,
        options: None,
        timeout_secs: None,
    })
}

/// Truncate a string to at most `max_bytes` bytes, ensuring the cut falls on a
/// valid UTF-8 character boundary (avoids panics on multi-byte characters).
pub(crate) fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Status of an agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Running,
    WaitingForFeedback,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for AgentRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Running => "running",
            Self::WaitingForFeedback => "waiting_for_feedback",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for AgentRunStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "waiting_for_feedback" => Ok(Self::WaitingForFeedback),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("unknown AgentRunStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(AgentRunStatus);

/// Status of a human-in-the-loop feedback request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackStatus {
    Pending,
    Responded,
    Dismissed,
}

impl std::fmt::Display for FeedbackStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Responded => "responded",
            Self::Dismissed => "dismissed",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for FeedbackStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "responded" => Ok(Self::Responded),
            "dismissed" => Ok(Self::Dismissed),
            _ => Err(format!("unknown FeedbackStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(FeedbackStatus);

/// Type of feedback being requested from the user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackType {
    /// Free-form text input (default).
    #[default]
    Text,
    /// Yes/No confirmation.
    Confirm,
    /// Pick exactly one option from a list.
    SingleSelect,
    /// Pick one or more options from a list.
    MultiSelect,
}

impl std::fmt::Display for FeedbackType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Text => "text",
            Self::Confirm => "confirm",
            Self::SingleSelect => "single_select",
            Self::MultiSelect => "multi_select",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for FeedbackType {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "text" => Ok(Self::Text),
            "confirm" => Ok(Self::Confirm),
            "single_select" => Ok(Self::SingleSelect),
            "multi_select" => Ok(Self::MultiSelect),
            _ => Err(format!("unknown FeedbackType: {s}")),
        }
    }
}

crate::impl_sql_enum!(FeedbackType);

/// Status of a single plan step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for StepStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("unknown StepStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(StepStatus);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_within_limit() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
    }

    #[test]
    fn ascii_exact_limit() {
        assert_eq!(truncate_utf8("hello", 5), "hello");
    }

    #[test]
    fn ascii_over_limit() {
        assert_eq!(truncate_utf8("hello world", 5), "hello");
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_utf8("", 5), "");
        assert_eq!(truncate_utf8("", 0), "");
    }

    #[test]
    fn max_bytes_zero() {
        assert_eq!(truncate_utf8("hello", 0), "");
        assert_eq!(truncate_utf8("é", 0), "");
    }

    #[test]
    fn two_byte_char_boundary() {
        // 'é' is 2 bytes (0xC3 0xA9), "aé" is 3 bytes
        let s = "aé";
        assert_eq!(s.len(), 3);
        // Limit 3: fits entirely
        assert_eq!(truncate_utf8(s, 3), "aé");
        // Limit 2: would split 'é', must back up to 1
        assert_eq!(truncate_utf8(s, 2), "a");
        // Limit 1: just 'a'
        assert_eq!(truncate_utf8(s, 1), "a");
    }

    #[test]
    fn three_byte_char_boundary() {
        // '€' is 3 bytes (0xE2 0x82 0xAC), "a€" is 4 bytes
        let s = "a€";
        assert_eq!(s.len(), 4);
        assert_eq!(truncate_utf8(s, 4), "a€");
        // Limit 3: splits '€', back up to 1
        assert_eq!(truncate_utf8(s, 3), "a");
        assert_eq!(truncate_utf8(s, 2), "a");
        assert_eq!(truncate_utf8(s, 1), "a");
    }

    #[test]
    fn four_byte_char_boundary() {
        // '🦀' is 4 bytes, "a🦀" is 5 bytes
        let s = "a🦀";
        assert_eq!(s.len(), 5);
        assert_eq!(truncate_utf8(s, 5), "a🦀");
        // Limits 2-4: all split '🦀', back up to 1
        assert_eq!(truncate_utf8(s, 4), "a");
        assert_eq!(truncate_utf8(s, 3), "a");
        assert_eq!(truncate_utf8(s, 2), "a");
    }

    #[test]
    fn all_multibyte_string() {
        // "ééé" = 6 bytes (each 'é' is 2 bytes)
        let s = "ééé";
        assert_eq!(s.len(), 6);
        assert_eq!(truncate_utf8(s, 6), "ééé");
        assert_eq!(truncate_utf8(s, 5), "éé");
        assert_eq!(truncate_utf8(s, 4), "éé");
        assert_eq!(truncate_utf8(s, 3), "é");
        assert_eq!(truncate_utf8(s, 2), "é");
        assert_eq!(truncate_utf8(s, 1), "");
    }

    #[test]
    fn large_string_sanity() {
        let s = "a".repeat(1000) + "🦀";
        assert_eq!(s.len(), 1004);
        assert_eq!(truncate_utf8(&s, 1004), s.as_str());
        assert_eq!(truncate_utf8(&s, 1003), &s[..1000]);
        assert_eq!(truncate_utf8(&s, 1000), &s[..1000]);
        assert_eq!(truncate_utf8(&s, 500), &s[..500]);
    }

    #[test]
    fn parse_structured_plain_text() {
        let parsed = parse_feedback_marker_structured("[NEEDS_FEEDBACK] What should I do?");
        let parsed = parsed.unwrap();
        assert_eq!(parsed.prompt, "What should I do?");
        assert_eq!(parsed.feedback_type, FeedbackType::Text);
        assert!(parsed.options.is_none());
        assert!(parsed.timeout_secs.is_none());
    }

    #[test]
    fn parse_structured_confirm_json() {
        let input = r#"[NEEDS_FEEDBACK] {"type":"confirm","prompt":"Create this issue?"}"#;
        let parsed = parse_feedback_marker_structured(input).unwrap();
        assert_eq!(parsed.prompt, "Create this issue?");
        assert_eq!(parsed.feedback_type, FeedbackType::Confirm);
        assert!(parsed.options.is_none());
    }

    #[test]
    fn parse_structured_single_select_json() {
        let input = r#"[NEEDS_FEEDBACK] {"type":"single_select","prompt":"Pick priority","options":[{"value":"p0","label":"P0"},{"value":"p1","label":"P1"}],"timeout_secs":60}"#;
        let parsed = parse_feedback_marker_structured(input).unwrap();
        assert_eq!(parsed.prompt, "Pick priority");
        assert_eq!(parsed.feedback_type, FeedbackType::SingleSelect);
        let opts = parsed.options.unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].value, "p0");
        assert_eq!(opts[1].label, "P1");
        assert_eq!(parsed.timeout_secs, Some(60));
    }

    #[test]
    fn parse_structured_invalid_json_falls_back() {
        let input = "[NEEDS_FEEDBACK] {invalid json here}";
        let parsed = parse_feedback_marker_structured(input).unwrap();
        assert_eq!(parsed.prompt, "{invalid json here}");
        assert_eq!(parsed.feedback_type, FeedbackType::Text);
    }

    #[test]
    fn parse_structured_json_without_type_falls_back() {
        let input = r#"[NEEDS_FEEDBACK] {"prompt":"no type field"}"#;
        let parsed = parse_feedback_marker_structured(input).unwrap();
        // No `type` field → treated as plain text
        assert_eq!(parsed.prompt, r#"{"prompt":"no type field"}"#);
        assert_eq!(parsed.feedback_type, FeedbackType::Text);
    }

    #[test]
    fn parse_structured_no_marker() {
        assert!(parse_feedback_marker_structured("plain text").is_none());
    }
}
