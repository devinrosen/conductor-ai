//! Hybrid workflow time estimation.
//!
//! Blends two signal sources to estimate total workflow duration:
//! 1. **LLM estimate** — `estimated_minutes` from a plan step's structured output.
//! 2. **Historical data** — `total_duration_ms` from past completed runs of the same workflow.
//!
//! All functions are pure (no DB, no async) — callers fetch data and pass it in.

use std::collections::HashMap;

use chrono::Utc;
use serde::Serialize;

use super::status::WorkflowStepStatus;
use super::types::WorkflowRunStep;

// ── Types ────────────────────────────────────────────────────────────

/// Confidence level for a time estimate, derived from sample size and variance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// A time estimate with confidence bounds.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Estimate {
    /// Best-guess duration in milliseconds.
    pub point_ms: i64,
    /// Lower bound (p25) in milliseconds.
    pub low_ms: i64,
    /// Upper bound (p75) in milliseconds.
    pub high_ms: i64,
    /// Confidence level based on sample size and variance.
    pub confidence: Confidence,
}

/// Result of a live remaining-time computation.
#[derive(Debug, Clone, Serialize)]
pub struct LiveEstimate {
    pub remaining_ms: i64,
    pub low_remaining_ms: i64,
    pub high_remaining_ms: i64,
    pub confidence: Confidence,
}

/// Per-step estimates keyed by step name.
pub type StepEstimates = HashMap<String, Estimate>;

// ── Workflow-level estimation (v1) ───────────────────────────────────

/// Compute the estimated total workflow duration in milliseconds.
///
/// Blending strategy based on amount of historical data:
/// - **0 completed runs** → LLM estimate only
/// - **1–2 runs** → LLM estimate if available, else historical median
/// - **3–9 runs** → 40% LLM + 60% historical median
/// - **10+ runs** → historical median only
///
/// Returns `None` when neither signal is available.
pub fn estimate_duration_ms(
    llm_estimate_ms: Option<i64>,
    historical_durations_ms: &[i64],
) -> Option<i64> {
    let historical_median = median(historical_durations_ms);
    let n = historical_durations_ms.len();

    match n {
        0 => llm_estimate_ms,
        1..=2 => llm_estimate_ms.or(historical_median),
        3..=9 => {
            let med = historical_median?;
            match llm_estimate_ms {
                Some(llm) => Some((0.4 * llm as f64 + 0.6 * med as f64) as i64),
                None => Some(med),
            }
        }
        _ => historical_median,
    }
}

/// Like [`estimate_duration_ms`] but returns an [`Estimate`] with confidence bounds.
pub fn estimate_with_confidence(
    llm_estimate_ms: Option<i64>,
    historical_durations_ms: &[i64],
) -> Option<Estimate> {
    let point = estimate_duration_ms(llm_estimate_ms, historical_durations_ms)?;
    let n = historical_durations_ms.len();

    if n < 3 {
        // Not enough data for meaningful bounds — use ±30% heuristic.
        let margin = (point as f64 * 0.3) as i64;
        return Some(Estimate {
            point_ms: point,
            low_ms: (point - margin).max(0),
            high_ms: point + margin,
            confidence: Confidence::Low,
        });
    }

    let mut sorted = historical_durations_ms.to_vec();
    sorted.sort_unstable();
    let p25 = percentile(&sorted, 0.25);
    let p75 = percentile(&sorted, 0.75);
    let confidence = compute_confidence(n, historical_durations_ms);

    Some(Estimate {
        point_ms: point,
        low_ms: p25,
        high_ms: p75,
        confidence,
    })
}

/// Compute estimated remaining milliseconds for an in-progress run.
///
/// Returns 0 when elapsed time already exceeds the estimate.
pub fn estimated_remaining_ms(estimated_total_ms: i64, started_at: &str) -> i64 {
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return 0;
    };
    let elapsed_ms = (Utc::now() - start.with_timezone(&Utc))
        .num_milliseconds()
        .max(0);
    (estimated_total_ms - elapsed_ms).max(0)
}

/// Extract the LLM's `estimated_minutes` from workflow run steps.
///
/// Scans steps for the first completed step whose `structured_output` JSON
/// contains an `estimated_minutes` numeric field. Converts minutes → milliseconds.
pub fn extract_llm_estimate_ms(steps: &[WorkflowRunStep]) -> Option<i64> {
    for step in steps {
        if let Some(ref json_str) = step.structured_output {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(minutes) = val.get("estimated_minutes").and_then(|v| v.as_f64()) {
                    return Some((minutes * 60_000.0) as i64);
                }
            }
        }
    }
    None
}

