//! Cross-domain composition patterns for workflow orchestration.
//!
//! This module provides higher-level composition primitives that build on
//! Wave 1 (retry, recovery, transitions) and Wave 3 (consistency, verification)
//! foundations. These patterns enable template-driven workflows, shared-state
//! communication, cross-cutting context injection, multi-stage verification
//! pipelines, the planner-executor-validator triad, domain applicability
//! filtering, and autonomous recovery cycles.
//!
//! Covers patterns:
//! - template-with-adaptation-propagation@1.0.0
//! - state-mediated-agent-communication@1.0.0
//! - cross-cutting-context-management@1.0.0
//! - gated-verification-pipeline@1.0.0
//! - agent-architecture-triad@1.0.0
//! - selective-domain-applicability-filter@1.0.0
//! - autonomous-recovery-cycle@1.0.0

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pattern 1: template-with-adaptation-propagation@1.0.0
// ---------------------------------------------------------------------------

/// A slot type constraining what values a template parameter accepts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SlotType {
    /// Free-form string value.
    Text,
    /// One of a fixed set of choices.
    Enum(Vec<String>),
    /// Integer value.
    Integer,
    /// Boolean flag.
    Boolean,
    /// File path (validated for existence at instantiation time).
    FilePath,
}

impl fmt::Display for SlotType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotType::Text => write!(f, "text"),
            SlotType::Enum(choices) => write!(f, "enum({})", choices.join("|")),
            SlotType::Integer => write!(f, "integer"),
            SlotType::Boolean => write!(f, "boolean"),
            SlotType::FilePath => write!(f, "filepath"),
        }
    }
}

/// A variant slot in a workflow template — a named parameter that can be
/// filled when instantiating the template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantSlot {
    /// Slot name used as `{{name}}` in the template skeleton.
    pub name: String,
    /// Type constraint for the slot value.
    pub slot_type: SlotType,
    /// Default value if none is supplied. `None` means required.
    pub default_value: Option<String>,
    /// Human-readable description of what this slot controls.
    pub description: String,
}

/// A workflow template with an invariant skeleton and variant slots.
///
/// The template engine resolves `{{slot_name}}` markers in the skeleton
/// with supplied or default values, producing a concrete workflow definition.
///
/// Builds on Wave 2's agent-template-standardization for template inheritance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTemplate {
    /// Template name (unique within the repository).
    pub name: String,
    /// The workflow definition text with `{{slot}}` markers.
    pub invariant_skeleton: String,
    /// Declared variant slots.
    pub variant_slots: Vec<VariantSlot>,
}

/// An adaptation parameter binding a slot name to a concrete value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptationParam {
    pub slot_name: String,
    pub value: String,
}

impl WorkflowTemplate {
    /// Instantiate the template by resolving all variant slots.
    ///
    /// Returns the concrete workflow definition text, or an error listing
    /// any required slots that were not provided.
    pub fn instantiate(
        &self,
        params: &[AdaptationParam],
    ) -> Result<String, TemplateInstantiationError> {
        let param_map: HashMap<&str, &str> = params
            .iter()
            .map(|p| (p.slot_name.as_str(), p.value.as_str()))
            .collect();

        let mut missing = Vec::new();
        let mut result = self.invariant_skeleton.clone();

        for slot in &self.variant_slots {
            let marker = format!("{{{{{}}}}}", slot.name);
            if let Some(value) = param_map.get(slot.name.as_str()) {
                result = result.replace(&marker, value);
            } else if let Some(ref default) = slot.default_value {
                result = result.replace(&marker, default);
            } else {
                missing.push(slot.name.clone());
            }
        }

        if missing.is_empty() {
            Ok(result)
        } else {
            Err(TemplateInstantiationError::MissingRequiredSlots(missing))
        }
    }
}

/// Errors arising during template instantiation.
#[derive(Debug, Clone)]
pub enum TemplateInstantiationError {
    /// One or more required slots were not provided and have no default.
    MissingRequiredSlots(Vec<String>),
}

impl fmt::Display for TemplateInstantiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateInstantiationError::MissingRequiredSlots(slots) => {
                write!(f, "missing required template slots: {}", slots.join(", "))
            }
        }
    }
}

impl std::error::Error for TemplateInstantiationError {}

// ---------------------------------------------------------------------------
// Pattern 2: state-mediated-agent-communication@1.0.0
// ---------------------------------------------------------------------------

/// A typed state entry in the shared state bus.
///
/// Agents communicate through SQLite by writing and reading state records.
/// This builds on Wave 2's agent communication DB tables (W2-T11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedStateEntry {
    /// Unique key for the state entry (e.g., "security_review.verdict").
    pub key: String,
    /// The value, serialized as JSON.
    pub value: serde_json::Value,
    /// Which agent wrote this entry.
    pub written_by: String,
    /// ISO 8601 timestamp of when the entry was written.
    pub written_at: String,
    /// Optional workflow run ID scoping this entry.
    pub workflow_run_id: Option<String>,
}

