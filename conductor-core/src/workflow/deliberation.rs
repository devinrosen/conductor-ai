//! Multi-agent deliberation patterns for structured decision-making.
//!
//! This module provides the facilitator-delegate separation, namespaced
//! decision IDs, parallel first-round independence, and reconciliation
//! strategies for merging divergent delegate outputs.
//!
//! Covers patterns:
//! - facilitator-delegate-separation@1.0.0
//! - namespace-separated-decision-ids@1.0.0
//! - parallel-first-round-independence@1.0.0

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pattern 8: facilitator-delegate-separation@1.0.0
// ---------------------------------------------------------------------------

/// Role constraint applied to a workflow step's agent invocation.
///
/// Builds on Wave 2's council-decision-architecture to enforce separation
/// between the facilitator (who coordinates) and delegates (who contribute).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RoleConstraint {
    /// The agent is a facilitator: must never simulate delegate responses.
    Facilitator,
    /// The agent is a delegate: produces independent assessments.
    Delegate,
    /// No role constraint applied.
    Unconstrained,
}

impl fmt::Display for RoleConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoleConstraint::Facilitator => write!(f, "facilitator"),
            RoleConstraint::Delegate => write!(f, "delegate"),
            RoleConstraint::Unconstrained => write!(f, "unconstrained"),
        }
    }
}

/// The anti-roleplay instruction injected into facilitator agent prompts.
pub const FACILITATOR_ANTI_ROLEPLAY_INSTRUCTION: &str = "\
CRITICAL: You are the facilitator. You must NEVER generate content attributed to \
delegate agents. All agent responses must come from actual Task tool call invocations. \
Never simulate or summarize what an agent would say.";

/// A facilitator role definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacilitatorRole {
    /// Agent name acting as facilitator.
    pub agent_name: String,
    /// Whether anti-roleplay injection is active.
    pub anti_roleplay_enabled: bool,
}

/// A delegate role definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateRole {
    /// Agent name acting as delegate.
    pub agent_name: String,
    /// Domain expertise area (e.g., "security", "performance").
    pub domain: String,
}

/// A deliberation session composed of a facilitator and multiple delegates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliberationSession {
    /// Session identifier.
    pub session_id: String,
    /// The facilitator role.
    pub facilitator: FacilitatorRole,
    /// Delegate roles participating in the session.
    pub delegates: Vec<DelegateRole>,
    /// Current round number (1-based).
    pub current_round: u32,
    /// Maximum number of deliberation rounds.
    pub max_rounds: u32,
}

impl DeliberationSession {
    /// Create a new deliberation session.
    pub fn new(
        session_id: String,
        facilitator: FacilitatorRole,
        delegates: Vec<DelegateRole>,
        max_rounds: u32,
    ) -> Self {
        Self {
            session_id,
            facilitator,
            delegates,
            current_round: 1,
            max_rounds,
        }
    }

    /// Generate the prompt injection for the facilitator, including
    /// anti-roleplay instruction if enabled.
    pub fn facilitator_prompt_injection(&self) -> Option<String> {
        if self.facilitator.anti_roleplay_enabled {
            Some(FACILITATOR_ANTI_ROLEPLAY_INSTRUCTION.to_string())
        } else {
            None
        }
    }

    /// Advance to the next round. Returns false if max rounds exceeded.
    pub fn advance_round(&mut self) -> bool {
        if self.current_round < self.max_rounds {
            self.current_round += 1;
            true
        } else {
            false
        }
    }

    /// Whether this is the first round (delegates should work independently).
    pub fn is_first_round(&self) -> bool {
        self.current_round == 1
    }
}

// ---------------------------------------------------------------------------
// Pattern 9: namespace-separated-decision-ids@1.0.0
// ---------------------------------------------------------------------------

/// A namespace for grouping related decisions.
///
/// Namespaces prevent ID collisions when multiple workflows or agents
/// create decisions concurrently. Builds on Wave 2's decision log
/// shared memory (W2-T11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionNamespace {
    /// 2-4 uppercase character prefix (e.g., "ARCH", "SEC", "PERF").
    pub prefix: String,
    /// Human-readable description of the namespace scope.
    pub description: String,
    /// Optional workflow run ID scoping this namespace.
    pub workflow_run_id: Option<String>,
}

impl DecisionNamespace {
    /// Create a new namespace. Returns an error if the prefix is invalid.
    pub fn new(
        prefix: String,
        description: String,
        workflow_run_id: Option<String>,
    ) -> Result<Self, DecisionNamespaceError> {
        if prefix.len() < 2 || prefix.len() > 4 {
            return Err(DecisionNamespaceError::InvalidPrefix(format!(
                "prefix must be 2-4 characters, got {}",
                prefix.len()
            )));
        }
        if !prefix.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(DecisionNamespaceError::InvalidPrefix(
                "prefix must be all uppercase ASCII".to_string(),
            ));
        }
        Ok(Self {
            prefix,
            description,
            workflow_run_id,
        })
    }
}

