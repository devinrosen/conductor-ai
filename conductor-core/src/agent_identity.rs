//! Agent identity types: persona, model tier, role hierarchy, namespace separation.
//!
//! Typed representations for the fields in migration v050's `agent_templates` table.
//!
//! Part of: persona-based-agent-specialization@1.1.0,
//! model-tier-selection@1.0.0, role-based-agent-hierarchy@1.0.0,
//! two-layer-agent-namespace-separation@1.0.0, generic-fsm-skeleton@1.1.0

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::Result;

// ─── Persona Configuration ──────────────────────────────────────────────────
// Part of: persona-based-agent-specialization@1.1.0

/// Depth of persona grounding for an agent template.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaDepth {
    /// Minimal persona — name and role only.
    #[default]
    Minimal,
    /// Standard persona — includes credentials and domain grounding.
    Standard,
    /// Deep persona — full philosophy and behavioral constraints.
    Deep,
}

impl fmt::Display for PersonaDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Minimal => write!(f, "minimal"),
            Self::Standard => write!(f, "standard"),
            Self::Deep => write!(f, "deep"),
        }
    }
}

impl FromStr for PersonaDepth {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "minimal" => Ok(Self::Minimal),
            "standard" => Ok(Self::Standard),
            "deep" => Ok(Self::Deep),
            _ => Err(format!("unknown PersonaDepth: {s}")),
        }
    }
}

crate::impl_sql_enum!(PersonaDepth);

/// Persona configuration for an agent template.
/// Maps to `persona_name`, `persona_depth`, `persona_credentials`,
/// `domain_grounding`, and `philosophy` columns in `agent_templates`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonaConfig {
    /// Human-readable persona name (e.g. "Senior Rust Reviewer").
    pub name: Option<String>,
    /// How deeply the persona is grounded.
    pub depth: PersonaDepth,
    /// Credentials or qualifications the persona claims (for prompt grounding).
    pub credentials: Option<String>,
    /// Domain knowledge the persona is grounded in (e.g. "Rust async, SQLite").
    pub domain_grounding: Option<String>,
    /// Behavioral philosophy / constraints (e.g. "Prefer explicit over implicit").
    pub philosophy: Option<String>,
}

impl PersonaConfig {
    /// Build a system prompt preamble from this persona config.
    #[allow(dead_code)]
    pub fn to_prompt_preamble(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref name) = self.name {
            parts.push(format!("You are {name}."));
        }
        if let Some(ref creds) = self.credentials {
            parts.push(format!("Credentials: {creds}"));
        }
        if let Some(ref domain) = self.domain_grounding {
            parts.push(format!("Domain expertise: {domain}"));
        }
        if let Some(ref phil) = self.philosophy {
            parts.push(format!("Philosophy: {phil}"));
        }
        parts.join(" ")
    }
}

// ─── Model Tier ─────────────────────────────────────────────────────────────
// Part of: model-tier-selection@1.0.0

/// Model capability tier for task-based selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Highest capability (e.g. claude-opus) — complex reasoning, planning.
    High,
    /// Standard capability (e.g. claude-sonnet) — general tasks.
    #[default]
    Standard,
    /// Efficient / fast (e.g. claude-haiku) — simple, high-throughput tasks.
    Efficient,
}

impl fmt::Display for ModelTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Standard => write!(f, "standard"),
            Self::Efficient => write!(f, "efficient"),
        }
    }
}

impl FromStr for ModelTier {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "high" => Ok(Self::High),
            "standard" => Ok(Self::Standard),
            "efficient" => Ok(Self::Efficient),
            _ => Err(format!("unknown ModelTier: {s}")),
        }
    }
}

crate::impl_sql_enum!(ModelTier);

/// Complexity signal used for model tier selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum TaskComplexity {
    /// Simple, well-scoped task (formatting, linting, simple code gen).
    Low,
    /// Moderate complexity (feature implementation, refactoring).
    Medium,
    /// High complexity (architecture, multi-file reasoning, planning).
    High,
}

impl TaskComplexity {
    /// Select the appropriate model tier for this complexity level.
    #[allow(dead_code)]
    pub fn recommended_tier(&self) -> ModelTier {
        match self {
            Self::Low => ModelTier::Efficient,
            Self::Medium => ModelTier::Standard,
            Self::High => ModelTier::High,
        }
    }
}