/// The shared state bus provides typed read/write access to state entries
/// backed by SQLite.
///
/// In the triple-role model:
/// - State record: `workflow_run_steps` status + metadata columns
/// - Communication artifact: `result_text` + `context_out` + `structured_output`
/// - Verification evidence: Wave 3's evidence directory system
#[derive(Debug, Clone)]
pub struct SharedStateBus {
    entries: HashMap<String, SharedStateEntry>,
}

impl SharedStateBus {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Write a state entry. Overwrites any previous entry with the same key.
    pub fn write(&mut self, entry: SharedStateEntry) {
        self.entries.insert(entry.key.clone(), entry);
    }

    /// Read a state entry by key.
    pub fn read(&self, key: &str) -> Option<&SharedStateEntry> {
        self.entries.get(key)
    }

    /// List all entries, optionally filtered by writing agent.
    pub fn list_entries(&self, written_by: Option<&str>) -> Vec<&SharedStateEntry> {
        self.entries
            .values()
            .filter(|e| {
                written_by
                    .map(|agent| e.written_by == agent)
                    .unwrap_or(true)
            })
            .collect()
    }

    /// Remove a state entry by key.
    pub fn remove(&mut self, key: &str) -> Option<SharedStateEntry> {
        self.entries.remove(key)
    }
}

impl Default for SharedStateBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pattern 3: cross-cutting-context-management@1.0.0
// ---------------------------------------------------------------------------

/// Repository-level configuration injected into agent sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoContextConfig {
    /// Primary language(s) of the repository.
    pub languages: Vec<String>,
    /// Frameworks detected or configured.
    pub frameworks: Vec<String>,
    /// Path to the repository root.
    pub root_path: String,
}

/// Global conductor configuration relevant to agent sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalContextConfig {
    /// Whether the user prefers verbose agent output.
    pub verbose: bool,
    /// Default timeout for agent operations in seconds.
    pub default_timeout_secs: u64,
}

/// Environment information injected into agent sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    /// Operating system (e.g., "darwin", "linux").
    pub os: String,
    /// Available CLI tools (e.g., ["gh", "jq", "bun"]).
    pub available_tools: Vec<String>,
    /// Whether running in CI.
    pub is_ci: bool,
}

/// Cross-cutting context injected into every agent session within a workflow.
///
/// Builds on Wave 3's error vocabulary propagation to provide a unified
/// context layer for all agents regardless of their role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCuttingContext {
    pub repo_config: RepoContextConfig,
    pub global_config: GlobalContextConfig,
    pub environment: EnvironmentInfo,
    /// Error categories from Wave 3's vocabulary, as string labels.
    /// Referenced by name to avoid circular dependency with Wave 3 types.
    pub error_vocabulary: Vec<String>,
    /// Optional per-workflow overrides.
    pub overrides: HashMap<String, serde_json::Value>,
}

impl CrossCuttingContext {
    /// Build a context from components.
    pub fn new(
        repo_config: RepoContextConfig,
        global_config: GlobalContextConfig,
        environment: EnvironmentInfo,
    ) -> Self {
        Self {
            repo_config,
            global_config,
            environment,
            error_vocabulary: Vec::new(),
            overrides: HashMap::new(),
        }
    }

    /// Apply a per-workflow override.
    pub fn with_override(mut self, key: String, value: serde_json::Value) -> Self {
        self.overrides.insert(key, value);
        self
    }

    /// Render the context as a string suitable for agent prompt injection.
    pub fn render_for_injection(&self) -> String {
        let mut lines = vec!["## Cross-Cutting Context".to_string()];
        lines.push(format!(
            "Repository: {} ({})",
            self.repo_config.root_path,
            self.repo_config.languages.join(", ")
        ));
        if !self.repo_config.frameworks.is_empty() {
            lines.push(format!(
                "Frameworks: {}",
                self.repo_config.frameworks.join(", ")
            ));
        }
        lines.push(format!("OS: {}", self.environment.os));
        lines.push(format!("CI: {}", self.environment.is_ci));
        if !self.error_vocabulary.is_empty() {
            lines.push(format!(
                "Error categories: {}",
                self.error_vocabulary.join(", ")
            ));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Pattern 4: gated-verification-pipeline@1.0.0
// ---------------------------------------------------------------------------

/// A stage in the verification pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PipelineStageKind {
    /// Lint checks (fastest, run first).
    Lint,
    /// Unit/integration test execution.
    Test,
    /// Integration or end-to-end test execution.
    Integration,
    /// Code review (human or agent).
    Review,
    /// Custom stage with a named identifier.
    Custom(String),
}

impl fmt::Display for PipelineStageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineStageKind::Lint => write!(f, "lint"),
            PipelineStageKind::Test => write!(f, "test"),
            PipelineStageKind::Integration => write!(f, "integration"),
            PipelineStageKind::Review => write!(f, "review"),
            PipelineStageKind::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// A single stage in the gated verification pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStage {
    /// Human-readable stage name.
    pub name: String,
    /// What kind of verification this stage performs.
    pub kind: PipelineStageKind,
    /// Minimum pass threshold (0.0 to 1.0). Stage fails if score is below.
    pub threshold: f64,
    /// Whether this stage is required for pipeline progression.
    pub required: bool,
    /// Maximum duration in seconds before the stage times out.
    pub timeout_secs: Option<u64>,
}

/// Result of executing a single pipeline stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage_name: String,
    pub passed: bool,
    pub score: f64,
    pub evidence: Vec<String>,
    pub error_message: Option<String>,
    pub duration_ms: u64,
}

