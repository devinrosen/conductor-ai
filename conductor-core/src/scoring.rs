//! Composable scoring and lifecycle-gating module.
//!
//! Covers patterns:
//! - threshold-based-decision-branching@1.0.0
//! - confidence-gated-task-progression@1.0.0
//! - weighted-alignment-scoring@1.1.0
//! - multi-dimension-readiness-gate@1.1.0
//! - score-fix-rescore-convergence-loop@1.0.0
//! - performance-gated-workflow-variant@1.0.0
//! - quality-gate-layering@1.0.0
//! - command-tier-classification@1.0.0
//! - verification-gate-template-instantiation@1.0.0
//!
//! Seven of nine lifecycle-gating patterns share `threshold-based-decision-branching`
//! as their common ancestor. The base [`ThresholdGate`] provides numeric threshold
//! evaluation; all other gates compose it.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core enums
// ---------------------------------------------------------------------------

/// The action a gate recommends after evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionType {
    /// Proceed without intervention.
    Proceed,
    /// Proceed but flag for human review.
    ProceedWithReview,
    /// Block progression entirely.
    Block,
    /// Block and require remediation before re-evaluation.
    BlockAndRemediate,
}

impl fmt::Display for ActionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proceed => write!(f, "proceed"),
            Self::ProceedWithReview => write!(f, "proceed-with-review"),
            Self::Block => write!(f, "block"),
            Self::BlockAndRemediate => write!(f, "block-and-remediate"),
        }
    }
}

/// Overall gate outcome used across all gate types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateOutcome {
    /// Gate passed — work may continue.
    Pass,
    /// Gate failed — work is blocked.
    Fail,
    /// Gate requests a retry (e.g. convergence loop not yet converged).
    Retry,
    /// Gate escalates to a human decision-maker.
    Escalate,
}

impl fmt::Display for GateOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => write!(f, "pass"),
            Self::Fail => write!(f, "fail"),
            Self::Retry => write!(f, "retry"),
            Self::Escalate => write!(f, "escalate"),
        }
    }
}

/// Whether a threshold can be changed after creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThresholdMutability {
    /// Cannot be overridden.
    Immutable,
    /// Overridable via team/project configuration.
    TeamConfigurable,
    /// Adjustable at runtime.
    Dynamic,
}

// ---------------------------------------------------------------------------
// ThresholdGate (threshold-based-decision-branching@1.0.0)
// ---------------------------------------------------------------------------

/// A single tier within a threshold gate. Tiers are evaluated highest-first;
/// the first tier whose threshold is met (score >= threshold) wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierDefinition {
    /// Human-readable tier name (e.g. "excellent", "acceptable", "failing").
    pub name: String,
    /// Minimum score for this tier. `None` means catch-all (always matches).
    pub threshold: Option<f64>,
    /// Recommended action when this tier matches.
    pub action: ActionType,
    /// Description of what this tier means.
    pub description: String,
    /// Whether a human checkpoint is required even on match.
    pub human_checkpoint: bool,
}

/// Result of evaluating a score against a [`ThresholdGate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdResult {
    /// Which tier matched.
    pub tier_name: String,
    /// The action recommended by that tier.
    pub action: ActionType,
    /// The raw score that was evaluated.
    pub score: f64,
    /// Whether a human checkpoint was requested.
    pub human_checkpoint: bool,
    /// The overall gate outcome derived from the action.
    pub outcome: GateOutcome,
}

/// Evaluates a numeric score against an ordered set of tiers.
///
/// This is the base building block for all lifecycle-gating patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdGate {
    /// Name of the metric being gated (e.g. "test_coverage", "alignment_score").
    pub metric_name: String,
    /// Valid score range as (min, max).
    pub score_range: (f64, f64),
    /// Tier definitions, ordered from highest threshold to catch-all.
    pub tiers: Vec<TierDefinition>,
    /// Whether this gate's thresholds can be changed.
    pub mutability: ThresholdMutability,
}