/// Model selection result with tier and optional explicit model override.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ModelSelection {
    pub tier: ModelTier,
    /// Explicit model name override (e.g. "claude-sonnet-4-6"). Takes precedence over tier.
    pub model_override: Option<String>,
}

impl ModelSelection {
    /// Resolve the final model name. If an override is set, use it; otherwise map tier to default.
    #[allow(dead_code)]
    pub fn resolve(&self) -> &str {
        if let Some(ref m) = self.model_override {
            return m.as_str();
        }
        match self.tier {
            ModelTier::High => "claude-opus-4-6",
            ModelTier::Standard => "claude-sonnet-4-6",
            ModelTier::Efficient => "claude-haiku-3-5",
        }
    }
}

// ─── Agent Role & Hierarchy ─────────────────────────────────────────────────
// Part of: role-based-agent-hierarchy@1.0.0

/// Hierarchical role for an agent in a multi-agent system.
/// Maps to the `role` column in `agent_templates`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Tier 0: Executes well-scoped tasks (code gen, formatting).
    Execution,
    /// Tier 1: Domain specialist (reviewer, security auditor, etc.).
    Specialist,
    /// Tier 2: Plans multi-step work, decomposes features.
    Planning,
    /// Tier 3: Supervises other agents, handles escalations.
    Supervisor,
    /// Legacy / generic role (maps to "reviewer" in DB default).
    #[default]
    Reviewer,
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execution => write!(f, "execution"),
            Self::Specialist => write!(f, "specialist"),
            Self::Planning => write!(f, "planning"),
            Self::Supervisor => write!(f, "supervisor"),
            Self::Reviewer => write!(f, "reviewer"),
        }
    }
}

impl FromStr for AgentRole {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "execution" => Ok(Self::Execution),
            "specialist" => Ok(Self::Specialist),
            "planning" => Ok(Self::Planning),
            "supervisor" => Ok(Self::Supervisor),
            "reviewer" => Ok(Self::Reviewer),
            _ => Err(format!("unknown AgentRole: {s}")),
        }
    }
}

crate::impl_sql_enum!(AgentRole);

/// Hierarchy tier (0-3) corresponding to AgentRole.
/// Maps to `tier` column in `agent_templates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AgentHierarchy {
    pub role: AgentRole,
    pub tier: u8,
    /// Capabilities this agent is allowed to exercise (JSON array in DB).
    pub capabilities: Vec<String>,
    /// Roles this agent can delegate to (JSON array in DB).
    pub delegation_table: Vec<String>,
}

impl AgentHierarchy {
    /// Create a hierarchy entry from a role, auto-deriving the tier.
    #[allow(dead_code)]
    pub fn from_role(role: AgentRole) -> Self {
        let tier = match &role {
            AgentRole::Execution => 0,
            AgentRole::Specialist => 1,
            AgentRole::Planning => 2,
            AgentRole::Supervisor => 3,
            AgentRole::Reviewer => 1,
        };
        Self {
            role,
            tier,
            capabilities: Vec::new(),
            delegation_table: Vec::new(),
        }
    }

    /// Whether this agent can delegate to agents of the given role.
    #[allow(dead_code)]
    pub fn can_delegate_to(&self, target: &AgentRole) -> bool {
        let target_tier = match target {
            AgentRole::Execution => 0,
            AgentRole::Specialist => 1,
            AgentRole::Planning => 2,
            AgentRole::Supervisor => 3,
            AgentRole::Reviewer => 1,
        };
        // Can only delegate downward in the hierarchy
        self.tier > target_tier
    }
}

// ─── Agent Namespace ────────────────────────────────────────────────────────
// Part of: two-layer-agent-namespace-separation@1.0.0

/// Namespace layer for agent templates.
/// System agents are built-in and immutable; user agents are custom.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentNamespace {
    /// Built-in system agents (immutable, shipped with conductor).
    System,
    /// User-defined custom agents.
    #[default]
    User,
}

impl fmt::Display for AgentNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => write!(f, "system"),
            Self::User => write!(f, "user"),
        }
    }
}

impl FromStr for AgentNamespace {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            _ => Err(format!("unknown AgentNamespace: {s}")),
        }
    }
}

crate::impl_sql_enum!(AgentNamespace);

/// Fully qualified agent name with namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct QualifiedAgentName {
    pub namespace: AgentNamespace,
    pub name: String,
}

