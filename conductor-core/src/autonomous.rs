//! Autonomous execution patterns: context guards and supervised autonomy.
//!
//! Provides token usage tracking to prevent context window exhaustion,
//! and configurable autonomy levels that control how much human oversight
//! is required at workflow checkpoints.
//!
//! Covers patterns:
//! - context-window-exhaustion-guard@1.0.0
//! - supervised-autonomy-model@1.0.0

use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pattern 11: context-window-exhaustion-guard@1.0.0
// ---------------------------------------------------------------------------

/// How frequently the context guard checks token usage.
///
/// Builds on Wave 1's checkpoint-persistence-protocol to ensure
/// state is saved when the guard triggers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CheckFrequency {
    /// Check at every workflow phase transition.
    EveryPhaseTransition,
    /// Check at every step boundary.
    EveryStepBoundary,
    /// Check every N steps.
    EveryNSteps(u32),
}

impl fmt::Display for CheckFrequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckFrequency::EveryPhaseTransition => write!(f, "every_phase_transition"),
            CheckFrequency::EveryStepBoundary => write!(f, "every_step_boundary"),
            CheckFrequency::EveryNSteps(n) => write!(f, "every_{n}_steps"),
        }
    }
}

/// Configuration for the context window exhaustion guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextGuardConfig {
    /// Remaining capacity threshold percentage (0-100).
    /// When remaining tokens drop below this %, the guard triggers.
    /// Default: 15 (trigger when less than 15% remains).
    pub threshold_pct: u32,
    /// Maximum context window size in tokens. If None, uses model default.
    pub max_context_tokens: Option<u64>,
    /// How often to check token usage.
    pub check_frequency: CheckFrequency,
}

impl Default for ContextGuardConfig {
    fn default() -> Self {
        Self {
            threshold_pct: 15,
            max_context_tokens: None,
            check_frequency: CheckFrequency::EveryStepBoundary,
        }
    }
}

/// Budget tracking for context window usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Maximum tokens available.
    pub max_tokens: u64,
    /// Tokens used so far (estimated, accumulated across steps).
    pub tokens_used: u64,
    /// Threshold percentage from config.
    pub threshold_pct: u32,
}

impl ContextBudget {
    /// Create a new budget from guard config.
    pub fn from_config(config: &ContextGuardConfig, model_default_tokens: u64) -> Self {
        Self {
            max_tokens: config.max_context_tokens.unwrap_or(model_default_tokens),
            tokens_used: 0,
            threshold_pct: config.threshold_pct,
        }
    }

    /// Record token usage from a completed step.
    pub fn record_usage(&mut self, tokens: u64) {
        self.tokens_used = self.tokens_used.saturating_add(tokens);
    }

    /// Remaining tokens available.
    pub fn tokens_remaining(&self) -> u64 {
        self.max_tokens.saturating_sub(self.tokens_used)
    }

    /// Remaining capacity as a percentage (0-100).
    pub fn remaining_pct(&self) -> u32 {
        if self.max_tokens == 0 {
            return 0;
        }
        ((self.tokens_remaining() as f64 / self.max_tokens as f64) * 100.0) as u32
    }

    /// Whether the guard should trigger (remaining capacity below threshold).
    pub fn should_trigger(&self) -> bool {
        self.remaining_pct() < self.threshold_pct
    }
}

/// Reason a workflow was blocked by the context guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextExhaustionInfo {
    /// Tokens used at time of trigger.
    pub tokens_used: u64,
    /// Remaining capacity percentage at trigger.
    pub tokens_remaining_pct: u32,
    /// Name of the last step that completed before the guard fired.
    pub last_completed_step: String,
    /// Summary of checkpoint state for handoff.
    pub checkpoint_summary: String,
}

/// The context guard monitors token usage and triggers workflow transitions.
///
/// At each check point (determined by `check_frequency`), the guard
/// evaluates whether the context window is nearing exhaustion. If so,
/// it signals the engine to checkpoint and transition to a waiting state.
#[derive(Debug, Clone)]
pub struct ContextGuard {
    pub config: ContextGuardConfig,
    pub budget: ContextBudget,
    steps_since_last_check: u32,
}