impl ThresholdGate {
    /// Evaluate a score against this gate's tiers.
    ///
    /// Tiers are checked in order; the first tier whose threshold is `None`
    /// or `<= score` is selected. Returns `None` only if `tiers` is empty.
    pub fn evaluate(&self, score: f64) -> Option<ThresholdResult> {
        let clamped = score.clamp(self.score_range.0, self.score_range.1);

        for tier in &self.tiers {
            let matches = match tier.threshold {
                Some(t) => clamped >= t,
                None => true, // catch-all
            };
            if matches {
                let outcome = match &tier.action {
                    ActionType::Proceed => GateOutcome::Pass,
                    ActionType::ProceedWithReview => GateOutcome::Escalate,
                    ActionType::Block => GateOutcome::Fail,
                    ActionType::BlockAndRemediate => GateOutcome::Retry,
                };
                return Some(ThresholdResult {
                    tier_name: tier.name.clone(),
                    action: tier.action.clone(),
                    score: clamped,
                    human_checkpoint: tier.human_checkpoint,
                    outcome,
                });
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// ConfidenceGate (confidence-gated-task-progression@1.0.0)
// ---------------------------------------------------------------------------

/// A binary confidence gate: score must meet a minimum confidence to proceed.
///
/// Wraps [`ThresholdGate`] with a two-tier setup (pass / fail).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceGate {
    /// The underlying threshold gate.
    gate: ThresholdGate,
}

impl ConfidenceGate {
    /// Create a new confidence gate with the given minimum confidence (0.0..=1.0).
    pub fn new(metric_name: &str, min_confidence: f64) -> Self {
        Self {
            gate: ThresholdGate {
                metric_name: metric_name.to_string(),
                score_range: (0.0, 1.0),
                tiers: vec![
                    TierDefinition {
                        name: "confident".to_string(),
                        threshold: Some(min_confidence),
                        action: ActionType::Proceed,
                        description: format!("Confidence >= {:.0}%", min_confidence * 100.0),
                        human_checkpoint: false,
                    },
                    TierDefinition {
                        name: "insufficient".to_string(),
                        threshold: None,
                        action: ActionType::Block,
                        description: format!("Confidence < {:.0}%", min_confidence * 100.0),
                        human_checkpoint: false,
                    },
                ],
                mutability: ThresholdMutability::TeamConfigurable,
            },
        }
    }

    /// Evaluate a confidence value. Returns `true` if the gate passes.
    pub fn passes(&self, confidence: f64) -> bool {
        self.gate
            .evaluate(confidence)
            .map(|r| r.outcome == GateOutcome::Pass)
            .unwrap_or(false)
    }

    /// Full evaluation returning the threshold result.
    pub fn evaluate(&self, confidence: f64) -> Option<ThresholdResult> {
        self.gate.evaluate(confidence)
    }
}

// ---------------------------------------------------------------------------
// WeightedScore (weighted-alignment-scoring@1.1.0)
// ---------------------------------------------------------------------------

/// A single scored dimension with a name, weight, and value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredDimension {
    /// Dimension name (e.g. "code_quality", "test_coverage").
    pub name: String,
    /// Weight of this dimension in the aggregate (must be > 0).
    pub weight: f64,
    /// Raw score for this dimension.
    pub value: f64,
}

/// Multi-criteria weighted scoring.
///
/// Computes a weighted average across named dimensions and evaluates it
/// against a [`ThresholdGate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedScore {
    /// The dimensions being scored.
    pub dimensions: Vec<ScoredDimension>,
    /// Gate to evaluate the aggregate score against.
    pub gate: ThresholdGate,
}

impl WeightedScore {
    /// Compute the weighted average of all dimensions.
    ///
    /// Returns 0.0 if total weight is zero.
    pub fn aggregate(&self) -> f64 {
        let total_weight: f64 = self.dimensions.iter().map(|d| d.weight).sum();
        if total_weight == 0.0 {
            return 0.0;
        }
        let weighted_sum: f64 = self.dimensions.iter().map(|d| d.weight * d.value).sum();
        weighted_sum / total_weight
    }

    /// Compute aggregate and evaluate against the gate.
    pub fn evaluate(&self) -> Option<ThresholdResult> {
        self.gate.evaluate(self.aggregate())
    }
}

// ---------------------------------------------------------------------------
// ReadinessGate (multi-dimension-readiness-gate@1.1.0)
// ---------------------------------------------------------------------------

/// A single readiness dimension with its own threshold gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessDimension {
    /// Name of this readiness dimension.
    pub name: String,
    /// The gate for this dimension.
    pub gate: ThresholdGate,
}

/// Multi-dimension readiness gate: ALL dimensions must pass for the overall
/// gate to pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessGate {
    /// The dimensions that must all pass.
    pub dimensions: Vec<ReadinessDimension>,
}

/// Result of evaluating a [`ReadinessGate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessResult {
    /// Overall outcome — Pass only if every dimension passed.
    pub outcome: GateOutcome,
    /// Per-dimension results.
    pub dimension_results: Vec<(String, ThresholdResult)>,
    /// Names of dimensions that failed.
    pub failed_dimensions: Vec<String>,
}

