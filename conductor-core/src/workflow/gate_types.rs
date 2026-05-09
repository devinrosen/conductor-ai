//! Conductor's open-enum representation of workflow gate types.
//!
//! Wraps the bare strings emitted by `runkon-flow` so conductor code can match
//! exhaustively on the gate types it specializes for, while letting any future
//! gate type that runkon-flow introduces flow through transparently as
//! [`GateType::Other`] — call-sites stay typesafe and DRY without forcing every
//! match arm to grow when a new gate type appears upstream.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum GateType {
    HumanApproval,
    HumanReview,
    PrApproval,
    PrChecks,
    QualityGate,
    /// Any gate type string conductor doesn't specifically distinguish.
    /// Future gate types from runkon-flow land here without breaking call-sites.
    Other(String),
}

const HUMAN_APPROVAL: &str = "human_approval";
const HUMAN_REVIEW: &str = "human_review";
const PR_APPROVAL: &str = "pr_approval";
const PR_CHECKS: &str = "pr_checks";

impl GateType {
    /// Canonical string form. Matches `runkon_flow::dsl::QUALITY_GATE_TYPE`
    /// exactly for the `QualityGate` variant — single source of truth for that
    /// constant lives upstream.
    pub fn as_str(&self) -> &str {
        match self {
            Self::HumanApproval => HUMAN_APPROVAL,
            Self::HumanReview => HUMAN_REVIEW,
            Self::PrApproval => PR_APPROVAL,
            Self::PrChecks => PR_CHECKS,
            Self::QualityGate => runkon_flow::dsl::QUALITY_GATE_TYPE,
            Self::Other(s) => s.as_str(),
        }
    }
}

impl From<&str> for GateType {
    fn from(s: &str) -> Self {
        match s {
            HUMAN_APPROVAL => Self::HumanApproval,
            HUMAN_REVIEW => Self::HumanReview,
            PR_APPROVAL => Self::PrApproval,
            PR_CHECKS => Self::PrChecks,
            s if s == runkon_flow::dsl::QUALITY_GATE_TYPE => Self::QualityGate,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<String> for GateType {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<GateType> for String {
    fn from(g: GateType) -> Self {
        g.to_string()
    }
}

impl fmt::Display for GateType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_strings_roundtrip() {
        let cases = [
            ("human_approval", GateType::HumanApproval),
            ("human_review", GateType::HumanReview),
            ("pr_approval", GateType::PrApproval),
            ("pr_checks", GateType::PrChecks),
            ("quality_gate", GateType::QualityGate),
        ];
        for (s, expected) in cases {
            let parsed = GateType::from(s);
            assert_eq!(parsed, expected, "from({s:?})");
            assert_eq!(parsed.as_str(), s, "as_str({expected:?})");
            assert_eq!(parsed.to_string(), s, "Display({expected:?})");
        }
    }

    #[test]
    fn unknown_string_lands_in_other() {
        let parsed = GateType::from("future_kind_of_gate");
        assert_eq!(parsed, GateType::Other("future_kind_of_gate".to_string()));
        assert_eq!(parsed.as_str(), "future_kind_of_gate");
        assert_eq!(parsed.to_string(), "future_kind_of_gate");
    }

    #[test]
    fn from_owned_string_matches_from_str() {
        assert_eq!(
            GateType::from(String::from("human_approval")),
            GateType::HumanApproval,
        );
        assert_eq!(
            GateType::from(String::from("custom_x")),
            GateType::Other("custom_x".to_string()),
        );
    }

    #[test]
    fn quality_gate_uses_runkon_flow_constant() {
        assert_eq!(
            GateType::QualityGate.as_str(),
            runkon_flow::dsl::QUALITY_GATE_TYPE,
        );
        assert_eq!(
            GateType::from(runkon_flow::dsl::QUALITY_GATE_TYPE),
            GateType::QualityGate,
        );
    }

    #[test]
    fn serde_roundtrip_via_string() {
        let val = GateType::HumanApproval;
        let json = serde_json::to_string(&val).unwrap();
        assert_eq!(json, "\"human_approval\"");
        let back: GateType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, val);

        let other = GateType::Other("custom_gate".to_string());
        let json = serde_json::to_string(&other).unwrap();
        assert_eq!(json, "\"custom_gate\"");
        let back: GateType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, other);
    }
}