impl QualifiedAgentName {
    #[allow(dead_code)]
    pub fn new(namespace: AgentNamespace, name: impl Into<String>) -> Self {
        Self {
            namespace,
            name: name.into(),
        }
    }

    /// Format as "namespace/name" (e.g. "system/reviewer").
    #[allow(dead_code)]
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    /// Parse "namespace/name" into a QualifiedAgentName.
    #[allow(dead_code)]
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        let parts: Vec<&str> = s.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(format!("expected 'namespace/name', got '{s}'"));
        }
        let namespace = AgentNamespace::from_str(parts[0])?;
        Ok(Self {
            namespace,
            name: parts[1].to_string(),
        })
    }
}

impl fmt::Display for QualifiedAgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

// ─── Agent Run FSM ──────────────────────────────────────────────────────────
// Part of: generic-fsm-skeleton@1.1.0
//
// Applies Wave 1's FSM transition guard pattern to the agent run lifecycle.
// The states correspond to `AgentRunStatus` variants; transitions are guarded.

/// Validated transition in the agent run FSM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AgentFsmTransition {
    pub from: String,
    pub to: String,
    pub trigger: String,
}

/// Check whether a state transition is valid in the agent run FSM.
///
/// Valid transitions:
///   running -> completed | failed | cancelled | waiting_for_feedback
///   waiting_for_feedback -> running | cancelled
///   (terminal states: completed, failed, cancelled — no outgoing transitions)
#[allow(dead_code)]
pub fn is_valid_agent_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("running", "completed")
            | ("running", "failed")
            | ("running", "cancelled")
            | ("running", "waiting_for_feedback")
            | ("waiting_for_feedback", "running")
            | ("waiting_for_feedback", "cancelled")
    )
}

/// Attempt a guarded FSM transition. Returns `Ok(transition)` if valid,
/// or an error describing the invalid transition.
#[allow(dead_code)]
pub fn try_agent_transition(
    from: &str,
    to: &str,
    trigger: &str,
) -> std::result::Result<AgentFsmTransition, String> {
    if is_valid_agent_transition(from, to) {
        Ok(AgentFsmTransition {
            from: from.to_string(),
            to: to.to_string(),
            trigger: trigger.to_string(),
        })
    } else {
        Err(format!(
            "invalid agent FSM transition: {from} -> {to} (trigger: {trigger})"
        ))
    }
}

// ─── Agent Template (DB CRUD) ───────────────────────────────────────────────

/// Full agent template record, mirroring the `agent_templates` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AgentTemplate {
    pub id: String,
    pub name: String,
    pub persona: PersonaConfig,
    pub role: AgentRole,
    pub tier: u8,
    pub namespace: AgentNamespace,
    pub model_tier: Option<ModelTier>,
    pub model_override: Option<String>,
    pub capabilities: Vec<String>,
    pub delegation_table: Vec<String>,
    pub output_contract: Option<String>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Manager for agent template CRUD operations, following conductor's Manager pattern.
pub struct AgentTemplateManager<'a> {
    conn: &'a Connection,
}