impl ReadinessGate {
    /// Evaluate all dimensions. Returns [`GateOutcome::Pass`] only if every
    /// dimension passes.
    pub fn evaluate(&self, scores: &HashMap<String, f64>) -> ReadinessResult {
        let mut dimension_results = Vec::new();
        let mut failed_dimensions = Vec::new();

        for dim in &self.dimensions {
            let score = scores.get(&dim.name).copied().unwrap_or(0.0);
            if let Some(result) = dim.gate.evaluate(score) {
                if result.outcome != GateOutcome::Pass {
                    failed_dimensions.push(dim.name.clone());
                }
                dimension_results.push((dim.name.clone(), result));
            } else {
                failed_dimensions.push(dim.name.clone());
            }
        }

        let outcome = if failed_dimensions.is_empty() {
            GateOutcome::Pass
        } else {
            GateOutcome::Fail
        };

        ReadinessResult {
            outcome,
            dimension_results,
            failed_dimensions,
        }
    }
}

// ---------------------------------------------------------------------------
// ConvergenceLoop (score-fix-rescore-convergence-loop@1.0.0)
// ---------------------------------------------------------------------------

/// Record of a single iteration in the convergence loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationRecord {
    /// Iteration number (1-based).
    pub iteration: usize,
    /// Score at the start of this iteration.
    pub score_before: f64,
    /// Score after the fix step.
    pub score_after: f64,
    /// Delta (score_after - score_before).
    pub delta: f64,
}

/// Result of running a convergence loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceResult {
    /// Whether the loop converged (final score meets threshold).
    pub converged: bool,
    /// Final score after all iterations.
    pub final_score: f64,
    /// Number of iterations executed.
    pub iterations_run: usize,
    /// Per-iteration records.
    pub history: Vec<IterationRecord>,
    /// Overall gate outcome.
    pub outcome: GateOutcome,
}

/// Configuration for a score-fix-rescore convergence loop.
///
/// Repeatedly scores, applies a fix, and re-scores until either the target
/// threshold is met or `max_iterations` is exhausted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceLoop {
    /// Target score threshold for convergence.
    pub target_threshold: f64,
    /// Maximum number of iterations before giving up.
    pub max_iterations: usize,
    /// Minimum improvement per iteration; if delta falls below this the loop
    /// is considered stuck and terminates early.
    pub min_delta: f64,
}

impl ConvergenceLoop {
    /// Run the convergence loop.
    ///
    /// - `initial_score`: the starting score.
    /// - `fix_fn`: a closure that takes the current score and returns the new
    ///   score after applying a fix. The caller is responsible for performing
    ///   the actual remediation; this closure models the score impact.
    pub fn run<F>(&self, initial_score: f64, mut fix_fn: F) -> ConvergenceResult
    where
        F: FnMut(f64) -> f64,
    {
        let mut current_score = initial_score;
        let mut history = Vec::new();

        for i in 1..=self.max_iterations {
            let score_before = current_score;
            let score_after = fix_fn(current_score);
            let delta = score_after - score_before;

            history.push(IterationRecord {
                iteration: i,
                score_before,
                score_after,
                delta,
            });

            current_score = score_after;

            // Converged?
            if current_score >= self.target_threshold {
                return ConvergenceResult {
                    converged: true,
                    final_score: current_score,
                    iterations_run: i,
                    history,
                    outcome: GateOutcome::Pass,
                };
            }

            // Stuck? (delta below minimum improvement)
            if delta.abs() < self.min_delta {
                return ConvergenceResult {
                    converged: false,
                    final_score: current_score,
                    iterations_run: i,
                    history,
                    outcome: GateOutcome::Escalate,
                };
            }
        }

        ConvergenceResult {
            converged: false,
            final_score: current_score,
            iterations_run: self.max_iterations,
            history,
            outcome: GateOutcome::Fail,
        }
    }
}

// ---------------------------------------------------------------------------
// WorkflowVariantSelector (performance-gated-workflow-variant@1.0.0)
// ---------------------------------------------------------------------------

/// Which workflow variant to use based on performance characteristics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowVariant {
    /// Fast path — fewer checks, quicker turnaround.
    Fast,
    /// Thorough path — full checks, longer turnaround.
    Thorough,
}

impl fmt::Display for WorkflowVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fast => write!(f, "fast"),
            Self::Thorough => write!(f, "thorough"),
        }
    }
}