impl ContextGuard {
    /// Create a new guard from config, using a model default for max tokens.
    pub fn new(config: ContextGuardConfig, model_default_tokens: u64) -> Self {
        let budget = ContextBudget::from_config(&config, model_default_tokens);
        Self {
            config,
            budget,
            steps_since_last_check: 0,
        }
    }

    /// Record token usage from a completed step and check whether to trigger.
    ///
    /// Returns Some(info) if the guard triggers, None otherwise.
    pub fn record_and_check(
        &mut self,
        tokens: u64,
        step_name: &str,
    ) -> Option<ContextExhaustionInfo> {
        self.budget.record_usage(tokens);
        self.steps_since_last_check += 1;

        if !self.should_check() {
            return None;
        }

        self.steps_since_last_check = 0;

        if self.budget.should_trigger() {
            Some(ContextExhaustionInfo {
                tokens_used: self.budget.tokens_used,
                tokens_remaining_pct: self.budget.remaining_pct(),
                last_completed_step: step_name.to_string(),
                checkpoint_summary: format!(
                    "Context guard triggered: {}% remaining ({}/{} tokens used)",
                    self.budget.remaining_pct(),
                    self.budget.tokens_used,
                    self.budget.max_tokens,
                ),
            })
        } else {
            None
        }
    }

    /// Whether a check should be performed based on frequency config.
    fn should_check(&self) -> bool {
        match &self.config.check_frequency {
            CheckFrequency::EveryPhaseTransition => true,
            CheckFrequency::EveryStepBoundary => true,
            CheckFrequency::EveryNSteps(n) => self.steps_since_last_check >= *n,
        }
    }
}

// ---------------------------------------------------------------------------
// Pattern 12: supervised-autonomy-model@1.0.0
// ---------------------------------------------------------------------------

/// Autonomy level controlling the governance posture of workflow execution.
///
/// Builds on Wave 2's human checkpoint protocol (W2-T16) and
/// Wave 1's RetryConfig for per-item bounds enforcement.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AutonomyLevel {
    /// Every gate pauses for human input.
    FullySupervised,
    /// Intrinsic gates pause; extrinsic auto-proceed with logging.
    #[default]
    Supervised,
    /// Only intrinsic gates pause; most quality gates auto-proceed.
    SemiAutonomous,
    /// No gates pause; all decisions automated (highest risk).
    FullyAutonomous,
}

impl fmt::Display for AutonomyLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AutonomyLevel::FullySupervised => write!(f, "fully_supervised"),
            AutonomyLevel::Supervised => write!(f, "supervised"),
            AutonomyLevel::SemiAutonomous => write!(f, "semi_autonomous"),
            AutonomyLevel::FullyAutonomous => write!(f, "fully_autonomous"),
        }
    }
}

/// Triggers that always require a pause regardless of autonomy level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IntrinsicTrigger {
    /// A blocker requires human resolution.
    BlockerResolution,
    /// A verification produced a verdict requiring review.
    VerificationVerdict,
    /// A threshold-based gate failed.
    ThresholdFailure,
    /// A security-sensitive review is needed.
    SecurityReview,
}

impl fmt::Display for IntrinsicTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntrinsicTrigger::BlockerResolution => write!(f, "blocker_resolution"),
            IntrinsicTrigger::VerificationVerdict => write!(f, "verification_verdict"),
            IntrinsicTrigger::ThresholdFailure => write!(f, "threshold_failure"),
            IntrinsicTrigger::SecurityReview => write!(f, "security_review"),
        }
    }
}

/// Classification of a checkpoint as intrinsic (always pause) or
/// extrinsic (may auto-proceed at higher autonomy levels).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CheckpointClassification {
    /// Always pause, regardless of autonomy level.
    Intrinsic,
    /// May auto-proceed at SemiAutonomous or FullyAutonomous levels.
    Extrinsic,
}

impl fmt::Display for CheckpointClassification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointClassification::Intrinsic => write!(f, "intrinsic"),
            CheckpointClassification::Extrinsic => write!(f, "extrinsic"),
        }
    }
}

