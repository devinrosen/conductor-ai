//! Persistence decorator that records gate metadata (`gate_type`, `gate_prompt`,
//! `gate_timeout`) on gate steps.
//!
//! runkon-flow's `execute_gate` inserts the gate step and marks it `waiting`
//! through [`WorkflowPersistence`], whose `NewStep` / `StepUpdate` carry no gate
//! fields. Without this decorator every gate row has NULL `gate_type`, and
//! `find_waiting_gate` can never find it (#3151). Remove once runkon persists
//! gate metadata itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use runkon_flow::dsl::{WorkflowDef, WorkflowNode, QUALITY_GATE_TYPE};
use runkon_flow::engine_error::EngineError;
use runkon_flow::status::WorkflowRunStatus;
use runkon_flow::traits::persistence::{
    FanOutItemStatus, FanOutItemUpdate, GateApprovalState, GateApprovalStore, NewRun, NewStep,
    StepUpdate, WorkflowPersistence,
};
use runkon_flow::types::{FanOutItemRow, WorkflowRun, WorkflowRunStep};
use rusqlite::{named_params, Connection};

/// Prompt and timeout for a gate name, when every gate with that name agrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateMeta {
    pub prompt: Option<String>,
    /// Formatted `"{secs}s"`, matching `helpers::parse_gate_timeout_secs`.
    pub timeout: String,
}

/// Build a gate-name → metadata table. A name shared by gates that disagree on
/// prompt or timeout maps to `None` (a wrong prompt is worse than none).
pub(crate) fn collect_gate_meta(def: &WorkflowDef) -> HashMap<String, Option<GateMeta>> {
    fn walk(nodes: &[WorkflowNode], out: &mut HashMap<String, Option<GateMeta>>) {
        for node in nodes {
            if let WorkflowNode::Gate(g) = node {
                if g.gate_type == QUALITY_GATE_TYPE {
                    continue;
                }
                let meta = GateMeta {
                    prompt: g.prompt.clone(),
                    timeout: format!("{}s", g.timeout_secs),
                };
                match out.get(&g.name) {
                    None => {
                        out.insert(g.name.clone(), Some(meta));
                    }
                    Some(Some(existing)) if *existing == meta => {}
                    Some(_) => {
                        out.insert(g.name.clone(), None);
                    }
                }
            }
            if let Some(body) = node.body() {
                walk(body, out);
            }
        }
    }
    let mut out = HashMap::new();
    walk(&def.body, &mut out);
    walk(&def.always, &mut out);
    out
}

pub(crate) struct GateMetadataPersistence {
    inner: Arc<dyn WorkflowPersistence>,
    conn: Arc<Mutex<Connection>>,
    gates: HashMap<String, Option<GateMeta>>,
}

impl GateMetadataPersistence {
    pub(crate) fn new(
        inner: Arc<dyn WorkflowPersistence>,
        conn: Arc<Mutex<Connection>>,
        gates: HashMap<String, Option<GateMeta>>,
    ) -> Self {
        Self { inner, conn, gates }
    }

    /// Best-effort: a failed write is logged with run/step context but does not
    /// fail the step. `gate_type` is backfilled from the step name on read, so
    /// approval still works; only `gate_prompt` and `gate_timeout` are lost.
    fn write_gate_metadata(&self, run_id: &str, step_id: &str, step_name: &str) {
        let meta = self.gates.get(step_name).and_then(|m| m.as_ref());
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "gate metadata: db mutex poisoned for run {run_id}, step {step_name} ({step_id}): {e}"
                );
                return;
            }
        };
        // The DSL guarantees gate step name == gate type.
        if let Err(e) = conn.execute(
            "UPDATE workflow_run_steps SET gate_type = :gate_type, \
                 gate_prompt = COALESCE(:prompt, gate_prompt), \
                 gate_timeout = COALESCE(:timeout, gate_timeout) WHERE id = :id",
            named_params![
                ":gate_type": step_name,
                ":prompt": meta.and_then(|m| m.prompt.as_deref()),
                ":timeout": meta.map(|m| m.timeout.as_str()),
                ":id": step_id,
            ],
        ) {
            tracing::warn!(
                "failed to record gate metadata for run {run_id}, step {step_name} ({step_id}): {e}"
            );
        }
    }
}

impl GateApprovalStore for GateMetadataPersistence {
    fn get_gate_approval(&self, step_id: &str) -> Result<GateApprovalState, EngineError> {
        self.inner.get_gate_approval(step_id)
    }
    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), EngineError> {
        self.inner
            .approve_gate(step_id, approved_by, feedback, selections)
    }
    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError> {
        self.inner.reject_gate(step_id, rejected_by, feedback)
    }
}