/// Overall result of a pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    pub stage_results: Vec<StageResult>,
    pub overall_passed: bool,
    /// The stage that caused pipeline failure, if any.
    pub failed_at: Option<String>,
}

/// A gated verification pipeline: a sequence of stages where each must pass
/// before the next is attempted.
///
/// Composes Wave 3 primitives:
/// - Evidence directory system (evidence collection per stage)
/// - Acceptance criteria DSL (per-stage verification criteria)
/// - ConsistencyChecker (DESYNC detection at stage boundaries)
/// - ErrorCategory vocabulary (structured error reporting)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPipeline {
    pub stages: Vec<PipelineStage>,
}

impl VerificationPipeline {
    pub fn new(stages: Vec<PipelineStage>) -> Self {
        Self { stages }
    }

    /// Execute the pipeline, evaluating stages in order.
    ///
    /// The `evaluate` closure receives a stage and returns its result.
    /// Execution stops at the first required stage that fails.
    pub fn run_pipeline<F>(&self, mut evaluate: F) -> PipelineResult
    where
        F: FnMut(&PipelineStage) -> StageResult,
    {
        let mut stage_results = Vec::new();
        let mut failed_at = None;

        for stage in &self.stages {
            let result = evaluate(stage);
            let passed = result.passed;
            let stage_name = result.stage_name.clone();
            stage_results.push(result);

            if !passed && stage.required {
                failed_at = Some(stage_name);
                break;
            }
        }

        let overall_passed = failed_at.is_none();
        PipelineResult {
            stage_results,
            overall_passed,
            failed_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Pattern 5: agent-architecture-triad@1.0.0
// ---------------------------------------------------------------------------

/// Role within the agent architecture triad.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TriadRole {
    /// Layer 1: Plans the approach before execution.
    Planner,
    /// Layer 2: Executes the plan.
    Executor,
    /// Layer 3: Validates the execution output.
    Validator,
}

impl fmt::Display for TriadRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriadRole::Planner => write!(f, "planner"),
            TriadRole::Executor => write!(f, "executor"),
            TriadRole::Validator => write!(f, "validator"),
        }
    }
}

/// Metadata describing a triad workflow's role assignments.
///
/// The triad decomposes agent orchestration into three independently
/// evolvable layers, building on Wave 2's persona-based specialization,
/// builder-validator quality gates, and cross-agent delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriadMetadata {
    /// Step name assigned the planner role.
    pub planner_step: String,
    /// Step names assigned executor roles.
    pub executor_steps: Vec<String>,
    /// Step name assigned the validator role.
    pub validator_step: String,
    /// Optional quality gate step after validation.
    pub quality_gate_step: Option<String>,
}

impl TriadMetadata {
    /// Validate that the triad ordering is correct: planner must come before
    /// executors, executors before validator.
    ///
    /// `step_order` is the ordered list of step names in the workflow.
    pub fn validate_ordering(&self, step_order: &[String]) -> Result<(), TriadValidationError> {
        let pos = |name: &str| step_order.iter().position(|s| s == name);

        let planner_pos = pos(&self.planner_step)
            .ok_or_else(|| TriadValidationError::StepNotFound(self.planner_step.clone()))?;

        for exec_step in &self.executor_steps {
            let exec_pos = pos(exec_step)
                .ok_or_else(|| TriadValidationError::StepNotFound(exec_step.clone()))?;
            if exec_pos <= planner_pos {
                return Err(TriadValidationError::MisorderedSteps {
                    earlier: self.planner_step.clone(),
                    later: exec_step.clone(),
                });
            }
        }

        let validator_pos = pos(&self.validator_step)
            .ok_or_else(|| TriadValidationError::StepNotFound(self.validator_step.clone()))?;

        for exec_step in &self.executor_steps {
            let exec_pos = pos(exec_step).unwrap(); // already validated above
            if validator_pos <= exec_pos {
                return Err(TriadValidationError::MisorderedSteps {
                    earlier: exec_step.clone(),
                    later: self.validator_step.clone(),
                });
            }
        }

        Ok(())
    }
}

/// Errors from triad metadata validation.
#[derive(Debug, Clone)]
pub enum TriadValidationError {
    /// A step referenced in the triad was not found in the workflow.
    StepNotFound(String),
    /// Steps are out of order (earlier must come before later).
    MisorderedSteps { earlier: String, later: String },
}

impl fmt::Display for TriadValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriadValidationError::StepNotFound(name) => {
                write!(f, "triad step not found in workflow: {name}")
            }
            TriadValidationError::MisorderedSteps { earlier, later } => {
                write!(
                    f,
                    "triad ordering violated: '{earlier}' must come before '{later}'"
                )
            }
        }
    }
}

impl std::error::Error for TriadValidationError {}

// ---------------------------------------------------------------------------
// Pattern 6: selective-domain-applicability-filter@1.0.0
// ---------------------------------------------------------------------------