/// Errors from namespace operations.
#[derive(Debug, Clone)]
pub enum DecisionNamespaceError {
    /// Prefix does not meet format requirements.
    InvalidPrefix(String),
    /// A namespace with this prefix already exists.
    DuplicatePrefix(String),
}

impl fmt::Display for DecisionNamespaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecisionNamespaceError::InvalidPrefix(msg) => {
                write!(f, "invalid namespace prefix: {msg}")
            }
            DecisionNamespaceError::DuplicatePrefix(prefix) => {
                write!(f, "namespace prefix already exists: {prefix}")
            }
        }
    }
}

impl std::error::Error for DecisionNamespaceError {}

/// A namespaced decision identifier in `{PREFIX}-{NNN}` format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NamespacedDecisionId {
    /// Namespace prefix (e.g., "ARCH").
    pub namespace: String,
    /// Sequence number within the namespace.
    pub sequence_num: u32,
}

impl NamespacedDecisionId {
    /// Create a new decision ID.
    pub fn new(namespace: String, sequence_num: u32) -> Self {
        Self {
            namespace,
            sequence_num,
        }
    }

    /// Format as `{PREFIX}-{NNN}`.
    pub fn format(&self) -> String {
        format!("{}-{}", self.namespace, self.sequence_num)
    }

    /// Parse from `{PREFIX}-{NNN}` format.
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.splitn(2, '-').collect();
        if parts.len() != 2 {
            return Err(format!("expected FORMAT 'PREFIX-NNN', got '{s}'"));
        }
        let sequence_num: u32 = parts[1]
            .parse()
            .map_err(|_| format!("invalid sequence number: '{}'", parts[1]))?;
        Ok(Self {
            namespace: parts[0].to_string(),
            sequence_num,
        })
    }
}

impl fmt::Display for NamespacedDecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format())
    }
}

/// A decision log entry with namespaced identification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    /// Namespaced decision ID.
    pub id: NamespacedDecisionId,
    /// Workflow run that produced this decision.
    pub workflow_run_id: Option<String>,
    /// Step name that produced this decision.
    pub step_name: Option<String>,
    /// Agent that made the decision.
    pub agent_name: Option<String>,
    /// Decision content.
    pub content: String,
    /// Rationale for the decision.
    pub rationale: Option<String>,
    /// ISO 8601 timestamp.
    pub created_at: String,
}

/// Registry managing decision namespaces and ID allocation.
#[derive(Debug, Clone)]
pub struct DecisionRegistry {
    namespaces: HashMap<String, DecisionNamespace>,
    /// Next sequence number per namespace.
    next_sequence: HashMap<String, u32>,
}

impl DecisionRegistry {
    pub fn new() -> Self {
        Self {
            namespaces: HashMap::new(),
            next_sequence: HashMap::new(),
        }
    }

    /// Register a new namespace.
    pub fn register_namespace(
        &mut self,
        namespace: DecisionNamespace,
    ) -> Result<(), DecisionNamespaceError> {
        if self.namespaces.contains_key(&namespace.prefix) {
            return Err(DecisionNamespaceError::DuplicatePrefix(
                namespace.prefix.clone(),
            ));
        }
        self.next_sequence.insert(namespace.prefix.clone(), 1);
        self.namespaces.insert(namespace.prefix.clone(), namespace);
        Ok(())
    }

    /// Allocate the next decision ID in a namespace.
    pub fn next_id(&mut self, namespace_prefix: &str) -> Result<NamespacedDecisionId, String> {
        let seq = self
            .next_sequence
            .get_mut(namespace_prefix)
            .ok_or_else(|| format!("namespace not registered: {namespace_prefix}"))?;

        let id = NamespacedDecisionId::new(namespace_prefix.to_string(), *seq);
        *seq += 1;
        Ok(id)
    }

    /// Get a registered namespace by prefix.
    pub fn get_namespace(&self, prefix: &str) -> Option<&DecisionNamespace> {
        self.namespaces.get(prefix)
    }
}

impl Default for DecisionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pattern 10: parallel-first-round-independence@1.0.0
// ---------------------------------------------------------------------------

