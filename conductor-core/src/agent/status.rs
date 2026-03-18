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
}
