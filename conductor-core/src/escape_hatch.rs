//! Process escape hatch: explicit authorization to bypass safety guards.
//!
//! Provides structured audit logging when users override checks via --force flags.
//! All overrides are recorded via tracing so they appear in logs for accountability.
//!
//! Part of: process-escape-hatch@1.0.0

/// Classification of override risk level.
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)] // Low tier will be used for non-destructive overrides
pub enum OverrideTier {
    /// Self-service: --force flag, low risk of data loss.
    Low,
    /// Higher risk: destructive action or state mutation.
    High,
}

impl std::fmt::Display for OverrideTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::High => write!(f, "high"),
        }
    }
}

/// Structured record of a safety guard being bypassed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OverrideRecord {
    pub timestamp: String,
    pub operation: String,
    pub constraint_bypassed: String,
    pub justification: String,
    pub tier: OverrideTier,
}

/// Log an override event via tracing for audit trail.
pub fn log_override(record: &OverrideRecord) {
    tracing::warn!(
        operation = %record.operation,
        constraint = %record.constraint_bypassed,
        justification = %record.justification,
        tier = %record.tier,
        "escape hatch override activated"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_record_serializable() {
        let record = OverrideRecord {
            timestamp: "2026-03-21T00:00:00Z".to_string(),
            operation: "workflow run".to_string(),
            constraint_bypassed: "WorkflowRunAlreadyActive".to_string(),
            justification: "--force flag".to_string(),
            tier: OverrideTier::High,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("WorkflowRunAlreadyActive"));
    }

    #[test]
    fn override_tier_display() {
        assert_eq!(OverrideTier::Low.to_string(), "low");
        assert_eq!(OverrideTier::High.to_string(), "high");
    }
}
