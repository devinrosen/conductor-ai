//! Agent communication infrastructure: decisions, handoffs, blockers, delegations, council.
//!
//! Provides typed CRUD operations for the 7 communication tables created in
//! migration v049 (agent_decisions, agent_handoffs, agent_blockers, agent_delegations,
//! council_sessions, council_votes) and v050 (agent_artifacts).
//!
//! Part of: structured-handoff-protocol@1.1.0, decision-log-as-shared-memory@1.0.0,
//! threaded-blocker-comments@1.1.0, cross-agent-delegation-protocol@1.0.0,
//! council-decision-architecture@1.0.0, artifact-mediated-agent-communication@1.0.0,
//! roundtable-structured-reconciliation@1.0.0

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

// ─── Decision Log ────────────────────────────────────────────────────────────

/// A recorded decision in the shared decision log.
/// Part of: decision-log-as-shared-memory@1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDecision {
    pub id: String,
    pub workflow_run_id: Option<String>,
    pub feature_id: Option<String>,
    pub sequence_number: i64,
    pub context: String,
    pub decision: String,
    pub rationale: String,
    pub agent_run_id: String,
    pub agent_name: Option<String>,
    pub supersedes_id: Option<String>,
    pub created_at: String,
}

/// Record a decision in the append-only log.
#[allow(clippy::too_many_arguments)]
pub fn record_decision(
    conn: &Connection,
    workflow_run_id: Option<&str>,
    feature_id: Option<&str>,
    context: &str,
    decision: &str,
    rationale: &str,
    agent_run_id: &str,
    agent_name: Option<&str>,
    supersedes_id: Option<&str>,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();

    // Get next sequence number for scope
    let seq: i64 = if let Some(wfr_id) = workflow_run_id {
        conn.query_row(
            "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM agent_decisions WHERE workflow_run_id = ?1",
            params![wfr_id],
            |row| row.get(0),
        )?
    } else {
        1
    };

    conn.execute(
        "INSERT INTO agent_decisions (id, workflow_run_id, feature_id, sequence_number, context, decision, rationale, agent_run_id, agent_name, supersedes_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![id, workflow_run_id, feature_id, seq, context, decision, rationale, agent_run_id, agent_name, supersedes_id],
    )?;

    Ok(id)
}

