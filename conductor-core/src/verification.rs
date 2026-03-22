//! Verification pipeline for workflow runs.
//!
//! Covers patterns:
//! - structured-evidence-directory@1.1.0
//! - acceptance-criteria-driven-verification@1.0.0
//! - evidence-based-task-verification@1.0.0
//! - prerequisite-verification-protocol@1.0.0
//! - critical-task-escalation@1.0.0

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::conductor_dir;
use crate::error::Result;

// ---------------------------------------------------------------------------
// Evidence directory (structured-evidence-directory@1.1.0)
// ---------------------------------------------------------------------------

/// Manages the evidence directory tree for a single workflow run.
///
/// Layout:
/// ```text
/// ~/.conductor/evidence/<run_id>/
///   report.md
///   <step_name>/
///     checklist.json
///     evidence/
///       test_output/
///       command_demos/
///       error_handling/
///       coverage/
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceDir {
    /// Root of the evidence tree for this run.
    pub root: PathBuf,
    pub run_id: String,
}

impl EvidenceDir {
    /// Base evidence directory: `~/.conductor/evidence/`.
    pub fn base_dir() -> PathBuf {
        conductor_dir().join("evidence")
    }

    /// Create a new `EvidenceDir` for the given workflow run, creating all
    /// directories on disk.
    pub fn create(run_id: &str) -> Result<Self> {
        let root = Self::base_dir().join(run_id);
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            run_id: run_id.to_string(),
        })
    }

    /// Create the sub-directory tree for a single step, returning its path.
    pub fn create_step_dir(&self, step_name: &str) -> Result<PathBuf> {
        let step_dir = self.root.join(step_name);
        let evidence = step_dir.join("evidence");
        for subdir in &["test_output", "command_demos", "error_handling", "coverage"] {
            std::fs::create_dir_all(evidence.join(subdir))?;
        }
        Ok(step_dir)
    }

    /// Path to the overall verification report for this run.
    pub fn report_path(&self) -> PathBuf {
        self.root.join("report.md")
    }

    /// Path to the checklist JSON for the given step.
    pub fn checklist_path(&self, step_name: &str) -> PathBuf {
        self.root.join(step_name).join("checklist.json")
    }

    /// Remove the entire evidence directory tree from disk.
    pub fn cleanup(&self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }

    /// List all run IDs that have evidence directories on disk.
    pub fn list_all() -> Result<Vec<String>> {
        let base = Self::base_dir();
        if !base.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(base)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// Acceptance criteria (acceptance-criteria-driven-verification@1.0.0)
// ---------------------------------------------------------------------------

/// The kind of evidence expected to satisfy a criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    TestOutput,
    CommandDemo,
    ErrorHandling,
    Coverage,
    ManualInspection,
}

impl EvidenceType {
    /// Classify a criterion description into an evidence type using keyword matching.
    pub fn classify(text: &str) -> Self {
        let lower = text.to_lowercase();
        if lower.contains("test") && (lower.contains("pass") || lower.contains("run")) {
            Self::TestOutput
        } else if lower.contains("coverage") {
            Self::Coverage
        } else if lower.contains("error") || lower.contains("fail") {
            Self::ErrorHandling
        } else if lower.contains("command") || lower.contains("output") || lower.contains("demo") {
            Self::CommandDemo
        } else {
            Self::ManualInspection
        }
    }

    /// Subdirectory name within the evidence tree.
    pub fn subdir(&self) -> &'static str {
        match self {
            Self::TestOutput => "test_output",
            Self::CommandDemo => "command_demos",
            Self::ErrorHandling => "error_handling",
            Self::Coverage => "coverage",
            Self::ManualInspection => "test_output", // falls back to test_output
        }
    }
}

impl fmt::Display for EvidenceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TestOutput => write!(f, "test_output"),
            Self::CommandDemo => write!(f, "command_demo"),
            Self::ErrorHandling => write!(f, "error_handling"),
            Self::Coverage => write!(f, "coverage"),
            Self::ManualInspection => write!(f, "manual_inspection"),
        }
    }
}

/// A single acceptance criterion attached to a workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Human-readable description (e.g. "All unit tests pass").
    pub text: String,
    /// The kind of evidence needed.
    pub evidence_type: EvidenceType,
}

impl AcceptanceCriterion {
    /// Parse a criterion from its textual description, auto-classifying the evidence type.
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let evidence_type = EvidenceType::classify(&text);
        Self {
            text,
            evidence_type,
        }
    }
}

// ---------------------------------------------------------------------------
// Verification result (evidence-based-task-verification@1.0.0)
// ---------------------------------------------------------------------------

/// Verdict for a single criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionVerdict {
    Pass,
    Fail,
    Skipped,
}

