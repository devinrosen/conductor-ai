#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use runkon_flow::cancellation::CancellationToken;
use runkon_flow::dsl::{
    AgentRef, ApprovalMode, CallNode, GateNode, GateType, OnTimeout, WorkflowDef, WorkflowNode,
    WorkflowTrigger,
};
use runkon_flow::engine::{ExecutionState, WorktreeContext};
use runkon_flow::engine_error::EngineError;
use runkon_flow::events::{EngineEventData, EventSink};
use runkon_flow::persistence_memory::InMemoryWorkflowPersistence;
use runkon_flow::traits::action_executor::{
    ActionExecutor, ActionOutput, ActionParams, ActionRegistry, ExecutionContext,
};
use runkon_flow::traits::persistence::{NewRun, WorkflowPersistence};
use runkon_flow::traits::script_env_provider::NoOpScriptEnvProvider;
use runkon_flow::types::WorkflowExecConfig;
use runkon_flow::ItemProviderRegistry;

// ---------------------------------------------------------------------------
// Mock executors
// ---------------------------------------------------------------------------

/// Returns configurable markers on every execution.
pub struct MockExecutor {
    pub label: String,
    pub markers: Vec<String>,
}

impl MockExecutor {
    pub fn new(name: &str) -> Self {
        Self {
            label: name.to_string(),
            markers: vec![],
        }
    }

    pub fn with_markers(name: &str, markers: &[&str]) -> Self {
        Self {
            label: name.to_string(),
            markers: markers.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl ActionExecutor for MockExecutor {
    fn name(&self) -> &str {
        &self.label
    }

    fn execute(
        &self,
        _ectx: &ExecutionContext,
        _params: &ActionParams,
    ) -> Result<ActionOutput, EngineError> {
        Ok(ActionOutput {
            markers: self.markers.clone(),
            ..Default::default()
        })
    }
}

/// Always returns an engine error — used to test failure propagation.
pub struct FailingExecutor;

impl ActionExecutor for FailingExecutor {
    fn name(&self) -> &str {
        "failing"
    }

    fn execute(
        &self,
        _ectx: &ExecutionContext,
        _params: &ActionParams,
    ) -> Result<ActionOutput, EngineError> {
        Err(EngineError::Workflow(
            "intentional test failure".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Event sink helpers
// ---------------------------------------------------------------------------

/// Collects all emitted events for post-run inspection.
pub struct VecSink {
    events: Mutex<Vec<EngineEventData>>,
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

/// Forwards events to a `VecSink` owned behind an `Arc`.
///
/// Used because `FlowEngineBuilder::event_sink` takes `Box<dyn EventSink>`,
/// while the test needs to keep an `Arc<VecSink>` to read the collected events
/// after `run()` completes.
pub struct ForwardSink(pub Arc<VecSink>);

impl EventSink for ForwardSink {
    fn emit(&self, event: &EngineEventData) {
        self.0.emit(event);
    }
}

// ---------------------------------------------------------------------------
// State construction helpers
// ---------------------------------------------------------------------------

pub fn make_persistence() -> Arc<InMemoryWorkflowPersistence> {
    Arc::new(InMemoryWorkflowPersistence::new())
}

/// Build an `ExecutionState` with a pre-created run record in `persistence`.
///
/// `named_executors` maps action name → executor; supply an empty `HashMap`
/// for workflows with no `call` steps.
pub fn make_state(
    wf_name: &str,
    persistence: Arc<InMemoryWorkflowPersistence>,
    named_executors: HashMap<String, Box<dyn ActionExecutor>>,
) -> ExecutionState {
    let run = persistence
        .create_run(NewRun {
            workflow_name: wf_name.to_string(),
            worktree_id: None,
            ticket_id: None,
            repo_id: None,
            parent_run_id: String::new(),
            dry_run: false,
            trigger: "test".to_string(),
            definition_snapshot: None,
            parent_workflow_run_id: None,
            target_label: None,
        })
        .expect("create_run failed");

    ExecutionState {
        persistence: Arc::clone(&persistence) as Arc<dyn WorkflowPersistence>,
        action_registry: Arc::new(ActionRegistry::new(named_executors, None)),
        script_env_provider: Arc::new(NoOpScriptEnvProvider),
        workflow_run_id: run.id,
        workflow_name: wf_name.to_string(),
        worktree_ctx: WorktreeContext {
            worktree_id: None,
            working_dir: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
            ticket_id: None,
            repo_id: None,
            conductor_bin_dir: None,
            extra_plugin_dirs: vec![],
        },
        model: None,
        exec_config: WorkflowExecConfig::default(),
        inputs: HashMap::new(),
        parent_run_id: String::new(),
        depth: 0,
        target_label: None,
        step_results: HashMap::new(),
        contexts: vec![],
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_input_tokens: 0,
        total_cache_creation_input_tokens: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: vec![],
        resume_ctx: None,
        default_bot_name: None,
        triggered_by_hook: false,
        schema_resolver: None,
        child_runner: None,
        last_heartbeat_at: ExecutionState::new_heartbeat(),
        registry: Arc::new(ItemProviderRegistry::new()),
        event_sinks: Arc::from(vec![]),
        cancellation: CancellationToken::new(),
        current_execution_id: Arc::new(Mutex::new(None)),
    }
}

// ---------------------------------------------------------------------------
// WorkflowDef construction helpers
// ---------------------------------------------------------------------------

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

pub fn make_def_with_always(
    name: &str,
    body: Vec<WorkflowNode>,
    always: Vec<WorkflowNode>,
) -> WorkflowDef {
    WorkflowDef {
        name: name.to_string(),
        title: None,
        description: String::new(),
        trigger: WorkflowTrigger::Manual,
        targets: vec![],
        group: None,
        inputs: vec![],
        body,
        always,
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

pub fn gate_node(name: &str) -> WorkflowNode {
    WorkflowNode::Gate(GateNode {
        name: name.to_string(),
        gate_type: GateType::HumanApproval,
        prompt: None,
        min_approvals: 1,
        approval_mode: ApprovalMode::default(),
        timeout_secs: 0,
        on_timeout: OnTimeout::Fail,
        bot_name: None,
        quality_gate: None,
        options: None,
    })
}