impl WorkflowPersistence for GateMetadataPersistence {
    fn create_run(&self, new_run: NewRun) -> Result<WorkflowRun, EngineError> {
        self.inner.create_run(new_run)
    }
    fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>, EngineError> {
        self.inner.get_run(run_id)
    }
    fn list_active_runs(
        &self,
        statuses: &[WorkflowRunStatus],
    ) -> Result<Vec<WorkflowRun>, EngineError> {
        self.inner.list_active_runs(statuses)
    }
    fn update_run_status(
        &self,
        run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError> {
        self.inner
            .update_run_status(run_id, status, result_summary, error)
    }
    fn insert_step(&self, new_step: NewStep) -> Result<String, EngineError> {
        let is_gate = new_step.role == "gate" && new_step.step_name != QUALITY_GATE_TYPE;
        let step_name = new_step.step_name.clone();
        let run_id = new_step.workflow_run_id.clone();
        // Delegate first, without holding our own lock: inner locks the same mutex.
        let step_id = self.inner.insert_step(new_step)?;
        if is_gate {
            self.write_gate_metadata(&run_id, &step_id, &step_name);
        }
        Ok(step_id)
    }
    fn update_step(&self, step_id: &str, update: StepUpdate) -> Result<(), EngineError> {
        self.inner.update_step(step_id, update)
    }
    fn get_steps(&self, run_id: &str) -> Result<Vec<WorkflowRunStep>, EngineError> {
        self.inner.get_steps(run_id)
    }
    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
        context: &HashMap<String, String>,
    ) -> Result<String, EngineError> {
        self.inner
            .insert_fan_out_item(step_run_id, item_type, item_id, item_ref, context)
    }
    fn update_fan_out_item(
        &self,
        item_id: &str,
        update: FanOutItemUpdate,
    ) -> Result<(), EngineError> {
        self.inner.update_fan_out_item(item_id, update)
    }
    fn batch_update_fan_out_items(
        &self,
        updates: &[(String, FanOutItemUpdate)],
    ) -> Result<(), EngineError> {
        self.inner.batch_update_fan_out_items(updates)
    }
    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<FanOutItemStatus>,
    ) -> Result<Vec<FanOutItemRow>, EngineError> {
        self.inner.get_fan_out_items(step_run_id, status_filter)
    }
    fn acquire_lease(
        &self,
        run_id: &str,
        token: &str,
        ttl_seconds: i64,
    ) -> Result<Option<i64>, EngineError> {
        self.inner.acquire_lease(run_id, token, ttl_seconds)
    }
    fn is_run_cancelled(&self, run_id: &str) -> Result<bool, EngineError> {
        self.inner.is_run_cancelled(run_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::persistence_sqlite::SqliteWorkflowPersistence;

    fn parse(src: &str) -> WorkflowDef {
        runkon_flow::dsl::parse_workflow_str(src, "test.wf").unwrap()
    }

    const WF: &str = r#"
workflow w {
  meta { trigger = "manual" targets = ["worktree"] }
  gate human_approval { prompt = "First" timeout = "1h" }
  if a.b {
    gate human_review { prompt = "Second" timeout = "2h" }
  }
  while a.c {
    max_iterations = 2
    gate pr_checks { timeout = "5m" }
  }
  always {
    gate pr_approval { prompt = "Last" timeout = "10m" }
  }
}
"#;

    #[test]
    fn collects_gates_in_nested_bodies() {
        let m = collect_gate_meta(&parse(WF));
        assert_eq!(
            m["human_approval"],
            Some(GateMeta {
                prompt: Some("First".into()),
                timeout: "3600s".into()
            })
        );
        assert_eq!(m["human_review"].as_ref().unwrap().timeout, "7200s");
        assert!(m.contains_key("pr_checks"));
        assert_eq!(m["pr_approval"].as_ref().unwrap().timeout, "600s");
    }

    #[test]
    fn same_name_different_prompt_is_ambiguous() {
        let src = r#"
workflow w {
  meta { trigger = "manual" targets = ["worktree"] }
  gate human_approval { prompt = "A" timeout = "1h" }
  gate human_approval { prompt = "B" timeout = "1h" }
}
"#;
        assert_eq!(collect_gate_meta(&parse(src))["human_approval"], None);
    }

    fn setup() -> (GateMetadataPersistence, Arc<Mutex<Connection>>, String) {
        let conn = crate::test_helpers::setup_db();
        let parent = crate::agent::AgentManager::new(&conn)
            .create_run(Some("w1"), "workflow", None)
            .unwrap();
        let shared = Arc::new(Mutex::new(conn));
        let inner: Arc<dyn WorkflowPersistence> = Arc::new(
            SqliteWorkflowPersistence::from_shared_connection(Arc::clone(&shared)),
        );
        let p =
            GateMetadataPersistence::new(inner, Arc::clone(&shared), collect_gate_meta(&parse(WF)));
        let run = p
            .create_run(NewRun {
                workflow_name: "w".into(),
                parent_run_id: parent.id,
                dry_run: false,
                trigger: "manual".into(),
                definition_snapshot: None,
                parent_workflow_run_id: None,
            })
            .unwrap();
        (p, shared, run.id)
    }

    fn new_step(run: &str, name: &str, role: &str) -> NewStep {
        NewStep {
            workflow_run_id: run.to_string(),
            step_name: name.to_string(),
            role: role.to_string(),
            can_commit: false,
            position: 0,
            iteration: 0,
            retry_count: None,
        }
    }

    fn gate_cols(
        conn: &Arc<Mutex<Connection>>,
        id: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        conn.lock()
            .unwrap()
            .query_row(
                "SELECT gate_type, gate_prompt, gate_timeout FROM workflow_run_steps WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    #[test]
    fn gate_insert_records_metadata() {
        let (p, conn, run) = setup();
        let id = p
            .insert_step(new_step(&run, "human_review", "gate"))
            .unwrap();
        assert_eq!(
            gate_cols(&conn, &id),
            (
                Some("human_review".into()),
                Some("Second".into()),
                Some("7200s".into())
            )
        );
    }

    #[test]
    fn unknown_gate_name_still_gets_type() {
        let (p, conn, run) = setup();
        let id = p
            .insert_step(new_step(&run, "custom_gate", "gate"))
            .unwrap();
        assert_eq!(
            gate_cols(&conn, &id),
            (Some("custom_gate".into()), None, None)
        );
    }

    #[test]
    fn non_gate_role_untouched() {
        let (p, conn, run) = setup();
        let id = p
            .insert_step(new_step(&run, "human_review", "actor"))
            .unwrap();
        assert_eq!(gate_cols(&conn, &id), (None, None, None));
    }
}