/// Result of evaluating a single acceptance criterion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionResult {
    pub criterion: AcceptanceCriterion,
    pub verdict: CriterionVerdict,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub evidence_path: Option<PathBuf>,
}

/// Aggregated verification result for a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub step_name: String,
    pub results: Vec<CriterionResult>,
    pub overall_pass: bool,
}

impl VerificationResult {
    /// Evaluate a set of criteria against an evidence directory.
    ///
    /// For each criterion, checks whether the corresponding evidence subdirectory
    /// contains at least one file. This is a baseline heuristic; more sophisticated
    /// evaluation strategies can be layered on top.
    pub fn evaluate(
        step_name: &str,
        criteria: &[AcceptanceCriterion],
        evidence_dir: &Path,
    ) -> Self {
        let results: Vec<CriterionResult> = criteria
            .iter()
            .map(|c| {
                let subdir = evidence_dir.join("evidence").join(c.evidence_type.subdir());
                let has_evidence = subdir.exists()
                    && std::fs::read_dir(&subdir)
                        .map(|mut rd| rd.next().is_some())
                        .unwrap_or(false);
                CriterionResult {
                    criterion: c.clone(),
                    verdict: if has_evidence {
                        CriterionVerdict::Pass
                    } else {
                        CriterionVerdict::Fail
                    },
                    expected: Some(format!("Evidence in {}", c.evidence_type.subdir())),
                    actual: if has_evidence {
                        Some("Evidence found".to_string())
                    } else {
                        Some("No evidence found".to_string())
                    },
                    evidence_path: if has_evidence { Some(subdir) } else { None },
                }
            })
            .collect();
        let overall_pass = results.iter().all(|r| r.verdict == CriterionVerdict::Pass);
        Self {
            step_name: step_name.to_string(),
            results,
            overall_pass,
        }
    }
}

// ---------------------------------------------------------------------------
// Prerequisite checks (prerequisite-verification-protocol@1.0.0)
// ---------------------------------------------------------------------------

/// A prerequisite that must be satisfied before a workflow step can execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PrerequisiteCheck {
    /// A file or directory must exist at the given path.
    FileExists { path: String },
    /// A command must be available on `$PATH`.
    CommandAvailable { command: String },
    /// A prior step must have completed successfully.
    StepCompleted { step_name: String },
}

/// Outcome of a single prerequisite check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrerequisiteResult {
    pub check: PrerequisiteCheck,
    pub passed: bool,
    pub message: String,
}

/// Verify all prerequisites, returning results for each.
///
/// `completed_steps` should contain the names of steps that have already
/// completed successfully in this run.
pub fn check_prerequisites(
    prerequisites: &[PrerequisiteCheck],
    working_dir: &Path,
    completed_steps: &[String],
) -> Vec<PrerequisiteResult> {
    prerequisites
        .iter()
        .map(|p| match p {
            PrerequisiteCheck::FileExists { path } => {
                let full = working_dir.join(path);
                let exists = full.exists();
                PrerequisiteResult {
                    check: p.clone(),
                    passed: exists,
                    message: if exists {
                        format!("File exists: {path}")
                    } else {
                        format!(
                            "File not found: {path} (looked in {})",
                            working_dir.display()
                        )
                    },
                }
            }
            PrerequisiteCheck::CommandAvailable { command } => {
                // Extract the binary name (first word) from the command string.
                let binary = command.split_whitespace().next().unwrap_or(command);
                let available = which_exists(binary);
                PrerequisiteResult {
                    check: p.clone(),
                    passed: available,
                    message: if available {
                        format!("Command available: {command}")
                    } else {
                        format!("Command not found: {binary}")
                    },
                }
            }
            PrerequisiteCheck::StepCompleted { step_name } => {
                let done = completed_steps.iter().any(|s| s == step_name);
                PrerequisiteResult {
                    check: p.clone(),
                    passed: done,
                    message: if done {
                        format!("Step completed: {step_name}")
                    } else {
                        format!("Step not yet completed: {step_name}")
                    },
                }
            }
        })
        .collect()
}

/// Check whether all prerequisites pass. Returns an error describing all
/// failures if any prerequisite is not met.
pub fn require_prerequisites(
    prerequisites: &[PrerequisiteCheck],
    working_dir: &Path,
    completed_steps: &[String],
) -> Result<()> {
    let results = check_prerequisites(prerequisites, working_dir, completed_steps);
    let failures: Vec<&PrerequisiteResult> = results.iter().filter(|r| !r.passed).collect();
    if failures.is_empty() {
        Ok(())
    } else {
        let msgs: Vec<String> = failures.iter().map(|f| f.message.clone()).collect();
        Err(crate::error::ConductorError::Workflow(format!(
            "Prerequisites not met: {}",
            msgs.join("; ")
        )))
    }
}

