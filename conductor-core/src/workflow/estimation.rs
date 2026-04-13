//! Hybrid workflow time estimation.
//!
//! Blends two signal sources to estimate total workflow duration:
//! 1. **LLM estimate** — `estimated_minutes` from a plan step's structured output.
//! 2. **Historical data** — `total_duration_ms` from past completed runs of the same workflow.
//!
//! All functions are pure (no DB, no async) — callers fetch data and pass it in.

use chrono::Utc;

use super::types::WorkflowRunStep;

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

#[cfg(test)]
mod tests {
    use super::*;

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
            make_step(None),
            make_step(Some(r#"{"estimated_minutes": 15, "summary": "test"}"#)),
        ];
        assert_eq!(extract_llm_estimate_ms(&steps), Some(900_000));
    }

    #[test]
    fn test_extract_llm_estimate_missing() {
        let steps = vec![make_step(Some(r#"{"summary": "no estimate"}"#))];
        assert_eq!(extract_llm_estimate_ms(&steps), None);
    }

    #[test]
    fn test_extract_llm_estimate_no_steps() {
        assert_eq!(extract_llm_estimate_ms(&[]), None);
    }

    fn make_step(structured_output: Option<&str>) -> WorkflowRunStep {
        WorkflowRunStep {
            structured_output: structured_output.map(String::from),
            ..Default::default()
        }
    }
}