/// Synchronization mode for parallel agent execution.
///
/// Controls how delegate outputs are collected and presented after
/// parallel execution. Builds on Wave 2's ParallelNode execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SyncMode {
    /// Collect all results before presenting any (independence guarantee).
    CollectAll,
    /// Present results as they arrive (no independence guarantee).
    StreamAsComplete,
    /// Return after first successful result.
    FirstSuccess,
}

impl fmt::Display for SyncMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncMode::CollectAll => write!(f, "collect_all"),
            SyncMode::StreamAsComplete => write!(f, "stream_as_complete"),
            SyncMode::FirstSuccess => write!(f, "first_success"),
        }
    }
}

/// A single delegate's output from a parallel round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateOutput {
    /// Name of the delegate agent.
    pub agent_name: String,
    /// Domain of expertise.
    pub domain: String,
    /// The output content.
    pub content: String,
    /// Whether the delegate completed successfully.
    pub succeeded: bool,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

/// A parallel round collects delegate outputs with a synchronization barrier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelRound {
    /// Round number (1-based).
    pub round_number: u32,
    /// Synchronization mode.
    pub sync_mode: SyncMode,
    /// Per-delegate timeout in seconds.
    pub per_delegate_timeout_secs: Option<u64>,
    /// Collected outputs (populated after barrier).
    pub outputs: Vec<DelegateOutput>,
}

impl ParallelRound {
    /// Create a new parallel round.
    pub fn new(round_number: u32, sync_mode: SyncMode) -> Self {
        Self {
            round_number,
            sync_mode,
            per_delegate_timeout_secs: None,
            outputs: Vec::new(),
        }
    }

    /// Add a delegate output to the round.
    pub fn add_output(&mut self, output: DelegateOutput) {
        self.outputs.push(output);
    }

    /// Check if all expected delegates have produced output.
    pub fn is_complete(&self, expected_count: usize) -> bool {
        self.outputs.len() >= expected_count
    }

    /// Whether the sync mode guarantees first-round independence.
    pub fn guarantees_independence(&self) -> bool {
        self.sync_mode == SyncMode::CollectAll
    }
}

/// Strategy for reconciling divergent delegate outputs after a parallel round.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReconciliationStrategy {
    /// Facilitator synthesizes all outputs into a unified decision.
    FacilitatorSynthesis,
    /// Majority vote among delegates.
    MajorityVote,
    /// All delegates must agree (unanimous consent).
    UnanimousConsent,
    /// Use the output with the highest confidence score.
    HighestConfidence,
    /// Custom reconciliation logic (identified by name).
    Custom(String),
}