/// Check if a binary is reachable on `$PATH` without executing it.
fn which_exists(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(binary);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Critical escalation (critical-task-escalation@1.0.0)
// ---------------------------------------------------------------------------

/// Decision on whether a step requires human escalation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticalEscalation {
    pub step_name: String,
    pub reason: String,
    pub evidence_path: Option<PathBuf>,
    pub review_template: String,
}

impl CriticalEscalation {
    /// Build an escalation for a critical step that passed verification but
    /// needs human sign-off before proceeding.
    pub fn for_critical_pass(step_name: &str, evidence_path: Option<PathBuf>) -> Self {
        let evidence_info = evidence_path
            .as_ref()
            .map(|p| format!("\nEvidence directory: {}", p.display()))
            .unwrap_or_default();
        Self {
            step_name: step_name.to_string(),
            reason: format!(
                "Critical step '{step_name}' passed verification but requires human review"
            ),
            evidence_path,
            review_template: format!(
                "## Critical Step Review: {step_name}\n\
                 \n\
                 This step is marked as critical and requires human approval.\n\
                 {evidence_info}\n\
                 \n\
                 Please review the evidence and approve or reject this step."
            ),
        }
    }

    /// Determine whether a step should be escalated.
    ///
    /// Escalation fires only when a critical step *passes* verification.
    /// Failed critical steps go directly to `Failed` status without escalation.
    pub fn should_escalate(is_critical: bool, verification_passed: bool) -> bool {
        is_critical && verification_passed
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- EvidenceDir tests --

    #[test]
    fn evidence_dir_create_and_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CONDUCTOR_HOME", tmp.path());

        let ed = EvidenceDir::create("run-001").unwrap();
        assert!(ed.root.exists());
        assert_eq!(ed.run_id, "run-001");

        ed.cleanup().unwrap();
        assert!(!ed.root.exists());
    }

    #[test]
    fn evidence_dir_step_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CONDUCTOR_HOME", tmp.path());

        let ed = EvidenceDir::create("run-002").unwrap();
        let step = ed.create_step_dir("build").unwrap();
        assert!(step.join("evidence/test_output").is_dir());
        assert!(step.join("evidence/command_demos").is_dir());
        assert!(step.join("evidence/error_handling").is_dir());
        assert!(step.join("evidence/coverage").is_dir());