/// Applicability classification tier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ApplicabilityTier {
    /// High confidence the pattern/workflow applies.
    StrongCandidate,
    /// Some indicators present but not definitive.
    WeakCandidate,
    /// Pattern does not apply to this repository.
    NotCandidate,
}

impl fmt::Display for ApplicabilityTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApplicabilityTier::StrongCandidate => write!(f, "strong_candidate"),
            ApplicabilityTier::WeakCandidate => write!(f, "weak_candidate"),
            ApplicabilityTier::NotCandidate => write!(f, "not_candidate"),
        }
    }
}

/// A predicate that can be evaluated against repository characteristics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FilterPredicate {
    /// Repository contains a file at the given relative path.
    HasFile(String),
    /// Repository contains a directory at the given relative path.
    HasDirectory(String),
    /// Primary language matches.
    LanguageIs(String),
    /// Framework matches.
    FrameworkIs(String),
    /// Total file count exceeds the threshold.
    FileCountAbove(usize),
}

/// A single filter criterion binding a predicate to a tier classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCriterion {
    /// Human-readable name for this criterion.
    pub name: String,
    /// The predicate to evaluate.
    pub predicate: FilterPredicate,
    /// Tier assigned if the predicate matches.
    pub tier_if_matched: ApplicabilityTier,
    /// Explanation of why this criterion matters.
    pub rationale: String,
}

/// Characteristics of a repository used for filter evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCharacteristics {
    /// Files present in the repository (relative paths).
    pub files: Vec<String>,
    /// Directories present (relative paths).
    pub directories: Vec<String>,
    /// Primary language.
    pub language: Option<String>,
    /// Detected framework.
    pub framework: Option<String>,
    /// Total file count.
    pub file_count: usize,
}

/// Filter that evaluates repository characteristics against criteria
/// to determine pattern/workflow applicability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicabilityFilter {
    pub criteria: Vec<FilterCriterion>,
}

impl ApplicabilityFilter {
    pub fn new(criteria: Vec<FilterCriterion>) -> Self {
        Self { criteria }
    }

    /// Evaluate a predicate against repository characteristics.
    fn evaluate_predicate(pred: &FilterPredicate, repo: &RepoCharacteristics) -> bool {
        match pred {
            FilterPredicate::HasFile(path) => repo.files.iter().any(|f| f == path),
            FilterPredicate::HasDirectory(path) => repo.directories.iter().any(|d| d == path),
            FilterPredicate::LanguageIs(lang) => {
                repo.language.as_ref().map(|l| l == lang).unwrap_or(false)
            }
            FilterPredicate::FrameworkIs(fw) => {
                repo.framework.as_ref().map(|f| f == fw).unwrap_or(false)
            }
            FilterPredicate::FileCountAbove(threshold) => repo.file_count > *threshold,
        }
    }

    /// Classify a repository by evaluating all criteria.
    ///
    /// Returns the highest-priority tier among matched criteria.
    /// If no criteria match, returns `NotCandidate`.
    /// Priority: StrongCandidate > WeakCandidate > NotCandidate.
    pub fn classify(&self, repo: &RepoCharacteristics) -> ApplicabilityTier {
        let mut best = ApplicabilityTier::NotCandidate;

        for criterion in &self.criteria {
            if Self::evaluate_predicate(&criterion.predicate, repo) {
                match (&best, &criterion.tier_if_matched) {
                    (_, ApplicabilityTier::StrongCandidate) => {
                        return ApplicabilityTier::StrongCandidate;
                    }
                    (ApplicabilityTier::NotCandidate, ApplicabilityTier::WeakCandidate) => {
                        best = ApplicabilityTier::WeakCandidate;
                    }
                    _ => {}
                }
            }
        }

        best
    }
}

// ---------------------------------------------------------------------------
// Pattern 7: autonomous-recovery-cycle@1.0.0
// ---------------------------------------------------------------------------

/// Escalation tier for the recovery cycle, ordered from least to most drastic.
///
/// Composes Wave 1's emergency recovery protocol (4-tier ladder) with
/// Wave 1's bounded retry and Wave 3's ConsistencyChecker for detection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum EscalationTier {
    /// Tier 1: Retry the specific failed operation.
    TargetedFix,
    /// Tier 2: Reset step state, retry from wider context.
    BroaderFix,
    /// Tier 3: Checkpoint state, rebuild from known good. Requires opt-in.
    BackupAndRebuild,
    /// Tier 4: Backup everything, clear state, rebuild from scratch. Requires opt-in.
    Nuclear,
}

impl fmt::Display for EscalationTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EscalationTier::TargetedFix => write!(f, "targeted_fix"),
            EscalationTier::BroaderFix => write!(f, "broader_fix"),
            EscalationTier::BackupAndRebuild => write!(f, "backup_and_rebuild"),
            EscalationTier::Nuclear => write!(f, "nuclear"),
        }
    }
}

/// Detection mechanism that triggers the recovery cycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DetectionMechanism {
    /// Wave 3's ConsistencyChecker detects state divergence.
    ConsistencyCheck,
    /// Step failure triggers recovery.
    StepFailure,
    /// Explicit manual trigger.
    Manual,
}