/// Configuration for the supervised autonomy model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyConfig {
    /// The autonomy level.
    pub level: AutonomyLevel,
    /// Max retries per step before escalation.
    pub per_item_bound: u32,
    /// Max steps per workflow run before a mandatory pause.
    pub per_session_bound: u32,
    /// Delegates to ContextGuardConfig threshold.
    pub capacity_threshold_pct: u32,
    /// Triggers that always require a pause.
    pub intrinsic_triggers: Vec<IntrinsicTrigger>,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            level: AutonomyLevel::Supervised,
            per_item_bound: 3,
            per_session_bound: 50,
            capacity_threshold_pct: 15,
            intrinsic_triggers: vec![
                IntrinsicTrigger::BlockerResolution,
                IntrinsicTrigger::SecurityReview,
            ],
        }
    }
}

/// Policy that maps checkpoint characteristics to autonomy decisions.
#[derive(Debug, Clone)]
pub struct AutonomyPolicy {
    pub config: AutonomyConfig,
}

impl AutonomyPolicy {
    pub fn new(config: AutonomyConfig) -> Self {
        Self { config }
    }

    /// Determine whether a checkpoint should pause execution.
    ///
    /// Returns true if the checkpoint requires a human pause.
    pub fn should_pause(
        &self,
        classification: &CheckpointClassification,
        trigger: Option<&IntrinsicTrigger>,
    ) -> bool {
        match classification {
            CheckpointClassification::Intrinsic => {
                // Intrinsic checkpoints always pause, unless FullyAutonomous
                // (even then, certain triggers still pause)
                match self.config.level {
                    AutonomyLevel::FullyAutonomous => {
                        // Only pause for explicitly listed intrinsic triggers
                        trigger
                            .map(|t| self.config.intrinsic_triggers.contains(t))
                            .unwrap_or(false)
                    }
                    _ => true,
                }
            }
            CheckpointClassification::Extrinsic => match self.config.level {
                AutonomyLevel::FullySupervised | AutonomyLevel::Supervised => true,
                AutonomyLevel::SemiAutonomous | AutonomyLevel::FullyAutonomous => false,
            },
        }
    }

    /// Check whether the per-item retry bound has been exceeded.
    pub fn exceeds_item_bound(&self, retry_count: u32) -> bool {
        retry_count >= self.config.per_item_bound
    }

    /// Check whether the per-session step bound has been exceeded.
    pub fn exceeds_session_bound(&self, step_count: u32) -> bool {
        step_count >= self.config.per_session_bound
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pattern 11: Context Guard tests --

    #[test]
    fn context_budget_tracks_usage() {
        let config = ContextGuardConfig {
            threshold_pct: 15,
            max_context_tokens: Some(100_000),
            check_frequency: CheckFrequency::EveryStepBoundary,
        };
        let mut budget = ContextBudget::from_config(&config, 200_000);

        assert_eq!(budget.max_tokens, 100_000); // uses explicit config
        assert_eq!(budget.tokens_used, 0);
        assert_eq!(budget.remaining_pct(), 100);

        budget.record_usage(50_000);
        assert_eq!(budget.tokens_remaining(), 50_000);
        assert_eq!(budget.remaining_pct(), 50);
        assert!(!budget.should_trigger());

        budget.record_usage(40_000);
        assert_eq!(budget.remaining_pct(), 10);
        assert!(budget.should_trigger()); // 10% < 15% threshold
    }

    #[test]
    fn context_budget_uses_model_default_when_no_explicit_max() {
        let config = ContextGuardConfig {
            threshold_pct: 20,
            max_context_tokens: None,
            check_frequency: CheckFrequency::EveryStepBoundary,
        };
        let budget = ContextBudget::from_config(&config, 200_000);
        assert_eq!(budget.max_tokens, 200_000);
    }

    #[test]
    fn context_guard_triggers_at_threshold() {
        let config = ContextGuardConfig {
            threshold_pct: 15,
            max_context_tokens: Some(100),
            check_frequency: CheckFrequency::EveryStepBoundary,
        };
        let mut guard = ContextGuard::new(config, 100);

        // 80 tokens used -> 20% remaining -> no trigger
        let result = guard.record_and_check(80, "step_1");
        assert!(result.is_none());

        // 10 more -> 90 total -> 10% remaining -> trigger
        let result = guard.record_and_check(10, "step_2");
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.tokens_used, 90);
        assert_eq!(info.last_completed_step, "step_2");
    }