impl<'a> AgentTemplateManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a new agent template.
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn create_agent_template(
        &self,
        name: &str,
        persona: &PersonaConfig,
        role: &AgentRole,
        namespace: &AgentNamespace,
        model_tier: Option<&ModelTier>,
        model_override: Option<&str>,
        capabilities: &[String],
        delegation_table: &[String],
        output_contract: Option<&str>,
    ) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        let tier: u8 = match role {
            AgentRole::Execution => 0,
            AgentRole::Specialist | AgentRole::Reviewer => 1,
            AgentRole::Planning => 2,
            AgentRole::Supervisor => 3,
        };
        let caps_json = serde_json::to_string(capabilities).unwrap_or_else(|_| "[]".to_string());
        let deleg_json =
            serde_json::to_string(delegation_table).unwrap_or_else(|_| "[]".to_string());
        let mt_str = model_tier.map(|t| t.to_string());

        self.conn.execute(
            "INSERT INTO agent_templates (id, name, persona_name, persona_depth, persona_credentials, domain_grounding, philosophy, role, tier, namespace, model_tier, model_override, capabilities, delegation_table, output_contract)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                id,
                name,
                persona.name,
                persona.depth.to_string(),
                persona.credentials,
                persona.domain_grounding,
                persona.philosophy,
                role.to_string(),
                tier,
                namespace.to_string(),
                mt_str,
                model_override,
                caps_json,
                deleg_json,
                output_contract,
            ],
        )?;

        Ok(id)
    }

    /// Get an agent template by name.
    #[allow(dead_code)]
    pub fn get_agent_template(&self, name: &str) -> Result<Option<AgentTemplate>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, persona_name, persona_depth, persona_credentials, domain_grounding, philosophy, role, tier, namespace, model_tier, model_override, capabilities, delegation_table, output_contract, version, created_at, updated_at
             FROM agent_templates WHERE name = ?1",
        )?;

        let mut rows = stmt.query_map(params![name], |row| {
            let depth_str: String = row.get(3)?;
            let role_str: String = row.get(7)?;
            let ns_str: String = row.get(9)?;
            let mt_str: Option<String> = row.get(10)?;
            let caps_str: String = row.get(12)?;
            let deleg_str: String = row.get(13)?;

            Ok(AgentTemplate {
                id: row.get(0)?,
                name: row.get(1)?,
                persona: PersonaConfig {
                    name: row.get(2)?,
                    depth: depth_str.parse().unwrap_or_default(),
                    credentials: row.get(4)?,
                    domain_grounding: row.get(5)?,
                    philosophy: row.get(6)?,
                },
                role: role_str.parse().unwrap_or_default(),
                tier: row.get(8)?,
                namespace: ns_str.parse().unwrap_or_default(),
                model_tier: mt_str.and_then(|s| s.parse().ok()),
                model_override: row.get(11)?,
                capabilities: serde_json::from_str(&caps_str).unwrap_or_default(),
                delegation_table: serde_json::from_str(&deleg_str).unwrap_or_default(),
                output_contract: row.get(14)?,
                version: row.get(15)?,
                created_at: row.get(16)?,
                updated_at: row.get(17)?,
            })
        })?;

        match rows.next() {
            Some(Ok(t)) => Ok(Some(t)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List agent templates filtered by namespace.
    #[allow(dead_code)]
    pub fn list_templates_by_namespace(
        &self,
        namespace: &AgentNamespace,
    ) -> Result<Vec<AgentTemplate>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, persona_name, persona_depth, persona_credentials, domain_grounding, philosophy, role, tier, namespace, model_tier, model_override, capabilities, delegation_table, output_contract, version, created_at, updated_at
             FROM agent_templates WHERE namespace = ?1 ORDER BY name",
        )?;

        let rows = stmt
            .query_map(params![namespace.to_string()], |row| {
                let depth_str: String = row.get(3)?;
                let role_str: String = row.get(7)?;
                let ns_str: String = row.get(9)?;
                let mt_str: Option<String> = row.get(10)?;
                let caps_str: String = row.get(12)?;
                let deleg_str: String = row.get(13)?;

                Ok(AgentTemplate {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    persona: PersonaConfig {
                        name: row.get(2)?,
                        depth: depth_str.parse().unwrap_or_default(),
                        credentials: row.get(4)?,
                        domain_grounding: row.get(5)?,
                        philosophy: row.get(6)?,
                    },
                    role: role_str.parse().unwrap_or_default(),
                    tier: row.get(8)?,
                    namespace: ns_str.parse().unwrap_or_default(),
                    model_tier: mt_str.and_then(|s| s.parse().ok()),
                    model_override: row.get(11)?,
                    capabilities: serde_json::from_str(&caps_str).unwrap_or_default(),
                    delegation_table: serde_json::from_str(&deleg_str).unwrap_or_default(),
                    output_contract: row.get(14)?,
                    version: row.get(15)?,
                    created_at: row.get(16)?,
                    updated_at: row.get(17)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    // ─── PersonaConfig tests ────────────────────────────────────────────

    #[test]
    fn persona_prompt_preamble_full() {
        let persona = PersonaConfig {
            name: Some("Senior Rust Reviewer".to_string()),
            depth: PersonaDepth::Deep,
            credentials: Some("10 years Rust experience".to_string()),
            domain_grounding: Some("async Rust, SQLite, CLI tooling".to_string()),
            philosophy: Some("Prefer explicit over implicit".to_string()),
        };
        let preamble = persona.to_prompt_preamble();
        assert!(preamble.contains("Senior Rust Reviewer"));
        assert!(preamble.contains("10 years"));
        assert!(preamble.contains("async Rust"));
        assert!(preamble.contains("explicit over implicit"));
    }

    #[test]
    fn persona_prompt_preamble_minimal() {
        let persona = PersonaConfig::default();
        let preamble = persona.to_prompt_preamble();
        assert!(preamble.is_empty());
    }

    #[test]
    fn persona_depth_roundtrip() {
        for depth in &[
            PersonaDepth::Minimal,
            PersonaDepth::Standard,
            PersonaDepth::Deep,
        ] {
            let s = depth.to_string();
            let parsed: PersonaDepth = s.parse().unwrap();
            assert_eq!(&parsed, depth);
        }
    }

    // ─── ModelTier tests ────────────────────────────────────────────────

    #[test]
    fn model_tier_roundtrip() {
        for tier in &[ModelTier::High, ModelTier::Standard, ModelTier::Efficient] {
            let s = tier.to_string();
            let parsed: ModelTier = s.parse().unwrap();
            assert_eq!(&parsed, tier);
        }
    }

    #[test]
    fn task_complexity_recommends_correct_tier() {
        assert_eq!(TaskComplexity::Low.recommended_tier(), ModelTier::Efficient);
        assert_eq!(
            TaskComplexity::Medium.recommended_tier(),
            ModelTier::Standard
        );
        assert_eq!(TaskComplexity::High.recommended_tier(), ModelTier::High);
    }

    #[test]
    fn model_selection_resolve_override() {
        let sel = ModelSelection {
            tier: ModelTier::Efficient,
            model_override: Some("claude-sonnet-4-6".to_string()),
        };
        assert_eq!(sel.resolve(), "claude-sonnet-4-6");
    }

    #[test]
    fn model_selection_resolve_by_tier() {
        let sel = ModelSelection {
            tier: ModelTier::High,
            model_override: None,
        };
        assert_eq!(sel.resolve(), "claude-opus-4-6");
    }

    // ─── AgentRole & Hierarchy tests ────────────────────────────────────

    #[test]
    fn agent_role_roundtrip() {
        for role in &[
            AgentRole::Execution,
            AgentRole::Specialist,
            AgentRole::Planning,
            AgentRole::Supervisor,
            AgentRole::Reviewer,
        ] {
            let s = role.to_string();
            let parsed: AgentRole = s.parse().unwrap();
            assert_eq!(&parsed, role);
        }
    }

    #[test]
    fn hierarchy_tier_derivation() {
        assert_eq!(AgentHierarchy::from_role(AgentRole::Execution).tier, 0);
        assert_eq!(AgentHierarchy::from_role(AgentRole::Specialist).tier, 1);
        assert_eq!(AgentHierarchy::from_role(AgentRole::Planning).tier, 2);
        assert_eq!(AgentHierarchy::from_role(AgentRole::Supervisor).tier, 3);
    }

    #[test]
    fn hierarchy_delegation_rules() {
        let supervisor = AgentHierarchy::from_role(AgentRole::Supervisor);
        assert!(supervisor.can_delegate_to(&AgentRole::Execution));
        assert!(supervisor.can_delegate_to(&AgentRole::Specialist));
        assert!(supervisor.can_delegate_to(&AgentRole::Planning));
        assert!(!supervisor.can_delegate_to(&AgentRole::Supervisor));

        let execution = AgentHierarchy::from_role(AgentRole::Execution);
        assert!(!execution.can_delegate_to(&AgentRole::Execution));
        assert!(!execution.can_delegate_to(&AgentRole::Specialist));
    }

    // ─── Namespace tests ────────────────────────────────────────────────

    #[test]
    fn namespace_roundtrip() {
        for ns in &[AgentNamespace::System, AgentNamespace::User] {
            let s = ns.to_string();
            let parsed: AgentNamespace = s.parse().unwrap();
            assert_eq!(&parsed, ns);
        }
    }

    #[test]
    fn qualified_name_format_and_parse() {
        let qn = QualifiedAgentName::new(AgentNamespace::System, "reviewer");
        assert_eq!(qn.qualified(), "system/reviewer");
        assert_eq!(qn.to_string(), "system/reviewer");

        let parsed = QualifiedAgentName::parse("user/my-agent").unwrap();
        assert_eq!(parsed.namespace, AgentNamespace::User);
        assert_eq!(parsed.name, "my-agent");
    }

    #[test]
    fn qualified_name_parse_error() {
        assert!(QualifiedAgentName::parse("no-slash").is_err());
        assert!(QualifiedAgentName::parse("invalid/agent").is_err());
    }

    // ─── FSM tests ──────────────────────────────────────────────────────

    #[test]
    fn fsm_valid_transitions() {
        assert!(is_valid_agent_transition("running", "completed"));
        assert!(is_valid_agent_transition("running", "failed"));
        assert!(is_valid_agent_transition("running", "cancelled"));
        assert!(is_valid_agent_transition("running", "waiting_for_feedback"));
        assert!(is_valid_agent_transition("waiting_for_feedback", "running"));
        assert!(is_valid_agent_transition(
            "waiting_for_feedback",
            "cancelled"
        ));
    }

    #[test]
    fn fsm_invalid_transitions() {
        // Terminal states cannot transition
        assert!(!is_valid_agent_transition("completed", "running"));
        assert!(!is_valid_agent_transition("failed", "running"));
        assert!(!is_valid_agent_transition("cancelled", "running"));
        // Cannot skip to completed from waiting
        assert!(!is_valid_agent_transition(
            "waiting_for_feedback",
            "completed"
        ));
    }

    #[test]
    fn fsm_try_transition_ok() {
        let t = try_agent_transition("running", "completed", "agent_finished").unwrap();
        assert_eq!(t.from, "running");
        assert_eq!(t.to, "completed");
        assert_eq!(t.trigger, "agent_finished");
    }

    #[test]
    fn fsm_try_transition_err() {
        let err = try_agent_transition("completed", "running", "restart").unwrap_err();
        assert!(err.contains("invalid agent FSM transition"));
    }

    // ─── Agent Template DB CRUD tests ───────────────────────────────────

    #[test]
    fn template_create_and_get() {
        let conn = setup_db();
        let mgr = AgentTemplateManager::new(&conn);
        let persona = PersonaConfig {
            name: Some("Test Agent".to_string()),
            depth: PersonaDepth::Standard,
            credentials: Some("Expert".to_string()),
            domain_grounding: Some("Rust".to_string()),
            philosophy: None,
        };

        let id = mgr
            .create_agent_template(
                "test-agent",
                &persona,
                &AgentRole::Specialist,
                &AgentNamespace::User,
                Some(&ModelTier::Standard),
                None,
                &["code_review".to_string()],
                &["execution".to_string()],
                None,
            )
            .unwrap();
        assert!(!id.is_empty());

        let tmpl = mgr.get_agent_template("test-agent").unwrap().unwrap();
        assert_eq!(tmpl.name, "test-agent");
        assert_eq!(tmpl.persona.name, Some("Test Agent".to_string()));
        assert_eq!(tmpl.persona.depth, PersonaDepth::Standard);
        assert_eq!(tmpl.role, AgentRole::Specialist);
        assert_eq!(tmpl.tier, 1);
        assert_eq!(tmpl.namespace, AgentNamespace::User);
        assert_eq!(tmpl.model_tier, Some(ModelTier::Standard));
        assert_eq!(tmpl.capabilities, vec!["code_review".to_string()]);
    }

    #[test]
    fn template_list_by_namespace() {
        let conn = setup_db();
        let mgr = AgentTemplateManager::new(&conn);
        let persona = PersonaConfig::default();

        mgr.create_agent_template(
            "sys-agent",
            &persona,
            &AgentRole::Reviewer,
            &AgentNamespace::System,
            None,
            None,
            &[],
            &[],
            None,
        )
        .unwrap();
        mgr.create_agent_template(
            "usr-agent",
            &persona,
            &AgentRole::Execution,
            &AgentNamespace::User,
            None,
            None,
            &[],
            &[],
            None,
        )
        .unwrap();

        let system = mgr
            .list_templates_by_namespace(&AgentNamespace::System)
            .unwrap();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0].name, "sys-agent");

        let user = mgr
            .list_templates_by_namespace(&AgentNamespace::User)
            .unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].name, "usr-agent");
    }

    #[test]
    fn template_get_nonexistent() {
        let conn = setup_db();
        let mgr = AgentTemplateManager::new(&conn);
        let result = mgr.get_agent_template("does-not-exist").unwrap();
        assert!(result.is_none());
    }
}