/// Policy for when to escalate to human intervention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HumanEscalationPolicy {
    /// Escalate after exhausting all configured tiers.
    AfterAllTiers,
    /// Escalate after a specific tier.
    AfterTier(EscalationTier),
    /// Never auto-escalate (fail instead).
    Never,
}

/// Configuration for the autonomous recovery cycle.
///
/// References Wave 1's `RetryConfig` for inner retry parameters
/// and Wave 1's emergency recovery protocol for the escalation ladder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryCycleConfig {
    /// Max retries within a single escalation tier.
    pub retry_limit_per_tier: u32,
    /// Maximum escalation depth (how many tiers to attempt).
    pub max_escalation_depth: u32,
    /// What triggers the cycle.
    pub detection: DetectionMechanism,
    /// When to involve a human.
    pub human_escalation: HumanEscalationPolicy,
    /// Whether tier 3+ (BackupAndRebuild, Nuclear) are enabled.
    /// Defaults to false for safety.
    pub allow_destructive_tiers: bool,
}

impl Default for RecoveryCycleConfig {
    fn default() -> Self {
        Self {
            retry_limit_per_tier: 3,
            max_escalation_depth: 2, // Only TargetedFix and BroaderFix by default
            detection: DetectionMechanism::StepFailure,
            human_escalation: HumanEscalationPolicy::AfterAllTiers,
            allow_destructive_tiers: false,
        }
    }
}

/// Outcome of a recovery cycle attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecoveryOutcome {
    /// Recovery succeeded at the given tier after N retries.
    Recovered {
        tier: EscalationTier,
        retries_used: u32,
    },
    /// All tiers exhausted, escalating to human.
    HumanEscalation {
        last_tier: EscalationTier,
        diagnostic: String,
    },
    /// All tiers exhausted, no human escalation configured.
    Exhausted {
        last_tier: EscalationTier,
        last_error: String,
    },
}

/// The recovery cycle coordinates detection, retry, and escalation.
///
/// Composes:
/// - Detection: Wave 3's ConsistencyChecker (referenced, not imported)
/// - Retry: Wave 1's retry_with_backoff() (referenced, not imported)
/// - Escalation: Wave 1's emergency recovery protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryCycle {
    pub config: RecoveryCycleConfig,
}

impl RecoveryCycle {
    pub fn new(config: RecoveryCycleConfig) -> Self {
        Self { config }
    }

    /// The ordered list of tiers this cycle will attempt, respecting config.
    pub fn active_tiers(&self) -> Vec<EscalationTier> {
        let all_tiers = [
            EscalationTier::TargetedFix,
            EscalationTier::BroaderFix,
            EscalationTier::BackupAndRebuild,
            EscalationTier::Nuclear,
        ];

        all_tiers
            .into_iter()
            .take(self.config.max_escalation_depth as usize)
            .filter(|tier| {
                if !self.config.allow_destructive_tiers {
                    matches!(
                        tier,
                        EscalationTier::TargetedFix | EscalationTier::BroaderFix
                    )
                } else {
                    true
                }
            })
            .collect()
    }