    #[test]
    fn context_guard_does_not_trigger_above_threshold() {
        let config = ContextGuardConfig {
            threshold_pct: 15,
            max_context_tokens: Some(1000),
            check_frequency: CheckFrequency::EveryStepBoundary,
        };
        let mut guard = ContextGuard::new(config, 1000);

        let result = guard.record_and_check(500, "step_1");
        assert!(result.is_none()); // 50% remaining
    }

    #[test]
    fn context_guard_respects_every_n_steps_frequency() {
        let config = ContextGuardConfig {
            threshold_pct: 50,
            max_context_tokens: Some(100),
            check_frequency: CheckFrequency::EveryNSteps(3),
        };
        let mut guard = ContextGuard::new(config, 100);

        // Use 90 tokens in first step, but check_frequency is every 3 steps
        let r1 = guard.record_and_check(90, "step_1");
        assert!(r1.is_none()); // Only 1 step, need 3

        let r2 = guard.record_and_check(0, "step_2");
        assert!(r2.is_none()); // Only 2 steps

        // Third step triggers the check, and 90% usage > 50% threshold
        let r3 = guard.record_and_check(0, "step_3");
        assert!(r3.is_some());
    }

    // -- Pattern 12: Supervised Autonomy tests --

    #[test]
    fn intrinsic_checkpoint_always_pauses_at_supervised() {
        let policy = AutonomyPolicy::new(AutonomyConfig {
            level: AutonomyLevel::Supervised,
            ..Default::default()
        });

        assert!(policy.should_pause(&CheckpointClassification::Intrinsic, None));
    }

    #[test]
    fn extrinsic_checkpoint_auto_proceeds_at_semi_autonomous() {
        let policy = AutonomyPolicy::new(AutonomyConfig {
            level: AutonomyLevel::SemiAutonomous,
            ..Default::default()
        });

        assert!(!policy.should_pause(&CheckpointClassification::Extrinsic, None));
    }

    #[test]
    fn extrinsic_checkpoint_pauses_at_fully_supervised() {
        let policy = AutonomyPolicy::new(AutonomyConfig {
            level: AutonomyLevel::FullySupervised,
            ..Default::default()
        });

        assert!(policy.should_pause(&CheckpointClassification::Extrinsic, None));
    }

    #[test]
    fn fully_autonomous_still_pauses_for_intrinsic_triggers() {
        let policy = AutonomyPolicy::new(AutonomyConfig {
            level: AutonomyLevel::FullyAutonomous,
            intrinsic_triggers: vec![IntrinsicTrigger::SecurityReview],
            ..Default::default()
        });

        // Intrinsic with a listed trigger -> still pauses
        assert!(policy.should_pause(
            &CheckpointClassification::Intrinsic,
            Some(&IntrinsicTrigger::SecurityReview),
        ));

        // Intrinsic with an unlisted trigger -> auto-proceeds at FullyAutonomous
        assert!(!policy.should_pause(
            &CheckpointClassification::Intrinsic,
            Some(&IntrinsicTrigger::VerificationVerdict),
        ));

        // Extrinsic -> always auto-proceeds at FullyAutonomous
        assert!(!policy.should_pause(&CheckpointClassification::Extrinsic, None));
    }

    #[test]
    fn per_item_bound_enforcement() {
        let policy = AutonomyPolicy::new(AutonomyConfig {
            per_item_bound: 3,
            ..Default::default()
        });

        assert!(!policy.exceeds_item_bound(0));
        assert!(!policy.exceeds_item_bound(2));
        assert!(policy.exceeds_item_bound(3));
        assert!(policy.exceeds_item_bound(10));
    }

    #[test]
    fn per_session_bound_enforcement() {
        let policy = AutonomyPolicy::new(AutonomyConfig {
            per_session_bound: 50,
            ..Default::default()
        });

        assert!(!policy.exceeds_session_bound(49));
        assert!(policy.exceeds_session_bound(50));
        assert!(policy.exceeds_session_bound(100));
    }

    #[test]
    fn autonomy_level_default_is_supervised() {
        assert_eq!(AutonomyLevel::default(), AutonomyLevel::Supervised);
    }

    #[test]
    fn checkpoint_classification_display() {
        assert_eq!(CheckpointClassification::Intrinsic.to_string(), "intrinsic");
        assert_eq!(CheckpointClassification::Extrinsic.to_string(), "extrinsic");
    }
}