// ── Per-step estimation (v2) ─────────────────────────────────────────

/// Build per-step estimates for all steps in a workflow using historical data.
///
/// `step_histories` is keyed by `step_name → Vec<duration_ms>` from past completed runs.
pub fn estimate_all_steps(step_histories: &HashMap<String, Vec<i64>>) -> StepEstimates {
    let mut result = HashMap::new();
    for (step_name, durations) in step_histories {
        if let Some(est) = estimate_with_confidence(None, durations) {
            result.insert(step_name.clone(), est);
        }
    }
    result
}

/// Compute live remaining estimate by summing per-step estimates for incomplete steps.
///
/// - **Completed/Skipped/Failed** steps contribute 0 (already done).
/// - **Running** steps contribute `max(0, estimate - elapsed)`.
/// - **Pending/Waiting** steps contribute the full estimate.
///
/// Returns `None` when no step has an estimate.
pub fn live_remaining_estimate(
    steps: &[WorkflowRunStep],
    step_estimates: &StepEstimates,
) -> Option<LiveEstimate> {
    let mut remaining_point: i64 = 0;
    let mut remaining_low: i64 = 0;
    let mut remaining_high: i64 = 0;
    let mut has_any = false;

    for step in steps {
        match step.status {
            WorkflowStepStatus::Completed
            | WorkflowStepStatus::Skipped
            | WorkflowStepStatus::Failed
            | WorkflowStepStatus::TimedOut => continue,

            WorkflowStepStatus::Running => {
                if let Some(est) = step_estimates.get(&step.step_name) {
                    let elapsed = step
                        .started_at
                        .as_ref()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|start| {
                            (Utc::now() - start.with_timezone(&Utc))
                                .num_milliseconds()
                                .max(0)
                        })
                        .unwrap_or(0);
                    remaining_point += (est.point_ms - elapsed).max(0);
                    remaining_low += (est.low_ms - elapsed).max(0);
                    remaining_high += (est.high_ms - elapsed).max(0);
                    has_any = true;
                }
            }

            WorkflowStepStatus::Pending | WorkflowStepStatus::Waiting => {
                if let Some(est) = step_estimates.get(&step.step_name) {
                    remaining_point += est.point_ms;
                    remaining_low += est.low_ms;
                    remaining_high += est.high_ms;
                    has_any = true;
                }
            }
        }
    }

    if !has_any {
        return None;
    }

    // Aggregate confidence: worst across all contributing steps.
    let worst = step_estimates
        .values()
        .map(|e| e.confidence)
        .min_by_key(|c| match c {
            Confidence::Low => 0,
            Confidence::Medium => 1,
            Confidence::High => 2,
        })
        .unwrap_or(Confidence::Low);

    Some(LiveEstimate {
        remaining_ms: remaining_point,
        low_remaining_ms: remaining_low,
        high_remaining_ms: remaining_high,
        confidence: worst,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Compute the median of a slice of durations.
fn median(values: &[i64]) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        Some((sorted[mid - 1] + sorted[mid]) / 2)
    } else {
        Some(sorted[mid])
    }
}

