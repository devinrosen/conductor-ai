use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::dsl::{AgentRef, CallNode, WorkflowDef, WorkflowNode, WorkflowTrigger};
use crate::events::{EngineEventData, EventSink};
use crate::traits::action_executor::{ActionParams, ExecutionContext};

pub fn make_ectx() -> ExecutionContext {
    ExecutionContext {
        run_id: "r1".to_string(),
        working_dir: PathBuf::from("/tmp"),
        repo_path: "/tmp/repo".to_string(),
        step_timeout: Duration::from_secs(60),
        shutdown: None,
        model: None,
        bot_name: None,
        plugin_dirs: vec![],
        workflow_name: "wf".to_string(),
        worktree_id: None,
        parent_run_id: "parent-run-1".to_string(),
        step_id: "step-1".to_string(),
    }
}

pub fn make_params(name: &str) -> ActionParams {
    ActionParams {
        name: name.to_string(),
        inputs: Arc::new(HashMap::new()),
        retries_remaining: 0,
        retry_error: None,
        snippets: vec![],
        dry_run: false,
        gate_feedback: None,
        schema: None,
    }
}

/// Collects all emitted events for post-run inspection.
pub struct VecSink {
    pub events: Mutex<Vec<EngineEventData>>,
}

impl VecSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
        })
    }

    pub fn collected(&self) -> Vec<EngineEventData> {
        self.events.lock().unwrap().clone()
    }
}

impl EventSink for VecSink {
    fn emit(&self, event: &EngineEventData) {
        self.events.lock().unwrap().push(event.clone());
    }
}

/// Forwards events to a `VecSink` — used so tests keep an `Arc<VecSink>` to read
/// collected events after `run()` completes while `FlowEngineBuilder` owns the sink.
pub struct ForwardSink(pub Arc<VecSink>);

impl EventSink for ForwardSink {
    fn emit(&self, event: &EngineEventData) {
        self.0.emit(event);
    }
}

pub fn make_def(name: &str, body: Vec<WorkflowNode>) -> WorkflowDef {
    WorkflowDef {
        name: name.to_string(),
        title: None,
        description: String::new(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
        inputs: vec![],
        body,
        always: vec![],
        source_path: "test.wf".to_string(),
    }
}

pub fn call_node(agent: &str) -> WorkflowNode {
    WorkflowNode::Call(CallNode {
        agent: AgentRef::Name(agent.to_string()),
        retries: 0,
        on_fail: None,
        output: None,
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
        timeout: None,
    })
}

/// `WorkflowPersistence` decorator that delegates to `InMemoryWorkflowPersistence`
/// and counts every call to `tick_heartbeat`. Also lets tests force
/// `is_run_cancelled` to return true at will.
///
/// Built for the regression coverage in #2731: wait loops in `parallel` and
/// `foreach` must keep `tick_heartbeat` firing while children are running so
/// the watchdog reaper does not race the engine.
pub struct CountingPersistence {
    inner: crate::persistence_memory::InMemoryWorkflowPersistence,
    tick_count: std::sync::atomic::AtomicUsize,
    cancelled: std::sync::atomic::AtomicBool,
}

impl Default for CountingPersistence {
    fn default() -> Self {
        Self::new()
    }
}

impl CountingPersistence {
    pub fn new() -> Self {
        Self {
            inner: crate::persistence_memory::InMemoryWorkflowPersistence::new(),
            tick_count: std::sync::atomic::AtomicUsize::new(0),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub fn tick_count(&self) -> usize {
        self.tick_count.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn set_cancelled(&self, v: bool) {
        self.cancelled
            .store(v, std::sync::atomic::Ordering::Relaxed);
    }
}

impl crate::traits::persistence::WorkflowPersistence for CountingPersistence {
    fn is_run_cancelled(&self, run_id: &str) -> Result<bool, crate::engine_error::EngineError> {
        if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(true);
        }
        self.inner.is_run_cancelled(run_id)
    }
    fn tick_heartbeat(&self, run_id: &str) -> Result<(), crate::engine_error::EngineError> {
        self.tick_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.tick_heartbeat(run_id)
    }
    fn create_run(
        &self,
        r: crate::traits::persistence::NewRun,
    ) -> Result<crate::types::WorkflowRun, crate::engine_error::EngineError> {
        self.inner.create_run(r)
    }
    fn get_run(
        &self,
        id: &str,
    ) -> Result<Option<crate::types::WorkflowRun>, crate::engine_error::EngineError> {
        self.inner.get_run(id)
    }
    fn list_active_runs(
        &self,
        s: &[crate::status::WorkflowRunStatus],
    ) -> Result<Vec<crate::types::WorkflowRun>, crate::engine_error::EngineError> {
        self.inner.list_active_runs(s)
    }
    fn update_run_status(
        &self,
        id: &str,
        s: crate::status::WorkflowRunStatus,
        result_summary: Option<&str>,
        err: Option<&str>,
    ) -> Result<(), crate::engine_error::EngineError> {
        self.inner.update_run_status(id, s, result_summary, err)
    }
    fn insert_step(
        &self,
        s: crate::traits::persistence::NewStep,
    ) -> Result<String, crate::engine_error::EngineError> {
        self.inner.insert_step(s)
    }
    fn update_step(
        &self,
        id: &str,
        u: crate::traits::persistence::StepUpdate,
    ) -> Result<(), crate::engine_error::EngineError> {
        self.inner.update_step(id, u)
    }
    fn get_steps(
        &self,
        run_id: &str,
    ) -> Result<Vec<crate::types::WorkflowRunStep>, crate::engine_error::EngineError> {
        self.inner.get_steps(run_id)
    }
    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, crate::engine_error::EngineError> {
        self.inner
            .insert_fan_out_item(step_run_id, item_type, item_id, item_ref)
    }
    fn update_fan_out_item(
        &self,
        id: &str,
        u: crate::traits::persistence::FanOutItemUpdate,
    ) -> Result<(), crate::engine_error::EngineError> {
        self.inner.update_fan_out_item(id, u)
    }
    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        f: Option<crate::traits::persistence::FanOutItemStatus>,
    ) -> Result<Vec<crate::types::FanOutItemRow>, crate::engine_error::EngineError> {
        self.inner.get_fan_out_items(step_run_id, f)
    }
    fn get_gate_approval(
        &self,
        step_id: &str,
    ) -> Result<crate::traits::persistence::GateApprovalState, crate::engine_error::EngineError>
    {
        self.inner.get_gate_approval(step_id)
    }
    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), crate::engine_error::EngineError> {
        self.inner
            .approve_gate(step_id, approved_by, feedback, selections)
    }
    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), crate::engine_error::EngineError> {
        self.inner.reject_gate(step_id, rejected_by, feedback)
    }
    fn persist_metrics(
        &self,
        run_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
        cost_usd: f64,
        num_turns: i64,
        duration_ms: i64,
    ) -> Result<(), crate::engine_error::EngineError> {
        self.inner.persist_metrics(
            run_id,
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cost_usd,
            num_turns,
            duration_ms,
        )
    }
}