/// Selects between fast and thorough workflow variants based on a performance
/// metric evaluated against a threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowVariantSelector {
    /// Name of the performance metric.
    pub metric_name: String,
    /// Threshold above which the thorough variant is selected.
    pub thorough_threshold: f64,
    /// Score range for the metric.
    pub score_range: (f64, f64),
}

impl WorkflowVariantSelector {
    /// Select the appropriate workflow variant for the given performance score.
    pub fn select(&self, performance_score: f64) -> WorkflowVariant {
        let clamped = performance_score.clamp(self.score_range.0, self.score_range.1);
        if clamped >= self.thorough_threshold {
            WorkflowVariant::Thorough
        } else {
            WorkflowVariant::Fast
        }
    }
}

// ---------------------------------------------------------------------------
// QualityGateStack (quality-gate-layering@1.0.0)
// ---------------------------------------------------------------------------

/// A named quality gate layer within a stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityLayer {
    /// Name of this quality layer (e.g. "lint", "test", "security").
    pub name: String,
    /// The threshold gate for this layer.
    pub gate: ThresholdGate,
}

/// Result of evaluating a single quality layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerResult {
    /// Name of the layer.
    pub layer_name: String,
    /// The threshold result for this layer.
    pub result: ThresholdResult,
}

/// Stacks multiple quality gates in layers. Evaluation short-circuits on the
/// first failure — layers are evaluated in order and a failing layer stops
/// further evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateStack {
    /// Ordered layers, evaluated first to last.
    pub layers: Vec<QualityLayer>,
}

/// Result of evaluating a [`QualityGateStack`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityStackResult {
    /// Overall outcome.
    pub outcome: GateOutcome,
    /// Results of layers that were evaluated (may be shorter than total layers
    /// if short-circuited).
    pub layer_results: Vec<LayerResult>,
    /// Name of the layer that caused failure, if any.
    pub failed_at: Option<String>,
}

impl QualityGateStack {
    /// Evaluate all layers in order. Short-circuits on the first non-passing layer.
    pub fn evaluate(&self, scores: &HashMap<String, f64>) -> QualityStackResult {
        let mut layer_results = Vec::new();

        for layer in &self.layers {
            let score = scores.get(&layer.name).copied().unwrap_or(0.0);
            if let Some(result) = layer.gate.evaluate(score) {
                let passed = result.outcome == GateOutcome::Pass;
                layer_results.push(LayerResult {
                    layer_name: layer.name.clone(),
                    result,
                });
                if !passed {
                    return QualityStackResult {
                        outcome: GateOutcome::Fail,
                        failed_at: Some(layer.name.clone()),
                        layer_results,
                    };
                }
            }
        }

        QualityStackResult {
            outcome: GateOutcome::Pass,
            layer_results,
            failed_at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// CommandTier (command-tier-classification@1.0.0)
// ---------------------------------------------------------------------------

/// Priority tier for command classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandTier {
    /// Critical / must-have — blocks releases if broken.
    P0,
    /// Important — should be fixed before release.
    P1,
    /// Nice-to-have — can be deferred.
    P2,
}

impl fmt::Display for CommandTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::P0 => write!(f, "P0"),
            Self::P1 => write!(f, "P1"),
            Self::P2 => write!(f, "P2"),
        }
    }
}

impl CommandTier {
    /// Classify a command into a tier based on a criticality score (0.0..=1.0).
    ///
    /// - P0: score >= 0.8
    /// - P1: score >= 0.5
    /// - P2: score < 0.5
    pub fn classify(criticality: f64) -> Self {
        let clamped = criticality.clamp(0.0, 1.0);
        if clamped >= 0.8 {
            Self::P0
        } else if clamped >= 0.5 {
            Self::P1
        } else {
            Self::P2
        }
    }

    /// Whether this tier blocks releases.
    pub fn is_release_blocking(&self) -> bool {
        matches!(self, Self::P0)
    }
}

// ---------------------------------------------------------------------------
// GateTemplate (verification-gate-template-instantiation@1.0.0)
// ---------------------------------------------------------------------------

/// A parameterized gate template that can be instantiated per-workflow.
///
/// Parameters are string placeholders (e.g. `{{metric_name}}`, `{{threshold}}`)
/// that are resolved at instantiation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateTemplate {
    /// Template identifier.
    pub template_id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this template gates.
    pub description: String,
    /// Parameter names expected during instantiation.
    pub parameters: Vec<String>,
    /// Default metric name (can be overridden via parameters).
    pub default_metric_name: String,
    /// Default score range.
    pub default_score_range: (f64, f64),
    /// Default tier definitions.
    pub default_tiers: Vec<TierDefinition>,
    /// Default mutability.
    pub default_mutability: ThresholdMutability,
}