/// Compute the value at a given percentile (0.0–1.0) from a pre-sorted slice.
fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (p * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Determine confidence from sample count and coefficient of variation.
fn compute_confidence(n: usize, values: &[i64]) -> Confidence {
    if n < 3 {
        return Confidence::Low;
    }
    if n < 10 {
        return Confidence::Medium;
    }
    let mean = values.iter().sum::<i64>() as f64 / n as f64;
    if mean <= 0.0 {
        return Confidence::Medium;
    }
    let variance = values
        .iter()
        .map(|v| {
            let d = *v as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n as f64;
    let cv = variance.sqrt() / mean;
    if cv < 0.3 {
        Confidence::High
    } else {
        Confidence::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── v1 tests ─────────────────────────────────────────────────────

    #[test]
    fn test_median_odd() {
        assert_eq!(median(&[100, 300, 200]), Some(200));
    }

    #[test]
    fn test_median_even() {
        assert_eq!(median(&[100, 200, 300, 400]), Some(250));
    }

    #[test]
    fn test_median_single() {
        assert_eq!(median(&[42]), Some(42));
    }

    #[test]
    fn test_median_empty() {
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn test_estimate_no_data() {
        assert_eq!(estimate_duration_ms(None, &[]), None);
    }

    #[test]
    fn test_estimate_llm_only() {
        assert_eq!(estimate_duration_ms(Some(600_000), &[]), Some(600_000));
    }

    #[test]
    fn test_estimate_few_runs_prefers_llm() {
        // 1-2 runs: LLM preferred over historical
        assert_eq!(
            estimate_duration_ms(Some(600_000), &[900_000]),
            Some(600_000)
        );
    }

    #[test]
    fn test_estimate_few_runs_falls_back_to_historical() {
        // 1-2 runs, no LLM: use historical median
        assert_eq!(estimate_duration_ms(None, &[900_000]), Some(900_000));
    }

    #[test]
    fn test_estimate_blend_3_runs() {
        // 3-9 runs: 40% LLM + 60% historical median
        let llm = 600_000i64;
        let hist = vec![800_000, 900_000, 1_000_000]; // median = 900_000
        let expected = (0.4 * 600_000.0 + 0.6 * 900_000.0) as i64; // 780_000
        assert_eq!(estimate_duration_ms(Some(llm), &hist), Some(expected));
    }

    #[test]
    fn test_estimate_blend_no_llm() {
        // 3-9 runs, no LLM: historical median only
        let hist = vec![800_000, 900_000, 1_000_000];
        assert_eq!(estimate_duration_ms(None, &hist), Some(900_000));
    }

    #[test]
    fn test_estimate_historical_only_10_plus() {
        // 10+ runs: historical median, LLM ignored
        let hist: Vec<i64> = (1..=12).map(|i| i * 100_000).collect(); // median ~650_000
        let result = estimate_duration_ms(Some(1), &hist);
        assert_eq!(result, median(&hist));
    }

    #[test]
    fn test_remaining_basic() {
        // Use a start time 5 minutes ago
        let start = (Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        let remaining = estimated_remaining_ms(600_000, &start); // 10 min total
                                                                 // Should be roughly 5 minutes remaining (300_000 ms), allow 2s tolerance
        assert!(
            remaining > 298_000 && remaining < 302_000,
            "got {remaining}"
        );
    }

    #[test]
    fn test_remaining_exceeded() {
        let start = (Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        assert_eq!(estimated_remaining_ms(600_000, &start), 0);
    }

    #[test]
    fn test_remaining_bad_timestamp() {
        assert_eq!(estimated_remaining_ms(600_000, "not-a-date"), 0);
    }

    #[test]
    fn test_extract_llm_estimate() {
        let steps = vec![
            make_step(None, WorkflowStepStatus::Completed),
            make_step(
                Some(r#"{"estimated_minutes": 15, "summary": "test"}"#),
                WorkflowStepStatus::Completed,
            ),
        ];
        assert_eq!(extract_llm_estimate_ms(&steps), Some(900_000));
    }

    #[test]
    fn test_extract_llm_estimate_missing() {
        let steps = vec![make_step(
            Some(r#"{"summary": "no estimate"}"#),
            WorkflowStepStatus::Completed,
        )];
        assert_eq!(extract_llm_estimate_ms(&steps), None);
    }

    #[test]
    fn test_extract_llm_estimate_no_steps() {
        assert_eq!(extract_llm_estimate_ms(&[]), None);
    }

    // ── v2 tests ─────────────────────────────────────────────────────

    #[test]
    fn test_percentile_basic() {
        let sorted = vec![100, 200, 300, 400, 500];
        assert_eq!(percentile(&sorted, 0.0), 100);
        assert_eq!(percentile(&sorted, 0.5), 300);
        assert_eq!(percentile(&sorted, 1.0), 500);
    }

    #[test]
    fn test_percentile_empty() {
        assert_eq!(percentile(&[], 0.5), 0);
    }

    #[test]
    fn test_confidence_low_sample() {
        assert_eq!(compute_confidence(2, &[100, 200]), Confidence::Low);
    }

    #[test]
    fn test_confidence_medium_sample() {
        let vals: Vec<i64> = vec![100, 200, 300, 400, 500];
        assert_eq!(compute_confidence(5, &vals), Confidence::Medium);
    }

    #[test]
    fn test_confidence_high_sample_low_variance() {
        // All values within ~10% of each other → low CV → High confidence
        let vals: Vec<i64> = vec![100, 102, 98, 101, 99, 100, 103, 97, 101, 99];
        assert_eq!(compute_confidence(10, &vals), Confidence::High);
    }

    #[test]
    fn test_confidence_high_sample_high_variance() {
        // Wide spread → high CV → Medium confidence even with 10+ samples
        let vals: Vec<i64> = vec![10, 1000, 20, 900, 50, 800, 30, 700, 40, 600];
        assert_eq!(compute_confidence(10, &vals), Confidence::Medium);
    }

    #[test]
    fn test_estimate_with_confidence_few_samples() {
        let est = estimate_with_confidence(Some(600_000), &[]).unwrap();
        assert_eq!(est.point_ms, 600_000);
        assert_eq!(est.confidence, Confidence::Low);
        assert!(est.low_ms < est.point_ms);
        assert!(est.high_ms > est.point_ms);
    }

    #[test]
    fn test_estimate_with_confidence_enough_samples() {
        let hist = vec![800_000, 850_000, 900_000, 950_000, 1_000_000];
        let est = estimate_with_confidence(None, &hist).unwrap();
        assert_eq!(est.point_ms, 900_000); // median
        assert!(est.low_ms <= est.point_ms);
        assert!(est.high_ms >= est.point_ms);
        assert_eq!(est.confidence, Confidence::Medium);
    }

    #[test]
    fn test_estimate_all_steps() {
        let mut histories = HashMap::new();
        histories.insert("plan".to_string(), vec![60_000, 70_000, 80_000]);
        histories.insert("implement".to_string(), vec![300_000, 400_000, 500_000]);
        let ests = estimate_all_steps(&histories);
        assert!(ests.contains_key("plan"));
        assert!(ests.contains_key("implement"));
        assert_eq!(ests["plan"].point_ms, 70_000);
        assert_eq!(ests["implement"].point_ms, 400_000);
    }

    #[test]
    fn test_live_remaining_completed_steps_ignored() {
        let histories: HashMap<String, Vec<i64>> = [
            ("plan".into(), vec![60_000, 70_000, 80_000]),
            ("implement".into(), vec![300_000, 400_000, 500_000]),
        ]
        .into();
        let step_ests = estimate_all_steps(&histories);

        let steps = vec![
            make_named_step("plan", WorkflowStepStatus::Completed, None),
            make_named_step("implement", WorkflowStepStatus::Pending, None),
        ];
        let live = live_remaining_estimate(&steps, &step_ests).unwrap();
        // Only "implement" should contribute (plan is completed)
        assert_eq!(live.remaining_ms, step_ests["implement"].point_ms);
    }

    #[test]
    fn test_live_remaining_running_step_subtracts_elapsed() {
        let histories: HashMap<String, Vec<i64>> =
            [("build".into(), vec![120_000, 120_000, 120_000])].into();
        let step_ests = estimate_all_steps(&histories);

        let started_2m_ago = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        let steps = vec![make_named_step(
            "build",
            WorkflowStepStatus::Running,
            Some(&started_2m_ago),
        )];
        let live = live_remaining_estimate(&steps, &step_ests).unwrap();
        // 120s estimate - ~60s elapsed ≈ 60s remaining (allow 2s tolerance)
        assert!(live.remaining_ms > 58_000 && live.remaining_ms < 62_000);
    }

    #[test]
    fn test_live_remaining_no_estimates() {
        let step_ests = HashMap::new();
        let steps = vec![make_named_step(
            "unknown",
            WorkflowStepStatus::Pending,
            None,
        )];
        assert!(live_remaining_estimate(&steps, &step_ests).is_none());
    }

    // ── Test helpers ─────────────────────────────────────────────────

    fn make_step(structured_output: Option<&str>, status: WorkflowStepStatus) -> WorkflowRunStep {
        make_named_step("", status, None).with_structured_output(structured_output)
    }

    fn make_named_step(
        name: &str,
        status: WorkflowStepStatus,
        started_at: Option<&str>,
    ) -> WorkflowRunStep {
        WorkflowRunStep {
            id: String::new(),
            workflow_run_id: String::new(),
            step_name: name.to_string(),
            role: String::new(),
            can_commit: false,
            condition_expr: None,
            status,
            child_run_id: None,
            position: 0,
            started_at: started_at.map(String::from),
            ended_at: None,
            result_text: None,
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: None,
            markers_out: None,
            retry_count: 0,
            gate_type: None,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: None,
            output_file: None,
            gate_options: None,
            gate_selections: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }

    trait WithStructuredOutput {
        fn with_structured_output(self, output: Option<&str>) -> Self;
    }

    impl WithStructuredOutput for WorkflowRunStep {
        fn with_structured_output(mut self, output: Option<&str>) -> Self {
            self.structured_output = output.map(String::from);
            self
        }
    }
}