        ed.cleanup().unwrap();
    }

    #[test]
    fn evidence_dir_list_all() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CONDUCTOR_HOME", tmp.path());

        EvidenceDir::create("aaa").unwrap();
        EvidenceDir::create("bbb").unwrap();
        let ids = EvidenceDir::list_all().unwrap();
        assert!(ids.contains(&"aaa".to_string()));
        assert!(ids.contains(&"bbb".to_string()));
    }

    #[test]
    fn evidence_dir_report_and_checklist_paths() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CONDUCTOR_HOME", tmp.path());

        let ed = EvidenceDir::create("run-003").unwrap();
        assert!(ed.report_path().ends_with("report.md"));
        assert!(ed.checklist_path("lint").ends_with("lint/checklist.json"));

        ed.cleanup().unwrap();
    }

    // -- EvidenceType classification tests --

    #[test]
    fn classify_test_output() {
        assert_eq!(
            EvidenceType::classify("All unit tests pass"),
            EvidenceType::TestOutput
        );
        assert_eq!(
            EvidenceType::classify("Run integration tests"),
            EvidenceType::TestOutput
        );
    }

    #[test]
    fn classify_coverage() {
        assert_eq!(
            EvidenceType::classify("Code coverage above 80%"),
            EvidenceType::Coverage
        );
    }

    #[test]
    fn classify_error_handling() {
        assert_eq!(
            EvidenceType::classify("Error paths handled correctly"),
            EvidenceType::ErrorHandling
        );
        assert_eq!(
            EvidenceType::classify("Failure modes documented"),
            EvidenceType::ErrorHandling
        );
    }

    #[test]
    fn classify_command_demo() {
        assert_eq!(
            EvidenceType::classify("Command output matches expected"),
            EvidenceType::CommandDemo
        );
        assert_eq!(
            EvidenceType::classify("Demo the feature"),
            EvidenceType::CommandDemo
        );
    }

    #[test]
    fn classify_manual_inspection() {
        assert_eq!(
            EvidenceType::classify("Code is well-structured"),
            EvidenceType::ManualInspection
        );
    }

    // -- AcceptanceCriterion tests --

    #[test]
    fn criterion_from_text_auto_classifies() {
        let c = AcceptanceCriterion::from_text("All unit tests pass (cargo test)");
        assert_eq!(c.evidence_type, EvidenceType::TestOutput);
        assert_eq!(c.text, "All unit tests pass (cargo test)");
    }

    // -- VerificationResult tests --

    #[test]
    fn verification_all_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let step_dir = tmp.path();
        let evidence = step_dir.join("evidence").join("test_output");
        std::fs::create_dir_all(&evidence).unwrap();
        std::fs::write(evidence.join("output.txt"), "ok").unwrap();

        let criteria = vec![AcceptanceCriterion::from_text("All tests pass")];
        let result = VerificationResult::evaluate("build", &criteria, step_dir);
        assert!(result.overall_pass);
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].verdict, CriterionVerdict::Pass);
    }

    #[test]
    fn verification_some_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let step_dir = tmp.path();
        // Create test_output with evidence but leave coverage empty
        let test_dir = step_dir.join("evidence").join("test_output");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("output.txt"), "ok").unwrap();
        std::fs::create_dir_all(step_dir.join("evidence").join("coverage")).unwrap();

        let criteria = vec![
            AcceptanceCriterion::from_text("All tests pass"),
            AcceptanceCriterion::from_text("Code coverage above 80%"),
        ];
        let result = VerificationResult::evaluate("build", &criteria, step_dir);
        assert!(!result.overall_pass);
        assert_eq!(result.results[0].verdict, CriterionVerdict::Pass);
        assert_eq!(result.results[1].verdict, CriterionVerdict::Fail);
    }

    // -- Prerequisite tests --

    #[test]
    fn prerequisite_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();

        let checks = vec![PrerequisiteCheck::FileExists {
            path: "Cargo.toml".to_string(),
        }];
        let results = check_prerequisites(&checks, tmp.path(), &[]);
        assert!(results[0].passed);
    }

    #[test]
    fn prerequisite_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let checks = vec![PrerequisiteCheck::FileExists {
            path: "nonexistent.txt".to_string(),
        }];
        let results = check_prerequisites(&checks, tmp.path(), &[]);
        assert!(!results[0].passed);
        assert!(results[0].message.contains("File not found"));
    }

    #[test]
    fn prerequisite_command_available() {
        let checks = vec![PrerequisiteCheck::CommandAvailable {
            command: "git --version".to_string(),
        }];
        let results = check_prerequisites(&checks, Path::new("/tmp"), &[]);
        // git should be available in dev environments
        assert!(results[0].passed);
    }

    #[test]
    fn prerequisite_command_missing() {
        let checks = vec![PrerequisiteCheck::CommandAvailable {
            command: "definitely_not_a_real_command_12345".to_string(),
        }];
        let results = check_prerequisites(&checks, Path::new("/tmp"), &[]);
        assert!(!results[0].passed);
    }

    #[test]
    fn prerequisite_step_completed() {
        let checks = vec![PrerequisiteCheck::StepCompleted {
            step_name: "build".to_string(),
        }];
        let completed = vec!["build".to_string()];
        let results = check_prerequisites(&checks, Path::new("/tmp"), &completed);
        assert!(results[0].passed);
    }

    #[test]
    fn prerequisite_step_not_completed() {
        let checks = vec![PrerequisiteCheck::StepCompleted {
            step_name: "deploy".to_string(),
        }];
        let results = check_prerequisites(&checks, Path::new("/tmp"), &[]);
        assert!(!results[0].passed);
    }

    #[test]
    fn require_prerequisites_all_pass() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("ok.txt"), "").unwrap();
        let checks = vec![PrerequisiteCheck::FileExists {
            path: "ok.txt".to_string(),
        }];
        assert!(require_prerequisites(&checks, tmp.path(), &[]).is_ok());
    }

    #[test]
    fn require_prerequisites_some_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let checks = vec![
            PrerequisiteCheck::FileExists {
                path: "missing.txt".to_string(),
            },
            PrerequisiteCheck::FileExists {
                path: "also_missing.txt".to_string(),
            },
        ];
        let err = require_prerequisites(&checks, tmp.path(), &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing.txt"));
        assert!(msg.contains("also_missing.txt"));
    }

    // -- CriticalEscalation tests --

    #[test]
    fn escalation_critical_pass_should_escalate() {
        assert!(CriticalEscalation::should_escalate(true, true));
    }

    #[test]
    fn escalation_critical_fail_should_not_escalate() {
        assert!(!CriticalEscalation::should_escalate(true, false));
    }

    #[test]
    fn escalation_non_critical_pass_should_not_escalate() {
        assert!(!CriticalEscalation::should_escalate(false, true));
    }

    #[test]
    fn escalation_for_critical_pass_has_review_template() {
        let esc = CriticalEscalation::for_critical_pass("deploy", None);
        assert!(esc.review_template.contains("deploy"));
        assert!(esc.review_template.contains("Critical Step Review"));
    }

    #[test]
    fn escalation_with_evidence_path() {
        let esc =
            CriticalEscalation::for_critical_pass("deploy", Some(PathBuf::from("/tmp/evidence")));
        assert!(esc.review_template.contains("/tmp/evidence"));
        assert!(esc.evidence_path.is_some());
    }
}