/// List decisions for a workflow run, ordered by sequence number.
pub fn list_decisions(conn: &Connection, workflow_run_id: &str) -> Result<Vec<AgentDecision>> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_run_id, feature_id, sequence_number, context, decision, rationale, agent_run_id, agent_name, supersedes_id, created_at
         FROM agent_decisions WHERE workflow_run_id = ?1 ORDER BY sequence_number",
    )?;
    let rows = stmt
        .query_map(params![workflow_run_id], |row| {
            Ok(AgentDecision {
                id: row.get(0)?,
                workflow_run_id: row.get(1)?,
                feature_id: row.get(2)?,
                sequence_number: row.get(3)?,
                context: row.get(4)?,
                decision: row.get(5)?,
                rationale: row.get(6)?,
                agent_run_id: row.get(7)?,
                agent_name: row.get(8)?,
                supersedes_id: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ─── Handoffs ────────────────────────────────────────────────────────────────

/// A structured handoff between workflow phases.
/// Part of: structured-handoff-protocol@1.1.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHandoff {
    pub id: String,
    pub workflow_run_id: String,
    pub from_step_id: Option<String>,
    pub to_step_id: Option<String>,
    pub payload: String,
    pub producer_agent: String,
    pub consumer_agent: Option<String>,
    pub validated: bool,
    pub created_at: String,
}

/// Create a handoff record.
pub fn create_handoff(
    conn: &Connection,
    workflow_run_id: &str,
    from_step_id: Option<&str>,
    to_step_id: Option<&str>,
    payload: &str,
    producer_agent: &str,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO agent_handoffs (id, workflow_run_id, from_step_id, to_step_id, payload, producer_agent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, workflow_run_id, from_step_id, to_step_id, payload, producer_agent],
    )?;
    Ok(id)
}

/// Get handoffs for a workflow run.
pub fn list_handoffs(conn: &Connection, workflow_run_id: &str) -> Result<Vec<AgentHandoff>> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_run_id, from_step_id, to_step_id, payload, producer_agent, consumer_agent, validated, created_at
         FROM agent_handoffs WHERE workflow_run_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![workflow_run_id], |row| {
            Ok(AgentHandoff {
                id: row.get(0)?,
                workflow_run_id: row.get(1)?,
                from_step_id: row.get(2)?,
                to_step_id: row.get(3)?,
                payload: row.get(4)?,
                producer_agent: row.get(5)?,
                consumer_agent: row.get(6)?,
                validated: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ─── Blockers ────────────────────────────────────────────────────────────────

/// A threaded blocker record.
/// Part of: threaded-blocker-comments@1.1.0, fail-forward-with-blocker-aggregation@1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBlocker {
    pub id: String,
    pub workflow_run_id: Option<String>,
    pub workflow_step_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub parent_blocker_id: Option<String>,
    pub severity: String,
    pub category: Option<String>,
    pub summary: String,
    pub detail: Option<String>,
    pub status: String,
    pub resolved_by: Option<String>,
    pub resolution_note: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// Record a blocker.
#[allow(clippy::too_many_arguments)]
pub fn create_blocker(
    conn: &Connection,
    workflow_run_id: Option<&str>,
    workflow_step_id: Option<&str>,
    agent_run_id: Option<&str>,
    parent_blocker_id: Option<&str>,
    severity: &str,
    category: Option<&str>,
    summary: &str,
    detail: Option<&str>,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO agent_blockers (id, workflow_run_id, workflow_step_id, agent_run_id, parent_blocker_id, severity, category, summary, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![id, workflow_run_id, workflow_step_id, agent_run_id, parent_blocker_id, severity, category, summary, detail],
    )?;
    Ok(id)
}

/// Resolve a blocker.
pub fn resolve_blocker(
    conn: &Connection,
    blocker_id: &str,
    resolved_by: &str,
    resolution_note: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE agent_blockers SET status = 'resolved', resolved_by = ?1, resolution_note = ?2, resolved_at = datetime('now') WHERE id = ?3",
        params![resolved_by, resolution_note, blocker_id],
    )?;
    Ok(())
}

/// List open blockers for a workflow run.
pub fn list_open_blockers(conn: &Connection, workflow_run_id: &str) -> Result<Vec<AgentBlocker>> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_run_id, workflow_step_id, agent_run_id, parent_blocker_id, severity, category, summary, detail, status, resolved_by, resolution_note, created_at, resolved_at
         FROM agent_blockers WHERE workflow_run_id = ?1 AND status = 'open' ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![workflow_run_id], |row| {
            Ok(AgentBlocker {
                id: row.get(0)?,
                workflow_run_id: row.get(1)?,
                workflow_step_id: row.get(2)?,
                agent_run_id: row.get(3)?,
                parent_blocker_id: row.get(4)?,
                severity: row.get(5)?,
                category: row.get(6)?,
                summary: row.get(7)?,
                detail: row.get(8)?,
                status: row.get(9)?,
                resolved_by: row.get(10)?,
                resolution_note: row.get(11)?,
                created_at: row.get(12)?,
                resolved_at: row.get(13)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ─── Delegations ─────────────────────────────────────────────────────────────

/// An agent delegation record.
/// Part of: cross-agent-delegation-protocol@1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDelegation {
    pub id: String,
    pub delegator_run_id: String,
    pub delegate_run_id: Option<String>,
    pub target_role: String,
    pub context_envelope: String,
    pub status: String,
    pub outcome: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Create a delegation request.
pub fn create_delegation(
    conn: &Connection,
    delegator_run_id: &str,
    target_role: &str,
    context_envelope: &str,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO agent_delegations (id, delegator_run_id, target_role, context_envelope) VALUES (?1, ?2, ?3, ?4)",
        params![id, delegator_run_id, target_role, context_envelope],
    )?;
    Ok(id)
}

/// Complete a delegation with outcome.
pub fn complete_delegation(conn: &Connection, delegation_id: &str, outcome: &str) -> Result<()> {
    conn.execute(
        "UPDATE agent_delegations SET status = 'completed', outcome = ?1, completed_at = datetime('now') WHERE id = ?2",
        params![outcome, delegation_id],
    )?;
    Ok(())
}

// ─── Council ─────────────────────────────────────────────────────────────────

/// A council decision session.
/// Part of: council-decision-architecture@1.0.0, roundtable-structured-reconciliation@1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilSession {
    pub id: String,
    pub workflow_run_id: Option<String>,
    pub question: String,
    pub quorum: i64,
    pub decision_method: String,
    pub status: String,
    pub reconciled_decision: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
}

/// A vote in a council session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilVote {
    pub id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub agent_role: String,
    pub vote: String,
    pub confidence: Option<f64>,
    pub rationale: Option<String>,
    pub created_at: String,
}

/// Create a council session.
pub fn create_council_session(
    conn: &Connection,
    workflow_run_id: Option<&str>,
    question: &str,
    quorum: i64,
    decision_method: &str,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO council_sessions (id, workflow_run_id, question, quorum, decision_method) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, workflow_run_id, question, quorum, decision_method],
    )?;
    Ok(id)
}

/// Cast a vote in a council session.
pub fn cast_vote(
    conn: &Connection,
    session_id: &str,
    agent_run_id: &str,
    agent_role: &str,
    vote: &str,
    confidence: Option<f64>,
    rationale: Option<&str>,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO council_votes (id, session_id, agent_run_id, agent_role, vote, confidence, rationale) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, session_id, agent_run_id, agent_role, vote, confidence, rationale],
    )?;
    Ok(id)
}

