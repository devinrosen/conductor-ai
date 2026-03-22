//! Catalog of operational structure triage decisions for Wave 5.
//!
//! Records the triage outcome (Adapt / Reference / Defer) for each of the 111
//! operational structures from the global-sdlc integration.
//!
//! Summary counts:
//! - Agents:    32 total (12 adapt, 10 reference, 10 defer)
//! - Commands:  19 total (8 adapt, 7 reference, 4 defer)
//! - Schemas:   22 total (6 adapt, 12 reference, 4 defer)
//! - Decisions: 27 total (8 adapt, 15 reference, 4 defer)
//! - Skills:     9 total (3 adapt, 4 reference, 2 defer)
//! - Protocols:  2 total (1 adapt, 1 reference, 0 defer)
//! - TOTAL:    111 total (38 adapt, 49 reference, 24 defer)

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Triage decision for an operational structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriageDecision {
    /// Actively adapt this OS for conductor.
    Adapt,
    /// Keep as reference material; do not integrate into conductor code.
    Reference,
    /// Defer — not applicable or out of scope.
    Defer,
}

impl std::fmt::Display for TriageDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Adapt => write!(f, "adapt"),
            Self::Reference => write!(f, "reference"),
            Self::Defer => write!(f, "defer"),
        }
    }
}

/// Category of operational structure.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OsCategory {
    Agent,
    Command,
    Schema,
    Decision,
    Skill,
    Protocol,
}

impl std::fmt::Display for OsCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent => write!(f, "agent"),
            Self::Command => write!(f, "command"),
            Self::Schema => write!(f, "schema"),
            Self::Decision => write!(f, "decision"),
            Self::Skill => write!(f, "skill"),
            Self::Protocol => write!(f, "protocol"),
        }
    }
}

/// A single entry in the operational structure catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsCatalogEntry {
    /// Unique identifier for this OS (e.g. "agent/claude-code-guide").
    pub os_id: String,
    /// Category (agent, command, schema, etc.).
    pub category: OsCategory,
    /// Human-readable name.
    pub name: String,
    /// Triage decision.
    pub triage_decision: TriageDecision,
    /// Notes on why this decision was made and any adaptation guidance.
    pub adaptation_notes: String,
    /// Target path in conductor's `.conductor/` directory, if adapted.
    pub target_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// The complete catalog of operational structure triage decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsCatalog {
    pub entries: Vec<OsCatalogEntry>,
}

impl OsCatalog {
    /// Load the hardcoded catalog of 111 operational structures.
    pub fn load() -> Self {
        Self {
            entries: build_catalog(),
        }
    }

    /// Filter entries by category.
    pub fn entries_by_category(&self, category: &OsCategory) -> Vec<&OsCatalogEntry> {
        self.entries
            .iter()
            .filter(|e| &e.category == category)
            .collect()
    }

    /// Filter entries by triage decision.
    pub fn entries_by_decision(&self, decision: &TriageDecision) -> Vec<&OsCatalogEntry> {
        self.entries
            .iter()
            .filter(|e| &e.triage_decision == decision)
            .collect()
    }

    /// Return summary counts: (adapt, reference, defer) for a given category.
    pub fn counts_by_category(&self, category: &OsCategory) -> (usize, usize, usize) {
        let items = self.entries_by_category(category);
        let adapt = items
            .iter()
            .filter(|e| e.triage_decision == TriageDecision::Adapt)
            .count();
        let reference = items
            .iter()
            .filter(|e| e.triage_decision == TriageDecision::Reference)
            .count();
        let defer = items
            .iter()
            .filter(|e| e.triage_decision == TriageDecision::Defer)
            .count();
        (adapt, reference, defer)
    }

    /// Return overall summary counts: (total, adapt, reference, defer).
    pub fn total_counts(&self) -> (usize, usize, usize, usize) {
        let total = self.entries.len();
        let adapt = self
            .entries
            .iter()
            .filter(|e| e.triage_decision == TriageDecision::Adapt)
            .count();
        let reference = self
            .entries
            .iter()
            .filter(|e| e.triage_decision == TriageDecision::Reference)
            .count();
        let defer = self
            .entries
            .iter()
            .filter(|e| e.triage_decision == TriageDecision::Defer)
            .count();
        (total, adapt, reference, defer)
    }
}

// ---------------------------------------------------------------------------
// Catalog builder (hardcoded entries)
// ---------------------------------------------------------------------------