    /// Attempt recovery using a provided operation closure.
    ///
    /// The `attempt` closure receives the current escalation tier and returns
    /// Ok(()) on success or Err(message) on failure.
    pub fn attempt_recovery<F>(&self, mut attempt: F) -> RecoveryOutcome
    where
        F: FnMut(&EscalationTier) -> Result<(), String>,
    {
        let tiers = self.active_tiers();
        let mut last_tier = EscalationTier::TargetedFix;
        let mut last_error = String::new();

        for tier in &tiers {
            last_tier = tier.clone();

            for retry in 0..self.config.retry_limit_per_tier {
                match attempt(tier) {
                    Ok(()) => {
                        return RecoveryOutcome::Recovered {
                            tier: tier.clone(),
                            retries_used: retry + 1,
                        };
                    }
                    Err(e) => {
                        last_error = e;
                    }
                }
            }
        }

        // All tiers exhausted
        match &self.config.human_escalation {
            HumanEscalationPolicy::AfterAllTiers => RecoveryOutcome::HumanEscalation {
                last_tier,
                diagnostic: last_error,
            },
            HumanEscalationPolicy::AfterTier(trigger_tier) => {
                if last_tier >= *trigger_tier {
                    RecoveryOutcome::HumanEscalation {
                        last_tier,
                        diagnostic: last_error,
                    }
                } else {
                    RecoveryOutcome::Exhausted {
                        last_tier,
                        last_error,
                    }
                }
            }
            HumanEscalationPolicy::Never => RecoveryOutcome::Exhausted {
                last_tier,
                last_error,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pattern 1: Template tests --

    #[test]
    fn template_instantiation_fills_slots() {
        let template = WorkflowTemplate {
            name: "test-template".to_string(),
            invariant_skeleton: "agent: {{agent_name}}\ntimeout: {{timeout}}".to_string(),
            variant_slots: vec![
                VariantSlot {
                    name: "agent_name".to_string(),
                    slot_type: SlotType::Text,
                    default_value: None,
                    description: "Agent to use".to_string(),
                },
                VariantSlot {
                    name: "timeout".to_string(),
                    slot_type: SlotType::Integer,
                    default_value: Some("300".to_string()),
                    description: "Timeout in seconds".to_string(),
                },
            ],
        };

        let result = template
            .instantiate(&[AdaptationParam {
                slot_name: "agent_name".to_string(),
                value: "claude".to_string(),
            }])
            .unwrap();

        assert_eq!(result, "agent: claude\ntimeout: 300");
    }

    #[test]
    fn template_instantiation_errors_on_missing_required_slot() {
        let template = WorkflowTemplate {
            name: "test".to_string(),
            invariant_skeleton: "{{required_slot}}".to_string(),
            variant_slots: vec![VariantSlot {
                name: "required_slot".to_string(),
                slot_type: SlotType::Text,
                default_value: None,
                description: "A required slot".to_string(),
            }],
        };

        let err = template.instantiate(&[]).unwrap_err();
        match err {
            TemplateInstantiationError::MissingRequiredSlots(slots) => {
                assert_eq!(slots, vec!["required_slot"]);
            }
        }
    }

    #[test]
    fn template_preserves_invariant_skeleton() {
        let template = WorkflowTemplate {
            name: "test".to_string(),
            invariant_skeleton: "fixed-prefix:{{slot}}:fixed-suffix".to_string(),
            variant_slots: vec![VariantSlot {
                name: "slot".to_string(),
                slot_type: SlotType::Text,
                default_value: None,
                description: "test".to_string(),
            }],
        };

        let result = template
            .instantiate(&[AdaptationParam {
                slot_name: "slot".to_string(),
                value: "VALUE".to_string(),
            }])
            .unwrap();

        assert!(result.starts_with("fixed-prefix:"));
        assert!(result.ends_with(":fixed-suffix"));
        assert!(result.contains("VALUE"));
    }

    // -- Pattern 2: SharedStateBus tests --

    #[test]
    fn shared_state_bus_write_and_read() {
        let mut bus = SharedStateBus::new();
        bus.write(SharedStateEntry {
            key: "review.verdict".to_string(),
            value: serde_json::json!("approved"),
            written_by: "security_agent".to_string(),
            written_at: "2026-01-01T00:00:00Z".to_string(),
            workflow_run_id: None,
        });

        let entry = bus.read("review.verdict").unwrap();
        assert_eq!(entry.value, serde_json::json!("approved"));
        assert_eq!(entry.written_by, "security_agent");
    }

    #[test]
    fn shared_state_bus_overwrites_on_duplicate_key() {
        let mut bus = SharedStateBus::new();
        bus.write(SharedStateEntry {
            key: "status".to_string(),
            value: serde_json::json!("pending"),
            written_by: "agent_a".to_string(),
            written_at: "2026-01-01T00:00:00Z".to_string(),
            workflow_run_id: None,
        });
        bus.write(SharedStateEntry {
            key: "status".to_string(),
            value: serde_json::json!("done"),
            written_by: "agent_b".to_string(),
            written_at: "2026-01-01T00:01:00Z".to_string(),
            workflow_run_id: None,
        });

        let entry = bus.read("status").unwrap();
        assert_eq!(entry.value, serde_json::json!("done"));
        assert_eq!(entry.written_by, "agent_b");
    }

    // -- Pattern 3: CrossCuttingContext tests --

    #[test]
    fn cross_cutting_context_renders_for_injection() {
        let ctx = CrossCuttingContext::new(
            RepoContextConfig {
                languages: vec!["rust".to_string()],
                frameworks: vec!["axum".to_string()],
                root_path: "/home/user/project".to_string(),
            },
            GlobalContextConfig {
                verbose: false,
                default_timeout_secs: 300,
            },
            EnvironmentInfo {
                os: "linux".to_string(),
                available_tools: vec!["gh".to_string()],
                is_ci: false,
            },
        );

        let rendered = ctx.render_for_injection();
        assert!(rendered.contains("rust"));
        assert!(rendered.contains("axum"));
        assert!(rendered.contains("linux"));
    }

    #[test]
    fn cross_cutting_context_supports_overrides() {
        let ctx = CrossCuttingContext::new(
            RepoContextConfig {
                languages: vec!["typescript".to_string()],
                frameworks: vec![],
                root_path: "/tmp/test".to_string(),
            },
            GlobalContextConfig {
                verbose: true,
                default_timeout_secs: 60,
            },
            EnvironmentInfo {
                os: "darwin".to_string(),
                available_tools: vec![],
                is_ci: true,
            },
        )
        .with_override("custom_key".to_string(), serde_json::json!("custom_val"));

        assert_eq!(
            ctx.overrides.get("custom_key"),
            Some(&serde_json::json!("custom_val"))
        );
    }

    // -- Pattern 4: VerificationPipeline tests --

    #[test]
    fn pipeline_runs_stages_in_order_and_stops_on_failure() {
        let pipeline = VerificationPipeline::new(vec![
            PipelineStage {
                name: "lint".to_string(),
                kind: PipelineStageKind::Lint,
                threshold: 0.8,
                required: true,
                timeout_secs: Some(60),
            },
            PipelineStage {
                name: "test".to_string(),
                kind: PipelineStageKind::Test,
                threshold: 0.9,
                required: true,
                timeout_secs: Some(300),
            },
            PipelineStage {
                name: "review".to_string(),
                kind: PipelineStageKind::Review,
                threshold: 0.7,
                required: true,
                timeout_secs: None,
            },
        ]);

        let result = pipeline.run_pipeline(|stage| StageResult {
            stage_name: stage.name.clone(),
            passed: stage.name != "test", // test stage fails
            score: if stage.name == "test" { 0.5 } else { 1.0 },
            evidence: vec![],
            error_message: if stage.name == "test" {
                Some("tests failed".to_string())
            } else {
                None
            },
            duration_ms: 100,
        });

        assert!(!result.overall_passed);
        assert_eq!(result.failed_at, Some("test".to_string()));
        assert_eq!(result.stage_results.len(), 2); // stopped after test
    }

    #[test]
    fn pipeline_succeeds_when_all_stages_pass() {
        let pipeline = VerificationPipeline::new(vec![
            PipelineStage {
                name: "lint".to_string(),
                kind: PipelineStageKind::Lint,
                threshold: 0.8,
                required: true,
                timeout_secs: None,
            },
            PipelineStage {
                name: "test".to_string(),
                kind: PipelineStageKind::Test,
                threshold: 0.9,
                required: true,
                timeout_secs: None,
            },
        ]);

        let result = pipeline.run_pipeline(|stage| StageResult {
            stage_name: stage.name.clone(),
            passed: true,
            score: 1.0,
            evidence: vec!["all green".to_string()],
            error_message: None,
            duration_ms: 50,
        });

        assert!(result.overall_passed);
        assert!(result.failed_at.is_none());
        assert_eq!(result.stage_results.len(), 2);
    }

    #[test]
    fn pipeline_continues_past_non_required_failure() {
        let pipeline = VerificationPipeline::new(vec![
            PipelineStage {
                name: "optional-lint".to_string(),
                kind: PipelineStageKind::Lint,
                threshold: 1.0,
                required: false,
                timeout_secs: None,
            },
            PipelineStage {
                name: "required-test".to_string(),
                kind: PipelineStageKind::Test,
                threshold: 0.9,
                required: true,
                timeout_secs: None,
            },
        ]);

        let result = pipeline.run_pipeline(|stage| StageResult {
            stage_name: stage.name.clone(),
            passed: stage.name != "optional-lint",
            score: if stage.name == "optional-lint" {
                0.5
            } else {
                1.0
            },
            evidence: vec![],
            error_message: None,
            duration_ms: 10,
        });

        assert!(result.overall_passed);
        assert_eq!(result.stage_results.len(), 2);
    }

    // -- Pattern 5: Triad tests --

    #[test]
    fn triad_validates_correct_ordering() {
        let triad = TriadMetadata {
            planner_step: "plan".to_string(),
            executor_steps: vec!["execute".to_string()],
            validator_step: "validate".to_string(),
            quality_gate_step: None,
        };

        let order = vec![
            "plan".to_string(),
            "execute".to_string(),
            "validate".to_string(),
        ];

        assert!(triad.validate_ordering(&order).is_ok());
    }

    #[test]
    fn triad_rejects_misordered_steps() {
        let triad = TriadMetadata {
            planner_step: "plan".to_string(),
            executor_steps: vec!["execute".to_string()],
            validator_step: "validate".to_string(),
            quality_gate_step: None,
        };

        // Validator before executor
        let order = vec![
            "plan".to_string(),
            "validate".to_string(),
            "execute".to_string(),
        ];

        assert!(triad.validate_ordering(&order).is_err());
    }

    #[test]
    fn triad_rejects_missing_step() {
        let triad = TriadMetadata {
            planner_step: "plan".to_string(),
            executor_steps: vec!["execute".to_string()],
            validator_step: "validate".to_string(),
            quality_gate_step: None,
        };

        let order = vec!["plan".to_string(), "execute".to_string()];

        let err = triad.validate_ordering(&order).unwrap_err();
        match err {
            TriadValidationError::StepNotFound(name) => assert_eq!(name, "validate"),
            _ => panic!("expected StepNotFound"),
        }
    }

    // -- Pattern 6: ApplicabilityFilter tests --

    #[test]
    fn filter_classifies_strong_candidate() {
        let filter = ApplicabilityFilter::new(vec![FilterCriterion {
            name: "has cargo toml".to_string(),
            predicate: FilterPredicate::HasFile("Cargo.toml".to_string()),
            tier_if_matched: ApplicabilityTier::StrongCandidate,
            rationale: "Rust project detected".to_string(),
        }]);

        let repo = RepoCharacteristics {
            files: vec!["Cargo.toml".to_string(), "src/main.rs".to_string()],
            directories: vec!["src".to_string()],
            language: Some("rust".to_string()),
            framework: None,
            file_count: 50,
        };

        assert_eq!(filter.classify(&repo), ApplicabilityTier::StrongCandidate);
    }

    #[test]
    fn filter_classifies_not_candidate_when_no_criteria_match() {
        let filter = ApplicabilityFilter::new(vec![FilterCriterion {
            name: "has package.json".to_string(),
            predicate: FilterPredicate::HasFile("package.json".to_string()),
            tier_if_matched: ApplicabilityTier::StrongCandidate,
            rationale: "JS project".to_string(),
        }]);

        let repo = RepoCharacteristics {
            files: vec!["Cargo.toml".to_string()],
            directories: vec![],
            language: Some("rust".to_string()),
            framework: None,
            file_count: 10,
        };

        assert_eq!(filter.classify(&repo), ApplicabilityTier::NotCandidate);
    }

    #[test]
    fn filter_language_predicate_works() {
        let filter = ApplicabilityFilter::new(vec![FilterCriterion {
            name: "is python".to_string(),
            predicate: FilterPredicate::LanguageIs("python".to_string()),
            tier_if_matched: ApplicabilityTier::WeakCandidate,
            rationale: "Python project".to_string(),
        }]);

        let repo = RepoCharacteristics {
            files: vec![],
            directories: vec![],
            language: Some("python".to_string()),
            framework: None,
            file_count: 5,
        };

        assert_eq!(filter.classify(&repo), ApplicabilityTier::WeakCandidate);
    }

    #[test]
    fn filter_file_count_predicate_works() {
        let filter = ApplicabilityFilter::new(vec![FilterCriterion {
            name: "large repo".to_string(),
            predicate: FilterPredicate::FileCountAbove(100),
            tier_if_matched: ApplicabilityTier::WeakCandidate,
            rationale: "Large repository".to_string(),
        }]);

        let small_repo = RepoCharacteristics {
            files: vec![],
            directories: vec![],
            language: None,
            framework: None,
            file_count: 50,
        };

        let large_repo = RepoCharacteristics {
            files: vec![],
            directories: vec![],
            language: None,
            framework: None,
            file_count: 200,
        };

        assert_eq!(
            filter.classify(&small_repo),
            ApplicabilityTier::NotCandidate
        );
        assert_eq!(
            filter.classify(&large_repo),
            ApplicabilityTier::WeakCandidate
        );
    }

    // -- Pattern 7: RecoveryCycle tests --

    #[test]
    fn recovery_cycle_succeeds_at_first_tier() {
        let cycle = RecoveryCycle::new(RecoveryCycleConfig {
            retry_limit_per_tier: 2,
            max_escalation_depth: 2,
            detection: DetectionMechanism::StepFailure,
            human_escalation: HumanEscalationPolicy::AfterAllTiers,
            allow_destructive_tiers: false,
        });

        let mut call_count = 0;
        let outcome = cycle.attempt_recovery(|_tier| {
            call_count += 1;
            if call_count >= 2 {
                Ok(())
            } else {
                Err("transient failure".to_string())
            }
        });

        match outcome {
            RecoveryOutcome::Recovered { tier, retries_used } => {
                assert_eq!(tier, EscalationTier::TargetedFix);
                assert_eq!(retries_used, 2);
            }
            _ => panic!("expected Recovered"),
        }
    }

    #[test]
    fn recovery_cycle_escalates_to_human() {
        let cycle = RecoveryCycle::new(RecoveryCycleConfig {
            retry_limit_per_tier: 1,
            max_escalation_depth: 2,
            detection: DetectionMechanism::StepFailure,
            human_escalation: HumanEscalationPolicy::AfterAllTiers,
            allow_destructive_tiers: false,
        });

        let outcome = cycle.attempt_recovery(|_tier| Err("persistent failure".to_string()));

        match outcome {
            RecoveryOutcome::HumanEscalation {
                last_tier,
                diagnostic,
            } => {
                assert_eq!(last_tier, EscalationTier::BroaderFix);
                assert!(diagnostic.contains("persistent failure"));
            }
            _ => panic!("expected HumanEscalation"),
        }
    }

    #[test]
    fn recovery_cycle_exhausted_when_no_human_escalation() {
        let cycle = RecoveryCycle::new(RecoveryCycleConfig {
            retry_limit_per_tier: 1,
            max_escalation_depth: 1,
            detection: DetectionMechanism::StepFailure,
            human_escalation: HumanEscalationPolicy::Never,
            allow_destructive_tiers: false,
        });

        let outcome = cycle.attempt_recovery(|_tier| Err("fatal".to_string()));

        match outcome {
            RecoveryOutcome::Exhausted {
                last_tier,
                last_error,
            } => {
                assert_eq!(last_tier, EscalationTier::TargetedFix);
                assert_eq!(last_error, "fatal");
            }
            _ => panic!("expected Exhausted"),
        }
    }

    #[test]
    fn recovery_cycle_respects_destructive_tier_flag() {
        let non_destructive = RecoveryCycle::new(RecoveryCycleConfig {
            retry_limit_per_tier: 1,
            max_escalation_depth: 4,
            detection: DetectionMechanism::StepFailure,
            human_escalation: HumanEscalationPolicy::Never,
            allow_destructive_tiers: false,
        });

        // With destructive disabled, only 2 tiers active
        assert_eq!(non_destructive.active_tiers().len(), 2);

        let destructive = RecoveryCycle::new(RecoveryCycleConfig {
            retry_limit_per_tier: 1,
            max_escalation_depth: 4,
            detection: DetectionMechanism::StepFailure,
            human_escalation: HumanEscalationPolicy::Never,
            allow_destructive_tiers: true,
        });

        assert_eq!(destructive.active_tiers().len(), 4);
    }
}
