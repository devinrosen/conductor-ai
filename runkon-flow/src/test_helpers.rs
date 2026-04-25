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