/// Get all votes for a council session.
pub fn get_votes(conn: &Connection, session_id: &str) -> Result<Vec<CouncilVote>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, agent_run_id, agent_role, vote, confidence, rationale, created_at
         FROM council_votes WHERE session_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(CouncilVote {
                id: row.get(0)?,
                session_id: row.get(1)?,
                agent_run_id: row.get(2)?,
                agent_role: row.get(3)?,
                vote: row.get(4)?,
                confidence: row.get(5)?,
                rationale: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Reconcile a council session with a final decision.
/// Part of: roundtable-structured-reconciliation@1.0.0
pub fn reconcile_session(
    conn: &Connection,
    session_id: &str,
    reconciled_decision: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE council_sessions SET status = 'decided', reconciled_decision = ?1, decided_at = datetime('now') WHERE id = ?2",
        params![reconciled_decision, session_id],
    )?;
    Ok(())
}

// ─── Artifacts ───────────────────────────────────────────────────────────────

/// An agent artifact record.
/// Part of: artifact-mediated-agent-communication@1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentArtifact {
    pub id: String,
    pub workflow_run_id: Option<String>,
    pub agent_run_id: String,
    pub artifact_type: String,
    pub name: String,
    pub content: String,
    pub format: String,
    pub created_at: String,
}

/// Store an artifact.
pub fn store_artifact(
    conn: &Connection,
    workflow_run_id: Option<&str>,
    agent_run_id: &str,
    artifact_type: &str,
    name: &str,
    content: &str,
    format: &str,
) -> Result<String> {
    let id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO agent_artifacts (id, workflow_run_id, agent_run_id, artifact_type, name, content, format) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, workflow_run_id, agent_run_id, artifact_type, name, content, format],
    )?;
    Ok(id)
}