impl fmt::Display for ReconciliationStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReconciliationStrategy::FacilitatorSynthesis => write!(f, "facilitator_synthesis"),
            ReconciliationStrategy::MajorityVote => write!(f, "majority_vote"),
            ReconciliationStrategy::UnanimousConsent => write!(f, "unanimous_consent"),
            ReconciliationStrategy::HighestConfidence => write!(f, "highest_confidence"),
            ReconciliationStrategy::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pattern 8: Facilitator-Delegate tests --

    #[test]
    fn facilitator_injects_anti_roleplay_when_enabled() {
        let session = DeliberationSession::new(
            "sess-1".to_string(),
            FacilitatorRole {
                agent_name: "coordinator".to_string(),
                anti_roleplay_enabled: true,
            },
            vec![DelegateRole {
                agent_name: "security".to_string(),
                domain: "security".to_string(),
            }],
            3,
        );

        let injection = session.facilitator_prompt_injection();
        assert!(injection.is_some());
        assert!(injection.unwrap().contains("CRITICAL"));
    }

    #[test]
    fn facilitator_no_injection_when_disabled() {
        let session = DeliberationSession::new(
            "sess-2".to_string(),
            FacilitatorRole {
                agent_name: "coordinator".to_string(),
                anti_roleplay_enabled: false,
            },
            vec![],
            1,
        );

        assert!(session.facilitator_prompt_injection().is_none());
    }

    #[test]
    fn session_round_advancement() {
        let mut session = DeliberationSession::new(
            "sess-3".to_string(),
            FacilitatorRole {
                agent_name: "coord".to_string(),
                anti_roleplay_enabled: true,
            },
            vec![],
            3,
        );

        assert!(session.is_first_round());
        assert!(session.advance_round());
        assert_eq!(session.current_round, 2);
        assert!(!session.is_first_round());
        assert!(session.advance_round());
        assert_eq!(session.current_round, 3);
        assert!(!session.advance_round()); // max reached
        assert_eq!(session.current_round, 3);
    }

    // -- Pattern 9: Namespaced Decision ID tests --

    #[test]
    fn decision_id_format_and_parse_roundtrip() {
        let id = NamespacedDecisionId::new("ARCH".to_string(), 42);
        assert_eq!(id.format(), "ARCH-42");

        let parsed = NamespacedDecisionId::parse("ARCH-42").unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn decision_id_parse_rejects_invalid_format() {
        assert!(NamespacedDecisionId::parse("invalid").is_err());
        assert!(NamespacedDecisionId::parse("ARCH-abc").is_err());
    }

    #[test]
    fn namespace_validates_prefix_length() {
        assert!(DecisionNamespace::new("A".to_string(), "too short".to_string(), None).is_err());
        assert!(DecisionNamespace::new("ABCDE".to_string(), "too long".to_string(), None).is_err());
        assert!(DecisionNamespace::new("AB".to_string(), "ok".to_string(), None).is_ok());
        assert!(DecisionNamespace::new("ABCD".to_string(), "ok".to_string(), None).is_ok());
    }

    #[test]
    fn namespace_validates_uppercase() {
        assert!(DecisionNamespace::new("abc".to_string(), "lowercase".to_string(), None).is_err());
        assert!(DecisionNamespace::new("Ab".to_string(), "mixed".to_string(), None).is_err());
        assert!(DecisionNamespace::new("AB".to_string(), "ok".to_string(), None).is_ok());
    }

    #[test]
    fn registry_allocates_sequential_ids() {
        let mut registry = DecisionRegistry::new();
        let ns = DecisionNamespace::new("SEC".to_string(), "security decisions".to_string(), None)
            .unwrap();
        registry.register_namespace(ns).unwrap();

        let id1 = registry.next_id("SEC").unwrap();
        let id2 = registry.next_id("SEC").unwrap();
        let id3 = registry.next_id("SEC").unwrap();

        assert_eq!(id1.format(), "SEC-1");
        assert_eq!(id2.format(), "SEC-2");
        assert_eq!(id3.format(), "SEC-3");
    }

    #[test]
    fn registry_rejects_duplicate_namespace() {
        let mut registry = DecisionRegistry::new();
        let ns1 =
            DecisionNamespace::new("ARCH".to_string(), "architecture".to_string(), None).unwrap();
        let ns2 =
            DecisionNamespace::new("ARCH".to_string(), "duplicate".to_string(), None).unwrap();

        registry.register_namespace(ns1).unwrap();
        assert!(registry.register_namespace(ns2).is_err());
    }

    #[test]
    fn registry_isolates_namespaces() {
        let mut registry = DecisionRegistry::new();
        let ns_arch =
            DecisionNamespace::new("ARCH".to_string(), "architecture".to_string(), None).unwrap();
        let ns_sec =
            DecisionNamespace::new("SEC".to_string(), "security".to_string(), None).unwrap();
        registry.register_namespace(ns_arch).unwrap();
        registry.register_namespace(ns_sec).unwrap();

        let arch_1 = registry.next_id("ARCH").unwrap();
        let sec_1 = registry.next_id("SEC").unwrap();
        let arch_2 = registry.next_id("ARCH").unwrap();

        assert_eq!(arch_1.format(), "ARCH-1");
        assert_eq!(sec_1.format(), "SEC-1");
        assert_eq!(arch_2.format(), "ARCH-2");
    }

    // -- Pattern 10: Parallel round tests --

    #[test]
    fn parallel_round_collect_all_guarantees_independence() {
        let round = ParallelRound::new(1, SyncMode::CollectAll);
        assert!(round.guarantees_independence());

        let stream_round = ParallelRound::new(1, SyncMode::StreamAsComplete);
        assert!(!stream_round.guarantees_independence());
    }

    #[test]
    fn parallel_round_tracks_completion() {
        let mut round = ParallelRound::new(1, SyncMode::CollectAll);
        assert!(!round.is_complete(2));

        round.add_output(DelegateOutput {
            agent_name: "security".to_string(),
            domain: "security".to_string(),
            content: "no issues found".to_string(),
            succeeded: true,
            duration_ms: 1000,
        });
        assert!(!round.is_complete(2));

        round.add_output(DelegateOutput {
            agent_name: "performance".to_string(),
            domain: "performance".to_string(),
            content: "minor concerns".to_string(),
            succeeded: true,
            duration_ms: 1500,
        });
        assert!(round.is_complete(2));
    }

    #[test]
    fn reconciliation_strategy_display() {
        assert_eq!(
            ReconciliationStrategy::FacilitatorSynthesis.to_string(),
            "facilitator_synthesis"
        );
        assert_eq!(
            ReconciliationStrategy::MajorityVote.to_string(),
            "majority_vote"
        );
        assert_eq!(
            ReconciliationStrategy::Custom("weighted".to_string()).to_string(),
            "custom:weighted"
        );
    }
}