impl GateTemplate {
    /// Instantiate this template into a concrete [`ThresholdGate`] by
    /// providing parameter overrides.
    ///
    /// Supported parameters:
    /// - `"metric_name"` — overrides the metric name
    /// - `"score_min"` / `"score_max"` — override score range bounds
    /// - `"threshold_<tier_name>"` — override a specific tier's threshold
    pub fn instantiate(&self, params: &HashMap<String, String>) -> ThresholdGate {
        let metric_name = params
            .get("metric_name")
            .cloned()
            .unwrap_or_else(|| self.default_metric_name.clone());

        let score_min = params
            .get("score_min")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(self.default_score_range.0);
        let score_max = params
            .get("score_max")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(self.default_score_range.1);

        let tiers = self
            .default_tiers
            .iter()
            .map(|tier| {
                let key = format!("threshold_{}", tier.name);
                let threshold = params
                    .get(&key)
                    .and_then(|v| v.parse::<f64>().ok())
                    .or(tier.threshold);
                TierDefinition {
                    name: tier.name.clone(),
                    threshold,
                    action: tier.action.clone(),
                    description: tier.description.clone(),
                    human_checkpoint: tier.human_checkpoint,
                }
            })
            .collect();

        ThresholdGate {
            metric_name,
            score_range: (score_min, score_max),
            tiers,
            mutability: self.default_mutability.clone(),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- helpers --

    fn make_three_tier_gate(metric: &str) -> ThresholdGate {
        ThresholdGate {
            metric_name: metric.to_string(),
            score_range: (0.0, 100.0),
            tiers: vec![
                TierDefinition {
                    name: "excellent".to_string(),
                    threshold: Some(80.0),
                    action: ActionType::Proceed,
                    description: "Score >= 80".to_string(),
                    human_checkpoint: false,
                },
                TierDefinition {
                    name: "acceptable".to_string(),
                    threshold: Some(50.0),
                    action: ActionType::ProceedWithReview,
                    description: "Score >= 50".to_string(),
                    human_checkpoint: true,
                },
                TierDefinition {
                    name: "failing".to_string(),
                    threshold: None,
                    action: ActionType::Block,
                    description: "Score < 50".to_string(),
                    human_checkpoint: false,
                },
            ],
            mutability: ThresholdMutability::TeamConfigurable,
        }
    }

    fn make_pass_gate(metric: &str, threshold: f64) -> ThresholdGate {
        ThresholdGate {
            metric_name: metric.to_string(),
            score_range: (0.0, 100.0),
            tiers: vec![
                TierDefinition {
                    name: "pass".to_string(),
                    threshold: Some(threshold),
                    action: ActionType::Proceed,
                    description: format!("Score >= {threshold}"),
                    human_checkpoint: false,
                },
                TierDefinition {
                    name: "fail".to_string(),
                    threshold: None,
                    action: ActionType::Block,
                    description: format!("Score < {threshold}"),
                    human_checkpoint: false,
                },
            ],
            mutability: ThresholdMutability::Immutable,
        }
    }

    // -----------------------------------------------------------------------
    // ThresholdGate tests
    // -----------------------------------------------------------------------

    #[test]
    fn threshold_gate_excellent_tier() {
        let gate = make_three_tier_gate("coverage");
        let result = gate.evaluate(90.0).unwrap();
        assert_eq!(result.tier_name, "excellent");
        assert_eq!(result.outcome, GateOutcome::Pass);
        assert!(!result.human_checkpoint);
    }

    #[test]
    fn threshold_gate_acceptable_tier() {
        let gate = make_three_tier_gate("coverage");
        let result = gate.evaluate(65.0).unwrap();
        assert_eq!(result.tier_name, "acceptable");
        assert_eq!(result.outcome, GateOutcome::Escalate);
        assert!(result.human_checkpoint);
    }

    #[test]
    fn threshold_gate_failing_tier_catchall() {
        let gate = make_three_tier_gate("coverage");
        let result = gate.evaluate(20.0).unwrap();
        assert_eq!(result.tier_name, "failing");
        assert_eq!(result.outcome, GateOutcome::Fail);
    }

    #[test]
    fn threshold_gate_clamps_out_of_range() {
        let gate = make_three_tier_gate("coverage");
        // Score above max should clamp to 100 -> excellent
        let result = gate.evaluate(150.0).unwrap();
        assert_eq!(result.tier_name, "excellent");
        assert_eq!(result.score, 100.0);
    }

    // -----------------------------------------------------------------------
    // ConfidenceGate tests
    // -----------------------------------------------------------------------

    #[test]
    fn confidence_gate_passes_above_threshold() {
        let gate = ConfidenceGate::new("test_confidence", 0.7);
        assert!(gate.passes(0.85));
    }

    #[test]
    fn confidence_gate_fails_below_threshold() {
        let gate = ConfidenceGate::new("test_confidence", 0.7);
        assert!(!gate.passes(0.5));
    }

    #[test]
    fn confidence_gate_boundary() {
        let gate = ConfidenceGate::new("test_confidence", 0.7);
        assert!(gate.passes(0.7)); // exactly at threshold should pass
    }

    // -----------------------------------------------------------------------
    // WeightedScore tests
    // -----------------------------------------------------------------------

    #[test]
    fn weighted_score_aggregate_calculation() {
        let ws = WeightedScore {
            dimensions: vec![
                ScoredDimension {
                    name: "quality".to_string(),
                    weight: 3.0,
                    value: 80.0,
                },
                ScoredDimension {
                    name: "speed".to_string(),
                    weight: 1.0,
                    value: 60.0,
                },
            ],
            gate: make_pass_gate("aggregate", 70.0),
        };
        // (3*80 + 1*60) / 4 = 75
        assert!((ws.aggregate() - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn weighted_score_passes_gate() {
        let ws = WeightedScore {
            dimensions: vec![
                ScoredDimension {
                    name: "quality".to_string(),
                    weight: 3.0,
                    value: 80.0,
                },
                ScoredDimension {
                    name: "speed".to_string(),
                    weight: 1.0,
                    value: 60.0,
                },
            ],
            gate: make_pass_gate("aggregate", 70.0),
        };
        let result = ws.evaluate().unwrap();
        assert_eq!(result.outcome, GateOutcome::Pass);
    }

    #[test]
    fn weighted_score_empty_dimensions() {
        let ws = WeightedScore {
            dimensions: vec![],
            gate: make_pass_gate("aggregate", 70.0),
        };
        assert!((ws.aggregate() - 0.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // ReadinessGate tests
    // -----------------------------------------------------------------------

    #[test]
    fn readiness_gate_all_pass() {
        let gate = ReadinessGate {
            dimensions: vec![
                ReadinessDimension {
                    name: "tests".to_string(),
                    gate: make_pass_gate("tests", 60.0),
                },
                ReadinessDimension {
                    name: "docs".to_string(),
                    gate: make_pass_gate("docs", 50.0),
                },
            ],
        };
        let mut scores = HashMap::new();
        scores.insert("tests".to_string(), 80.0);
        scores.insert("docs".to_string(), 70.0);

        let result = gate.evaluate(&scores);
        assert_eq!(result.outcome, GateOutcome::Pass);
        assert!(result.failed_dimensions.is_empty());
    }

    #[test]
    fn readiness_gate_one_fails() {
        let gate = ReadinessGate {
            dimensions: vec![
                ReadinessDimension {
                    name: "tests".to_string(),
                    gate: make_pass_gate("tests", 60.0),
                },
                ReadinessDimension {
                    name: "docs".to_string(),
                    gate: make_pass_gate("docs", 50.0),
                },
            ],
        };
        let mut scores = HashMap::new();
        scores.insert("tests".to_string(), 80.0);
        scores.insert("docs".to_string(), 30.0); // below 50

        let result = gate.evaluate(&scores);
        assert_eq!(result.outcome, GateOutcome::Fail);
        assert_eq!(result.failed_dimensions, vec!["docs"]);
    }

    #[test]
    fn readiness_gate_missing_dimension_fails() {
        let gate = ReadinessGate {
            dimensions: vec![ReadinessDimension {
                name: "tests".to_string(),
                gate: make_pass_gate("tests", 60.0),
            }],
        };
        let scores = HashMap::new(); // no scores at all
        let result = gate.evaluate(&scores);
        assert_eq!(result.outcome, GateOutcome::Fail);
    }

    // -----------------------------------------------------------------------
    // ConvergenceLoop tests
    // -----------------------------------------------------------------------

    #[test]
    fn convergence_loop_converges() {
        let loop_cfg = ConvergenceLoop {
            target_threshold: 80.0,
            max_iterations: 10,
            min_delta: 1.0,
        };
        // Each fix adds 15 points
        let result = loop_cfg.run(50.0, |score| score + 15.0);
        assert!(result.converged);
        assert_eq!(result.outcome, GateOutcome::Pass);
        assert_eq!(result.iterations_run, 2); // 50->65->80
        assert!(result.final_score >= 80.0);
    }

    #[test]
    fn convergence_loop_exhausts_iterations() {
        let loop_cfg = ConvergenceLoop {
            target_threshold: 100.0,
            max_iterations: 3,
            min_delta: 0.5,
        };
        // Each fix adds only 2 points
        let result = loop_cfg.run(50.0, |score| score + 2.0);
        assert!(!result.converged);
        assert_eq!(result.outcome, GateOutcome::Fail);
        assert_eq!(result.iterations_run, 3);
    }

    #[test]
    fn convergence_loop_stuck_early_exit() {
        let loop_cfg = ConvergenceLoop {
            target_threshold: 100.0,
            max_iterations: 10,
            min_delta: 1.0,
        };
        // Fix does almost nothing
        let result = loop_cfg.run(50.0, |score| score + 0.1);
        assert!(!result.converged);
        assert_eq!(result.outcome, GateOutcome::Escalate);
        assert_eq!(result.iterations_run, 1); // exits on first stuck iteration
    }

    // -----------------------------------------------------------------------
    // WorkflowVariantSelector tests
    // -----------------------------------------------------------------------

    #[test]
    fn variant_selector_fast_path() {
        let selector = WorkflowVariantSelector {
            metric_name: "complexity".to_string(),
            thorough_threshold: 70.0,
            score_range: (0.0, 100.0),
        };
        assert_eq!(selector.select(30.0), WorkflowVariant::Fast);
    }

    #[test]
    fn variant_selector_thorough_path() {
        let selector = WorkflowVariantSelector {
            metric_name: "complexity".to_string(),
            thorough_threshold: 70.0,
            score_range: (0.0, 100.0),
        };
        assert_eq!(selector.select(85.0), WorkflowVariant::Thorough);
    }

    #[test]
    fn variant_selector_boundary() {
        let selector = WorkflowVariantSelector {
            metric_name: "complexity".to_string(),
            thorough_threshold: 70.0,
            score_range: (0.0, 100.0),
        };
        assert_eq!(selector.select(70.0), WorkflowVariant::Thorough);
    }

    // -----------------------------------------------------------------------
    // QualityGateStack tests
    // -----------------------------------------------------------------------

    #[test]
    fn quality_stack_all_pass() {
        let stack = QualityGateStack {
            layers: vec![
                QualityLayer {
                    name: "lint".to_string(),
                    gate: make_pass_gate("lint", 60.0),
                },
                QualityLayer {
                    name: "test".to_string(),
                    gate: make_pass_gate("test", 70.0),
                },
            ],
        };
        let mut scores = HashMap::new();
        scores.insert("lint".to_string(), 80.0);
        scores.insert("test".to_string(), 90.0);

        let result = stack.evaluate(&scores);
        assert_eq!(result.outcome, GateOutcome::Pass);
        assert!(result.failed_at.is_none());
        assert_eq!(result.layer_results.len(), 2);
    }

    #[test]
    fn quality_stack_short_circuits_on_failure() {
        let stack = QualityGateStack {
            layers: vec![
                QualityLayer {
                    name: "lint".to_string(),
                    gate: make_pass_gate("lint", 60.0),
                },
                QualityLayer {
                    name: "test".to_string(),
                    gate: make_pass_gate("test", 70.0),
                },
                QualityLayer {
                    name: "security".to_string(),
                    gate: make_pass_gate("security", 80.0),
                },
            ],
        };
        let mut scores = HashMap::new();
        scores.insert("lint".to_string(), 80.0);
        scores.insert("test".to_string(), 50.0); // fails
        scores.insert("security".to_string(), 90.0);

        let result = stack.evaluate(&scores);
        assert_eq!(result.outcome, GateOutcome::Fail);
        assert_eq!(result.failed_at, Some("test".to_string()));
        // Only lint and test evaluated, security was short-circuited
        assert_eq!(result.layer_results.len(), 2);
    }

    // -----------------------------------------------------------------------
    // CommandTier tests
    // -----------------------------------------------------------------------

    #[test]
    fn command_tier_p0() {
        assert_eq!(CommandTier::classify(0.95), CommandTier::P0);
        assert!(CommandTier::P0.is_release_blocking());
    }

    #[test]
    fn command_tier_p1() {
        assert_eq!(CommandTier::classify(0.6), CommandTier::P1);
        assert!(!CommandTier::P1.is_release_blocking());
    }

    #[test]
    fn command_tier_p2() {
        assert_eq!(CommandTier::classify(0.3), CommandTier::P2);
        assert!(!CommandTier::P2.is_release_blocking());
    }

    #[test]
    fn command_tier_boundary_p0() {
        assert_eq!(CommandTier::classify(0.8), CommandTier::P0);
    }

    #[test]
    fn command_tier_boundary_p1() {
        assert_eq!(CommandTier::classify(0.5), CommandTier::P1);
    }

    // -----------------------------------------------------------------------
    // GateTemplate tests
    // -----------------------------------------------------------------------

    #[test]
    fn gate_template_default_instantiation() {
        let template = GateTemplate {
            template_id: "coverage-gate".to_string(),
            name: "Coverage Gate".to_string(),
            description: "Gates on test coverage".to_string(),
            parameters: vec!["metric_name".to_string(), "threshold_pass".to_string()],
            default_metric_name: "test_coverage".to_string(),
            default_score_range: (0.0, 100.0),
            default_tiers: vec![
                TierDefinition {
                    name: "pass".to_string(),
                    threshold: Some(70.0),
                    action: ActionType::Proceed,
                    description: "Coverage >= 70%".to_string(),
                    human_checkpoint: false,
                },
                TierDefinition {
                    name: "fail".to_string(),
                    threshold: None,
                    action: ActionType::Block,
                    description: "Coverage < 70%".to_string(),
                    human_checkpoint: false,
                },
            ],
            default_mutability: ThresholdMutability::TeamConfigurable,
        };

        let gate = template.instantiate(&HashMap::new());
        assert_eq!(gate.metric_name, "test_coverage");
        assert_eq!(gate.score_range, (0.0, 100.0));

        let result = gate.evaluate(80.0).unwrap();
        assert_eq!(result.outcome, GateOutcome::Pass);
    }

    #[test]
    fn gate_template_with_overrides() {
        let template = GateTemplate {
            template_id: "coverage-gate".to_string(),
            name: "Coverage Gate".to_string(),
            description: "Gates on test coverage".to_string(),
            parameters: vec!["metric_name".to_string(), "threshold_pass".to_string()],
            default_metric_name: "test_coverage".to_string(),
            default_score_range: (0.0, 100.0),
            default_tiers: vec![
                TierDefinition {
                    name: "pass".to_string(),
                    threshold: Some(70.0),
                    action: ActionType::Proceed,
                    description: "Coverage passes".to_string(),
                    human_checkpoint: false,
                },
                TierDefinition {
                    name: "fail".to_string(),
                    threshold: None,
                    action: ActionType::Block,
                    description: "Coverage fails".to_string(),
                    human_checkpoint: false,
                },
            ],
            default_mutability: ThresholdMutability::TeamConfigurable,
        };

        let mut params = HashMap::new();
        params.insert("metric_name".to_string(), "branch_coverage".to_string());
        params.insert("threshold_pass".to_string(), "90.0".to_string());

        let gate = template.instantiate(&params);
        assert_eq!(gate.metric_name, "branch_coverage");

        // 80 would pass default (70) but fails override (90)
        let result = gate.evaluate(80.0).unwrap();
        assert_eq!(result.outcome, GateOutcome::Fail);
    }

    #[test]
    fn gate_template_score_range_override() {
        let template = GateTemplate {
            template_id: "perf-gate".to_string(),
            name: "Performance Gate".to_string(),
            description: "Gates on performance".to_string(),
            parameters: vec!["score_min".to_string(), "score_max".to_string()],
            default_metric_name: "latency_ms".to_string(),
            default_score_range: (0.0, 1000.0),
            default_tiers: vec![TierDefinition {
                name: "ok".to_string(),
                threshold: None,
                action: ActionType::Proceed,
                description: "Always passes".to_string(),
                human_checkpoint: false,
            }],
            default_mutability: ThresholdMutability::Dynamic,
        };

        let mut params = HashMap::new();
        params.insert("score_min".to_string(), "0".to_string());
        params.insert("score_max".to_string(), "500".to_string());

        let gate = template.instantiate(&params);
        assert_eq!(gate.score_range, (0.0, 500.0));
    }
}