/// List artifacts for a workflow run.
pub fn list_artifacts(conn: &Connection, workflow_run_id: &str) -> Result<Vec<AgentArtifact>> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_run_id, agent_run_id, artifact_type, name, content, format, created_at
         FROM agent_artifacts WHERE workflow_run_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![workflow_run_id], |row| {
            Ok(AgentArtifact {
                id: row.get(0)?,
                workflow_run_id: row.get(1)?,
                agent_run_id: row.get(2)?,
                artifact_type: row.get(3)?,
                name: row.get(4)?,
                content: row.get(5)?,
                format: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ─── Output Behavior Contract ────────────────────────────────────────────────

/// Output behavior contract enforced via typed deserialization.
/// Part of: output-behavior-contract@1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputContract {
    /// Expected output sections (e.g., "summary", "changes", "risks").
    pub required_sections: Vec<String>,
    /// Maximum length per section (0 = unlimited).
    pub max_section_length: usize,
    /// Format constraint (e.g., "markdown", "json").
    pub format: String,
}

impl Default for OutputContract {
    fn default() -> Self {
        Self {
            required_sections: vec![],
            max_section_length: 0,
            format: "markdown".to_string(),
        }
    }
}

/// Validate agent output against a contract.
pub fn validate_output(output: &str, contract: &OutputContract) -> Vec<String> {
    let mut violations = Vec::new();

    for section in &contract.required_sections {
        let header = format!("## {section}");
        let header_lower = format!("## {}", section.to_lowercase());
        if !output.contains(&header) && !output.to_lowercase().contains(&header_lower) {
            violations.push(format!("missing required section: {section}"));
        }
    }

    if contract.max_section_length > 0 {
        for section in &contract.required_sections {
            let header = format!("## {section}");
            if let Some(start) = output.find(&header) {
                let rest = &output[start + header.len()..];
                let end = rest.find("\n## ").unwrap_or(rest.len());
                let section_text = &rest[..end];
                if section_text.len() > contract.max_section_length {
                    violations.push(format!(
                        "section '{section}' exceeds max length ({} > {})",
                        section_text.len(),
                        contract.max_section_length
                    ));
                }
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = crate::test_helpers::setup_db();
        conn
    }

    #[test]
    fn decision_crud() {
        let conn = setup_db();
        // Create a minimal agent run for FK
        // Create FK-satisfying agent run (worktree_id nullable after migration 032)
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar1', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        let id = record_decision(
            &conn,
            None,
            None,
            "Should we use SQLite?",
            "Yes",
            "It's already in use",
            "ar1",
            Some("test-agent"),
            None,
        )
        .unwrap();
        assert!(!id.is_empty());
    }

    #[test]
    fn blocker_create_and_resolve() {
        let conn = setup_db();
        // Create FK-satisfying agent run (worktree_id nullable after migration 032)
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar1', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        let id = create_blocker(
            &conn,
            None,
            None,
            Some("ar1"),
            None,
            "high",
            Some("build_failure"),
            "cargo build fails",
            Some("error[E0308]: mismatched types"),
        )
        .unwrap();

        resolve_blocker(&conn, &id, "ar1", "Fixed the type mismatch").unwrap();
    }

    #[test]
    fn council_voting_flow() {
        let conn = setup_db();
        // Create FK-satisfying agent run (worktree_id nullable after migration 032)
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar1', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar2', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        let session_id =
            create_council_session(&conn, None, "Should we refactor?", 2, "majority").unwrap();

        cast_vote(
            &conn,
            &session_id,
            "ar1",
            "reviewer",
            "yes",
            Some(0.9),
            Some("Improves maintainability"),
        )
        .unwrap();
        cast_vote(
            &conn,
            &session_id,
            "ar2",
            "reviewer",
            "yes",
            Some(0.7),
            None,
        )
        .unwrap();

        let votes = get_votes(&conn, &session_id).unwrap();
        assert_eq!(votes.len(), 2);

        reconcile_session(&conn, &session_id, "Approved: refactor with 2/2 votes").unwrap();
    }

    #[test]
    fn output_contract_validation() {
        let contract = OutputContract {
            required_sections: vec!["Summary".to_string(), "Changes".to_string()],
            max_section_length: 0,
            format: "markdown".to_string(),
        };

        let good = "## Summary\nDid stuff\n## Changes\nChanged things";
        assert!(validate_output(good, &contract).is_empty());

        let missing = "## Summary\nDid stuff";
        let violations = validate_output(missing, &contract);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("Changes"));
    }

    #[test]
    fn handoff_crud() {
        let conn = setup_db();
        // Create FK-satisfying agent run (worktree_id nullable after migration 032)
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar1', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();
        // Need a workflow run for FK — parent_run_id and started_at are NOT NULL
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, status, parent_run_id, started_at) VALUES ('wr1', 'test-wf', 'running', 'ar1', datetime('now'))",
            [],
        )
        .unwrap();

        let id = create_handoff(
            &conn,
            "wr1",
            None,
            None,
            r#"{"overview": "phase 1 complete"}"#,
            "test-agent",
        )
        .unwrap();
        assert!(!id.is_empty());

        let handoffs = list_handoffs(&conn, "wr1").unwrap();
        assert_eq!(handoffs.len(), 1);
    }

    #[test]
    fn delegation_crud() {
        let conn = setup_db();
        // Create FK-satisfying agent run (worktree_id nullable after migration 032)
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar1', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        let id = create_delegation(
            &conn,
            "ar1",
            "reviewer",
            r#"{"subtask": "review PR", "constraints": []}"#,
        )
        .unwrap();

        complete_delegation(&conn, &id, r#"{"approved": true}"#).unwrap();
    }

    #[test]
    fn artifact_crud() {
        let conn = setup_db();
        // Create FK-satisfying agent run (worktree_id nullable after migration 032)
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at) VALUES ('ar1', 'test', 'completed', datetime('now'))",
            [],
        )
        .unwrap();

        let id = store_artifact(
            &conn,
            None,
            "ar1",
            "report",
            "analysis.md",
            "# Analysis\nLooks good",
            "markdown",
        )
        .unwrap();
        assert!(!id.is_empty());
    }
}