fn entry(
    os_id: &str,
    category: OsCategory,
    name: &str,
    decision: TriageDecision,
    notes: &str,
    target: Option<&str>,
) -> OsCatalogEntry {
    OsCatalogEntry {
        os_id: os_id.to_string(),
        category,
        name: name.to_string(),
        triage_decision: decision,
        adaptation_notes: notes.to_string(),
        target_path: target.map(|s| s.to_string()),
    }
}

#[allow(clippy::too_many_lines)]
fn build_catalog() -> Vec<OsCatalogEntry> {
    use OsCategory::*;
    use TriageDecision::*;

    vec![
        // ===== Agents (32): 12 adapt, 10 reference, 10 defer =====
        entry(
            "agent/claude-code-guide",
            Agent,
            "Claude Code Guide",
            Adapt,
            "Claude Code workflow design for conductor",
            Some(".conductor/agents/claude-code-guide"),
        ),
        entry(
            "agent/prompt-analyst",
            Agent,
            "Prompt Analyst",
            Adapt,
            "Prompt quality analysis for conductor agents",
            Some(".conductor/agents/prompt-analyst"),
        ),
        entry(
            "agent/prompt-engineer",
            Agent,
            "Prompt Engineer",
            Adapt,
            "Prompt construction for conductor agents",
            Some(".conductor/agents/prompt-engineer"),
        ),
        entry(
            "agent/tsdlc-autonomous-debugger",
            Agent,
            "Autonomous Debugger",
            Adapt,
            "Autonomous debugging methodology for Rust/conductor",
            Some(".conductor/agents/autonomous-debugger"),
        ),
        entry(
            "agent/tsdlc-debugger",
            Agent,
            "Debugger",
            Adapt,
            "General debugging methodology",
            Some(".conductor/agents/debugger"),
        ),
        entry(
            "agent/tsdlc-doc-librarian",
            Agent,
            "Doc Librarian",
            Adapt,
            "Doc reorganization for any codebase",
            Some(".conductor/agents/doc-librarian"),
        ),
        entry(
            "agent/tsdlc-doc-writer",
            Agent,
            "Doc Writer",
            Adapt,
            "Doc creation for any codebase",
            Some(".conductor/agents/doc-writer"),
        ),
        entry(
            "agent/tsdlc-engineering-lead",
            Agent,
            "Engineering Lead",
            Adapt,
            "Process improvement, DX, tech debt governance",
            Some(".conductor/agents/engineering-lead"),
        ),
        entry(
            "agent/tsdlc-planner",
            Agent,
            "Planner",
            Adapt,
            "Milestone planning methodology",
            Some(".conductor/agents/planner"),
        ),
        entry(
            "agent/tsdlc-preplanner",
            Agent,
            "Preplanner",
            Adapt,
            "Research readiness assessment",
            Some(".conductor/agents/preplanner"),
        ),
        entry(
            "agent/tsdlc-verification-engineer",
            Agent,
            "Verification Engineer",
            Adapt,
            "Evidence-based verification",
            Some(".conductor/agents/verification-engineer"),
        ),
        entry(
            "agent/tsdlc-handoff-compliance",
            Agent,
            "Handoff Compliance",
            Adapt,
            "Milestone handoff process",
            Some(".conductor/agents/handoff-compliance"),
        ),
        entry(
            "agent/tsdlc-command-remediator",
            Agent,
            "Command Remediator",
            Reference,
            "Slash command remediation patterns",
            None,
        ),
        entry(
            "agent/tsdlc-milestone-aligner",
            Agent,
            "Milestone Aligner",
            Reference,
            "Alignment scoring methodology",
            None,
        ),
        entry(
            "agent/tsdlc-platform-architect",
            Agent,
            "Platform Architect",
            Reference,
            "Architecture patterns (Go-specific internals)",
            None,
        ),
        entry(
            "agent/tsdlc-product-manager",
            Agent,
            "Product Manager",
            Reference,
            "Product strategy methodology",
            None,
        ),
        entry(
            "agent/tsdlc-program-manager",
            Agent,
            "Program Manager",
            Reference,
            "Program coordination patterns",
            None,
        ),
        entry(
            "agent/tsdlc-progress-analyst",
            Agent,
            "Progress Analyst",
            Reference,
            "Progress analysis methodology",
            None,
        ),
        entry(
            "agent/tsdlc-qa-verification-engineer",
            Agent,
            "QA Verification Engineer",
            Reference,
            "QA methodology",
            None,
        ),
        entry(
            "agent/tsdlc-research-readiness-assessor",
            Agent,
            "Research Readiness Assessor",
            Reference,
            "Research assessment rubric",
            None,
        ),
        entry(
            "agent/tsdlc-sdlc-guide",
            Agent,
            "SDLC Guide",
            Reference,
            "SDLC guidance methodology",
            None,
        ),
        entry(
            "agent/tsdlc-skill-tooling-developer",
            Agent,
            "Skill Tooling Developer",
            Reference,
            "Skill/tool development patterns",
            None,
        ),
        entry(
            "agent/tsdlc-go-cli-architect",
            Agent,
            "Go CLI Architect",
            Defer,
            "Go-specific: irrelevant to Rust/conductor",
            None,
        ),
        entry(
            "agent/tsdlc-go-services-architect",
            Agent,
            "Go Services Architect",
            Defer,
            "Go-specific: irrelevant to Rust/conductor",
            None,
        ),
        entry(
            "agent/tsdlc-typescript-services-architect",
            Agent,
            "TypeScript Services Architect",
            Defer,
            "TypeScript-specific: irrelevant to Rust/conductor",
            None,
        ),
        entry(
            "agent/tsdlc-web-frontend-architect",
            Agent,
            "Web Frontend Architect",
            Defer,
            "Frontend-specific: conductor-web is minimal",
            None,
        ),
        entry(
            "agent/tsdlc-playwright-executor",
            Agent,
            "Playwright Executor",
            Defer,
            "Playwright-specific: conductor uses cargo test",
            None,
        ),
        entry(
            "agent/tsdlc-ux-specialist",
            Agent,
            "UX Specialist",
            Defer,
            "UX-specific: conductor TUI has different patterns",
            None,
        ),
        entry(
            "agent/tsdlc-visual-designer",
            Agent,
            "Visual Designer",
            Defer,
            "Visual design: global-sdlc specific",
            None,
        ),
        entry(
            "agent/lively-people-ops-manager",
            Agent,
            "People Ops Manager",
            Defer,
            "Company-specific: Lively HR agent",
            None,
        ),
        entry(
            "agent/product-intel-analyst",
            Agent,
            "Product Intel Analyst",
            Defer,
            "Company-specific: competitive intelligence",
            None,
        ),
        entry(
            "agent/sidebar-command-executor",
            Agent,
            "Sidebar Command Executor",
            Defer,
            "Platform-specific: sidebar UI executor",
            None,
        ),
        // ===== Commands (19): 8 adapt, 7 reference, 4 defer =====
        entry(
            "command/verify",
            Command,
            "Verify",
            Adapt,
            "Evidence-based verification for conductor workflows",
            Some(".conductor/commands/verify"),
        ),
        entry(
            "command/project-status",
            Command,
            "Project Status",
            Adapt,
            "Progress analysis maps to conductor workflow/agent run status",
            Some(".conductor/commands/project-status"),
        ),
        entry(
            "command/preplan",
            Command,
            "Preplan",
            Adapt,
            "Research readiness assessment before implementation",
            Some(".conductor/commands/preplan"),
        ),
        entry(
            "command/plan-milestone",
            Command,
            "Plan Milestone",
            Adapt,
            "Milestone planning maps to conductor feature planning",
            Some(".conductor/commands/plan-milestone"),
        ),
        entry(
            "command/pr",
            Command,
            "PR",
            Adapt,
            "PR workflow already exists in conductor (iterate-pr.wf)",
            Some(".conductor/commands/pr"),
        ),
        entry(
            "command/align-milestone",
            Command,
            "Align Milestone",
            Adapt,
            "Alignment scoring uses lifecycle-gating patterns",
            Some(".conductor/commands/align-milestone"),
        ),
        entry(
            "command/handoff-milestone",
            Command,
            "Handoff Milestone",
            Adapt,
            "Milestone handoff process",
            Some(".conductor/commands/handoff-milestone"),
        ),
        entry(
            "command/ship-milestone",
            Command,
            "Ship Milestone",
            Adapt,
            "Milestone completion workflow",
            Some(".conductor/commands/ship-milestone"),
        ),
        entry(
            "command/agent-generator",
            Command,
            "Agent Generator",
            Reference,
            "Meta-agent generation methodology",
            None,
        ),
        entry(
            "command/explore-backlog",
            Command,
            "Explore Backlog",
            Reference,
            "Backlog exploration methodology",
            None,
        ),
        entry(
            "command/qa-test",
            Command,
            "QA Test",
            Reference,
            "QA testing methodology",
            None,
        ),
        entry(
            "command/update-project-state",
            Command,
            "Update Project State",
            Reference,
            "State update patterns",
            None,
        ),
        entry(
            "command/information-topology",
            Command,
            "Information Topology",
            Reference,
            "Information architecture analysis",
            None,
        ),
        entry(
            "command/create-theme",
            Command,
            "Create Theme",
            Reference,
            "Theme creation patterns",
            None,
        ),
        entry(
            "command/remediate-all-commands",
            Command,
            "Remediate All Commands",
            Reference,
            "Bulk command remediation",
            None,
        ),
        entry(
            "command/roundtable-strategic",
            Command,
            "Roundtable Strategic",
            Defer,
            "Multi-agent roundtable exceeds workflow DSL",
            None,
        ),
        entry(
            "command/roundtable-command-debug",
            Command,
            "Roundtable Command Debug",
            Defer,
            "Multi-agent roundtable exceeds workflow DSL",
            None,
        ),
        entry(
            "command/roundtable-ux",
            Command,
            "Roundtable UX",
            Defer,
            "Multi-agent roundtable exceeds workflow DSL",
            None,
        ),
        entry(
            "command/roundtable-people-ops",
            Command,
            "Roundtable People Ops",
            Defer,
            "Company-specific and complex",
            None,
        ),
        // ===== Schemas (22): 6 adapt, 12 reference, 4 defer =====
        entry(
            "schema/bug",
            Schema,
            "Bug Schema",
            Adapt,
            "Maps to conductor ticket type with priority/severity",
            Some(".conductor/schemas/bug.yaml"),
        ),
        entry(
            "schema/decision",
            Schema,
            "Decision Schema",
            Adapt,
            "Maps to .conductor/decisions/ format",
            Some(".conductor/schemas/decision.yaml"),
        ),
        entry(
            "schema/checkpoint",
            Schema,
            "Checkpoint Schema",
            Adapt,
            "Maps to workflow gate checkpoints",
            Some(".conductor/schemas/checkpoint.yaml"),
        ),
        entry(
            "schema/change-request",
            Schema,
            "Change Request Schema",
            Adapt,
            "Maps to conductor ticket lifecycle extension",
            Some(".conductor/schemas/change-request.yaml"),
        ),
        entry(
            "schema/bypass",
            Schema,
            "Bypass Schema",
            Adapt,
            "Maps to gate override/bypass mechanism (DEC-004 escape hatch)",
            Some(".conductor/schemas/bypass.yaml"),
        ),
        entry(
            "schema/escalation",
            Schema,
            "Escalation Schema",
            Adapt,
            "Maps to workflow blocked-on escalation",
            Some(".conductor/schemas/escalation.yaml"),
        ),
        entry(
            "schema/project",
            Schema,
            "Project Schema",
            Reference,
            "Conductor uses repos, not projects",
            None,
        ),
        entry(
            "schema/story",
            Schema,
            "Story Schema",
            Reference,
            "Conductor uses tickets, not stories",
            None,
        ),
        entry(
            "schema/deliverable",
            Schema,
            "Deliverable Schema",
            Reference,
            "No direct conductor equivalent",
            None,
        ),
        entry(
            "schema/goal",
            Schema,
            "Goal Schema",
            Reference,
            "Alignment scoring reference",
            None,
        ),
        entry(
            "schema/objective",
            Schema,
            "Objective Schema",
            Reference,
            "No direct equivalent",
            None,
        ),
        entry(
            "schema/tech-debt",
            Schema,
            "Tech Debt Schema",
            Reference,
            "Useful for future ticket type extension",
            None,
        ),
        entry(
            "schema/tech-request",
            Schema,
            "Tech Request Schema",
            Reference,
            "Useful for future ticket type extension",
            None,
        ),
        entry(
            "schema/user",
            Schema,
            "User Schema",
            Reference,
            "Conductor has no user entity (single-user tool)",
            None,
        ),
        entry(
            "schema/charter",
            Schema,
            "Charter Schema",
            Reference,
            "Organizational scope document",
            None,
        ),
        entry(
            "schema/codebase",
            Schema,
            "Codebase Schema",
            Reference,
            "Conductor uses repos table",
            None,
        ),
        entry(
            "schema/domain",
            Schema,
            "Domain Schema",
            Reference,
            "Organizational grouping",
            None,
        ),
        entry(
            "schema/action-item",
            Schema,
            "Action Item Schema",
            Reference,
            "Meeting-originated action items",
            None,
        ),
        entry(
            "schema/meeting",
            Schema,
            "Meeting Schema",
            Defer,
            "Domain-specific: meeting management",
            None,
        ),
        entry(
            "schema/agenda-item",
            Schema,
            "Agenda Item Schema",
            Defer,
            "Domain-specific: meeting agendas",
            None,
        ),
        entry(
            "schema/workgroup",
            Schema,
            "Workgroup Schema",
            Defer,
            "Domain-specific: organizational workgroups",
            None,
        ),
        entry(
            "schema/readme",
            Schema,
            "README",
            Defer,
            "Documentation, not a schema",
            None,
        ),
        // ===== Decisions (27): 8 adapt, 15 reference, 4 defer =====
        entry(
            "decision/dec-001",
            Decision,
            "Two-Layer Architecture",
            Adapt,
            "Separate agent populations for different concerns",
            Some(".conductor/decisions/dec-001.md"),
        ),
        entry(
            "decision/dec-002",
            Decision,
            "Hard Fork Model",
            Adapt,
            "Independent evolutionary branches",
            Some(".conductor/decisions/dec-002.md"),
        ),
        entry(
            "decision/dec-004",
            Decision,
            "Escape Hatch",
            Adapt,
            "SDLC as tool not constraint",
            Some(".conductor/decisions/dec-004.md"),
        ),
        entry(
            "decision/dec-017",
            Decision,
            "Shared Domain",
            Adapt,
            "Shared domain model",
            Some(".conductor/decisions/dec-017.md"),
        ),
        entry(
            "decision/dec-018",
            Decision,
            "Exit Codes",
            Adapt,
            "Semantic exit codes for workflow scripts",
            Some(".conductor/decisions/dec-018.md"),
        ),
        entry(
            "decision/dec-122",
            Decision,
            "DEC-122",
            Adapt,
            "Assess for applicability",
            Some(".conductor/decisions/dec-122.md"),
        ),
        entry(
            "decision/dec-123",
            Decision,
            "DEC-123",
            Adapt,
            "Assess for applicability",
            Some(".conductor/decisions/dec-123.md"),
        ),
        entry(
            "decision/dec-124",
            Decision,
            "DEC-124",
            Adapt,
            "Assess for applicability",
            Some(".conductor/decisions/dec-124.md"),
        ),
        entry(
            "decision/dec-008",
            Decision,
            "DEC-008",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-009",
            Decision,
            "DEC-009",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-010",
            Decision,
            "DEC-010",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-011",
            Decision,
            "DEC-011",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-012",
            Decision,
            "DEC-012",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-013",
            Decision,
            "DEC-013",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-014",
            Decision,
            "DEC-014",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-015",
            Decision,
            "DEC-015",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-016",
            Decision,
            "DEC-016",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-019",
            Decision,
            "DEC-019",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-020",
            Decision,
            "DEC-020",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-021",
            Decision,
            "DEC-021",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-100",
            Decision,
            "DEC-100",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-200",
            Decision,
            "DEC-200",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-309",
            Decision,
            "DEC-309",
            Reference,
            "Varying degrees of applicability",
            None,
        ),
        entry(
            "decision/dec-lively-001",
            Decision,
            "Lively DEC-001",
            Defer,
            "Company-specific: Lively internal",
            None,
        ),
        entry(
            "decision/dec-lively-002",
            Decision,
            "Lively DEC-002",
            Defer,
            "Company-specific: Lively internal",
            None,
        ),
        entry(
            "decision/dec-vantage-001",
            Decision,
            "Vantage DEC-001",
            Defer,
            "Company-specific: Vantage internal",
            None,
        ),
        entry(
            "decision/dec-vantage-002",
            Decision,
            "Vantage DEC-002",
            Defer,
            "Company-specific: Vantage internal",
            None,
        ),
        // ===== Skills (9): 3 adapt, 4 reference, 2 defer =====
        entry(
            "skill/agent-generator",
            Skill,
            "Agent Generator",
            Adapt,
            "Generate conductor agent definitions",
            Some(".conductor/skills/agent-generator"),
        ),
        entry(
            "skill/qa-verification",
            Skill,
            "QA Verification",
            Adapt,
            "QA verification methodology",
            Some(".conductor/skills/qa-verification"),
        ),
        entry(
            "skill/bug-fix-templates",
            Skill,
            "Bug Fix Templates",
            Adapt,
            "Bug fix workflow templates",
            Some(".conductor/skills/bug-fix-templates"),
        ),
        entry(
            "skill/debug-analysis-patterns",
            Skill,
            "Debug Analysis Patterns",
            Reference,
            "Debugging methodology",
            None,
        ),
        entry(
            "skill/research-readiness-assessment",
            Skill,
            "Research Readiness Assessment",
            Reference,
            "Assessment rubric",
            None,
        ),
        entry(
            "skill/slash-command-remediation",
            Skill,
            "Slash Command Remediation",
            Reference,
            "Command design patterns",
            None,
        ),
        entry(
            "skill/test-structure-templates",
            Skill,
            "Test Structure Templates",
            Reference,
            "Test organization patterns",
            None,
        ),
        entry(
            "skill/cli-tool-generator",
            Skill,
            "CLI Tool Generator",
            Defer,
            "Go-specific: CLI generation for Go",
            None,
        ),
        entry(
            "skill/test-data-management",
            Skill,
            "Test Data Management",
            Defer,
            "Domain-specific: test data patterns",
            None,
        ),
        // ===== Protocols (2): 1 adapt, 1 reference =====
        entry(
            "protocol/output-contract",
            Protocol,
            "OUTPUT_CONTRACT.md",
            Adapt,
            "Output behavior rules for conductor agents",
            Some(".conductor/protocols/output-contract.md"),
        ),
        entry(
            "protocol/claude-md",
            Protocol,
            "CLAUDE.md",
            Reference,
            "Conductor already has its own CLAUDE.md with different structure",
            None,
        ),
    ]
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_111_entries() {
        let catalog = OsCatalog::load();
        assert_eq!(catalog.entries.len(), 111);
    }

    #[test]
    fn catalog_total_counts() {
        let catalog = OsCatalog::load();
        let (total, adapt, reference, defer) = catalog.total_counts();
        assert_eq!(total, 111);
        assert_eq!(adapt, 38);
        assert_eq!(reference, 49);
        assert_eq!(defer, 24);
    }

    #[test]
    fn catalog_agent_counts() {
        let catalog = OsCatalog::load();
        let (adapt, reference, defer) = catalog.counts_by_category(&OsCategory::Agent);
        assert_eq!(adapt, 12);
        assert_eq!(reference, 10);
        assert_eq!(defer, 10);
    }

    #[test]
    fn catalog_command_counts() {
        let catalog = OsCatalog::load();
        let (adapt, reference, defer) = catalog.counts_by_category(&OsCategory::Command);
        assert_eq!(adapt, 8);
        assert_eq!(reference, 7);
        assert_eq!(defer, 4);
    }

    #[test]
    fn catalog_schema_counts() {
        let catalog = OsCatalog::load();
        let (adapt, reference, defer) = catalog.counts_by_category(&OsCategory::Schema);
        assert_eq!(adapt, 6);
        assert_eq!(reference, 12);
        assert_eq!(defer, 4);
    }

    #[test]
    fn catalog_decision_counts() {
        let catalog = OsCatalog::load();
        let (adapt, reference, defer) = catalog.counts_by_category(&OsCategory::Decision);
        assert_eq!(adapt, 8);
        assert_eq!(reference, 15);
        assert_eq!(defer, 4);
    }

    #[test]
    fn catalog_skill_counts() {
        let catalog = OsCatalog::load();
        let (adapt, reference, defer) = catalog.counts_by_category(&OsCategory::Skill);
        assert_eq!(adapt, 3);
        assert_eq!(reference, 4);
        assert_eq!(defer, 2);
    }

    #[test]
    fn catalog_protocol_counts() {
        let catalog = OsCatalog::load();
        let (adapt, reference, defer) = catalog.counts_by_category(&OsCategory::Protocol);
        assert_eq!(adapt, 1);
        assert_eq!(reference, 1);
        assert_eq!(defer, 0);
    }

    #[test]
    fn catalog_filter_by_decision() {
        let catalog = OsCatalog::load();
        let adapted = catalog.entries_by_decision(&TriageDecision::Adapt);
        assert_eq!(adapted.len(), 38);
        // All adapted entries should have a target path
        for entry in &adapted {
            assert!(
                entry.target_path.is_some(),
                "Adapted entry {} should have a target path",
                entry.os_id
            );
        }
    }

    #[test]
    fn catalog_filter_by_category() {
        let catalog = OsCatalog::load();
        let agents = catalog.entries_by_category(&OsCategory::Agent);
        assert_eq!(agents.len(), 32);
    }
}
