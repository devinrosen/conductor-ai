//! Agent orchestration patterns: trigger dispatch, dependency DAGs, plan-then-swarm,
//! builder-validator quality gates, few-shot example dispatch, and human checkpoint
//! characterization.
//!
//! Part of: behavioral-trigger-dispatch@1.2.0,
//! dependency-aware-parallel-agent-spawning@1.0.0,
//! plan-then-swarm-execution@1.0.0, builder-validator-quality-gate@1.0.0,
//! few-shot-example-dispatch-blocks@1.0.0, human-checkpoint-protocol@1.0.0,
//! human-escalation-artifact@1.0.0

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// ─── Trigger Dispatch ───────────────────────────────────────────────────────
// Part of: behavioral-trigger-dispatch@1.2.0

/// Event type that can trigger agent dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum TriggerEvent {
    /// A commit was pushed to a branch.
    CommitPush,
    /// A PR was opened.
    PrOpened,
    /// A PR review was requested.
    ReviewRequested,
    /// A workflow step completed.
    StepCompleted,
    /// A blocker was raised.
    BlockerRaised,
    /// A human checkpoint was reached.
    CheckpointReached,
    /// Custom event with arbitrary type name.
    Custom(String),
}

/// Action to take when a trigger fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TriggerAction {
    /// Agent template name to dispatch.
    pub agent_template: String,
    /// Priority (lower = higher priority).
    pub priority: u8,
    /// Optional condition expression (evaluated externally).
    pub condition: Option<String>,
}

/// Maps trigger events to agent actions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TriggerDispatch {
    rules: Vec<TriggerRule>,
}

/// A single trigger-to-action mapping rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TriggerRule {
    pub event: TriggerEvent,
    pub actions: Vec<TriggerAction>,
    /// Whether the rule is active.
    pub enabled: bool,
}

#[allow(dead_code)]
impl TriggerDispatch {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Register a new trigger rule.
    pub fn add_rule(&mut self, event: TriggerEvent, actions: Vec<TriggerAction>, enabled: bool) {
        self.rules.push(TriggerRule {
            event,
            actions,
            enabled,
        });
    }

    /// Get all actions matching the given event, sorted by priority.
    pub fn dispatch(&self, event: &TriggerEvent) -> Vec<&TriggerAction> {
        let mut actions: Vec<&TriggerAction> = self
            .rules
            .iter()
            .filter(|r| r.enabled && &r.event == event)
            .flat_map(|r| r.actions.iter())
            .collect();
        actions.sort_by_key(|a| a.priority);
        actions
    }

    /// Number of registered rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

// ─── Dependency Graph (DAG) ─────────────────────────────────────────────────
// Part of: dependency-aware-parallel-agent-spawning@1.0.0

/// A node in the task dependency DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TaskNode {
    /// Unique task identifier.
    pub id: String,
    /// Agent template to use for this task.
    pub agent_template: String,
    /// IDs of tasks this task depends on.
    pub depends_on: Vec<String>,
    /// Task-specific context / prompt.
    pub context: String,
}

/// DAG of agent tasks with dependency tracking and topological sort.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct DependencyGraph {
    nodes: Vec<TaskNode>,
}

#[allow(dead_code)]
impl DependencyGraph {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Add a task node to the graph.
    pub fn add_node(&mut self, node: TaskNode) {
        self.nodes.push(node);
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Perform topological sort using Kahn's algorithm.
    /// Returns batches of task IDs that can be executed in parallel.
    /// Each batch contains tasks whose dependencies are all in previous batches.
    /// Returns `Err` if a cycle is detected.
    pub fn topological_batches(&self) -> std::result::Result<Vec<Vec<String>>, String> {
        let node_ids: HashSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();

        // Build adjacency and in-degree maps
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        for node in &self.nodes {
            in_degree.entry(node.id.as_str()).or_insert(0);
            for dep in &node.depends_on {
                if !node_ids.contains(dep.as_str()) {
                    return Err(format!(
                        "task '{}' depends on unknown task '{}'",
                        node.id, dep
                    ));
                }
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(node.id.as_str());
                *in_degree.entry(node.id.as_str()).or_insert(0) += 1;
            }
        }

        let mut batches: Vec<Vec<String>> = Vec::new();
        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut processed = 0;

        while !queue.is_empty() {
            let mut batch: Vec<String> = Vec::new();
            let mut next_queue: VecDeque<&str> = VecDeque::new();

            while let Some(id) = queue.pop_front() {
                batch.push(id.to_string());
                processed += 1;

                if let Some(deps) = dependents.get(id) {
                    for &dep_id in deps {
                        let deg = in_degree.get_mut(dep_id).unwrap();
                        *deg -= 1;
                        if *deg == 0 {
                            next_queue.push_back(dep_id);
                        }
                    }
                }
            }

            batch.sort(); // Deterministic ordering within batch
            batches.push(batch);
            queue = next_queue;
        }

        if processed != self.nodes.len() {
            return Err("cycle detected in dependency graph".to_string());
        }

        Ok(batches)
    }

    /// Get tasks that have no dependencies (can start immediately).
    pub fn root_tasks(&self) -> Vec<&TaskNode> {
        self.nodes
            .iter()
            .filter(|n| n.depends_on.is_empty())
            .collect()
    }
}

// ─── Plan-Then-Swarm ────────────────────────────────────────────────────────
// Part of: plan-then-swarm-execution@1.0.0

/// Phase of a plan-then-swarm execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SwarmPhase {
    /// Planning phase: a single agent decomposes the task.
    Planning,
    /// Execution phase: a swarm of workers executes the plan in parallel.
    Executing,
    /// All workers have completed.
    Completed,
    /// Planning or execution failed.
    Failed,
}

/// A work item produced by the planning phase for swarm execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct SwarmWorkItem {
    pub id: String,
    pub description: String,
    pub agent_template: String,
    pub context: String,
    pub depends_on: Vec<String>,
    pub status: SwarmWorkItemStatus,
}

/// Status of a single swarm work item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SwarmWorkItemStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
}

/// Two-phase plan-then-swarm execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PlanThenSwarm {
    /// Agent template used for the planning phase.
    pub planner_template: String,
    /// Current phase.
    pub phase: SwarmPhase,
    /// Work items produced by the planning phase.
    pub work_items: Vec<SwarmWorkItem>,
    /// Maximum number of concurrent workers.
    pub max_concurrency: usize,
}

#[allow(dead_code)]
impl PlanThenSwarm {
    pub fn new(planner_template: impl Into<String>, max_concurrency: usize) -> Self {
        Self {
            planner_template: planner_template.into(),
            phase: SwarmPhase::Planning,
            work_items: Vec::new(),
            max_concurrency: max_concurrency.max(1),
        }
    }

    /// Set work items (transition from planning to executing).
    pub fn set_work_items(&mut self, items: Vec<SwarmWorkItem>) {
        self.work_items = items;
        self.phase = SwarmPhase::Executing;
    }

    /// Get work items that are ready to execute (dependencies met, status pending).
    pub fn ready_items(&self) -> Vec<&SwarmWorkItem> {
        let completed: HashSet<&str> = self
            .work_items
            .iter()
            .filter(|w| w.status == SwarmWorkItemStatus::Completed)
            .map(|w| w.id.as_str())
            .collect();

        self.work_items
            .iter()
            .filter(|w| {
                w.status == SwarmWorkItemStatus::Pending
                    && w.depends_on.iter().all(|d| completed.contains(d.as_str()))
            })
            .collect()
    }

    /// Count of currently running work items.
    pub fn running_count(&self) -> usize {
        self.work_items
            .iter()
            .filter(|w| w.status == SwarmWorkItemStatus::Running)
            .count()
    }

    /// Whether all work items are completed or failed.
    pub fn is_complete(&self) -> bool {
        self.work_items.iter().all(|w| {
            w.status == SwarmWorkItemStatus::Completed || w.status == SwarmWorkItemStatus::Failed
        })
    }
}

// ─── Builder-Validator Quality Gate ─────────────────────────────────────────
// Part of: builder-validator-quality-gate@1.0.0

/// Phase in the builder-validator cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum BuilderValidatorPhase {
    /// Building phase: agent generates output.
    Building,
    /// Validation phase: validator checks output.
    Validating,
    /// Output passed validation.
    Accepted,
    /// Output failed validation, needs rebuild.
    Rejected,
}

/// A validation finding from the validator phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ValidationFinding {
    pub severity: ValidationSeverity,
    pub message: String,
    /// File or section reference where the issue was found.
    pub location: Option<String>,
}

/// Severity of a validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ValidationSeverity {
    Error,
    Warning,
    Info,
}

/// Result of a validation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ValidationResult {
    pub passed: bool,
    pub findings: Vec<ValidationFinding>,
    /// Number of build-validate iterations so far.
    pub iteration: u32,
}

/// Builder-validator quality gate that splits agent output into build/validate phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct BuilderValidator {
    /// Agent template for the builder phase.
    pub builder_template: String,
    /// Agent template for the validator phase.
    pub validator_template: String,
    /// Maximum number of build-validate iterations before failing.
    pub max_iterations: u32,
    /// Current phase.
    pub phase: BuilderValidatorPhase,
    /// History of validation results.
    pub validation_history: Vec<ValidationResult>,
}

#[allow(dead_code)]
impl BuilderValidator {
    pub fn new(
        builder_template: impl Into<String>,
        validator_template: impl Into<String>,
        max_iterations: u32,
    ) -> Self {
        Self {
            builder_template: builder_template.into(),
            validator_template: validator_template.into(),
            max_iterations: max_iterations.max(1),
            phase: BuilderValidatorPhase::Building,
            validation_history: Vec::new(),
        }
    }

    /// Record a validation result and advance the phase.
    pub fn record_validation(&mut self, result: ValidationResult) {
        let passed = result.passed;
        self.validation_history.push(result);

        if passed {
            self.phase = BuilderValidatorPhase::Accepted;
        } else if self.validation_history.len() as u32 >= self.max_iterations {
            // Exhausted iterations — stay rejected
            self.phase = BuilderValidatorPhase::Rejected;
        } else {
            // Send back for another build iteration
            self.phase = BuilderValidatorPhase::Building;
        }
    }

    /// Current iteration number (1-based).
    pub fn current_iteration(&self) -> u32 {
        self.validation_history.len() as u32 + 1
    }

    /// Whether the gate has reached a terminal state (accepted or rejected after max iterations).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.phase,
            BuilderValidatorPhase::Accepted | BuilderValidatorPhase::Rejected
        )
    }
}

// ─── Few-Shot Example Dispatch ──────────────────────────────────────────────
// Part of: few-shot-example-dispatch-blocks@1.0.0

/// A few-shot example for inclusion in agent prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct FewShotExample {
    /// Unique identifier.
    pub id: String,
    /// Tags for matching (e.g. ["rust", "refactoring", "error_handling"]).
    pub tags: Vec<String>,
    /// The input portion of the example.
    pub input: String,
    /// The expected output portion of the example.
    pub output: String,
    /// Quality score for ranking (higher = better example).
    pub quality_score: f64,
}

/// Selects contextually relevant few-shot examples for agent prompts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct FewShotDispatch {
    examples: Vec<FewShotExample>,
}

#[allow(dead_code)]
impl FewShotDispatch {
    pub fn new() -> Self {
        Self {
            examples: Vec::new(),
        }
    }

    /// Register an example.
    pub fn add_example(&mut self, example: FewShotExample) {
        self.examples.push(example);
    }

    /// Select up to `max_count` examples matching any of the given tags,
    /// sorted by quality score descending.
    pub fn select(&self, tags: &[&str], max_count: usize) -> Vec<&FewShotExample> {
        let tag_set: HashSet<&str> = tags.iter().copied().collect();
        let mut matching: Vec<&FewShotExample> = self
            .examples
            .iter()
            .filter(|ex| ex.tags.iter().any(|t| tag_set.contains(t.as_str())))
            .collect();
        matching.sort_by(|a, b| {
            b.quality_score
                .partial_cmp(&a.quality_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matching.truncate(max_count);
        matching
    }

    /// Format selected examples into a prompt block.
    pub fn format_prompt_block(&self, tags: &[&str], max_count: usize) -> String {
        let examples = self.select(tags, max_count);
        if examples.is_empty() {
            return String::new();
        }
        let mut block = String::from("## Examples\n\n");
        for (i, ex) in examples.iter().enumerate() {
            block.push_str(&format!("### Example {}\n", i + 1));
            block.push_str(&format!("**Input:**\n{}\n\n", ex.input));
            block.push_str(&format!("**Output:**\n{}\n\n", ex.output));
        }
        block
    }

    /// Total number of registered examples.
    pub fn example_count(&self) -> usize {
        self.examples.len()
    }
}

// ─── Human Checkpoint Protocol (characterization) ───────────────────────────
// Part of: human-checkpoint-protocol@1.0.0
//
// This pattern already exists in conductor via GateNode (workflow_dsl/types.rs)
// with gate_type: HumanApproval / HumanReview, and via the FeedbackRequest system
// (agent/types.rs). These characterization types document the pattern's presence.

/// Characterization: a human checkpoint in an agent workflow.
/// The actual implementation lives in `GateNode` with `GateType::HumanApproval`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct HumanCheckpoint {
    /// Reference to the gate node name.
    pub gate_name: String,
    /// What is being checked (description for the human).
    pub checkpoint_description: String,
    /// Timeout in seconds before the on_timeout action fires.
    pub timeout_secs: u64,
    /// Whether approval was granted.
    pub approved: Option<bool>,
}

// ─── Human Escalation Artifact (characterization) ───────────────────────────
// Part of: human-escalation-artifact@1.0.0
//
// This pattern already exists via the feedback system (FeedbackRequest in
// agent/types.rs) and blocker escalation (agent_comm.rs). These characterization
// types document the pattern's presence.

/// Characterization: a human escalation with supporting artifacts.
/// The actual implementation lives in `FeedbackRequest` + `AgentBlocker`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct HumanEscalation {
    /// The feedback request ID (if escalated via feedback system).
    pub feedback_request_id: Option<String>,
    /// The blocker ID (if escalated via blocker system).
    pub blocker_id: Option<String>,
    /// Escalation reason / summary.
    pub reason: String,
    /// Severity of the escalation.
    pub severity: EscalationSeverity,
    /// Artifacts attached to the escalation (references to agent_artifacts).
    pub artifact_ids: Vec<String>,
}

/// Severity of a human escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum EscalationSeverity {
    /// Low: informational, can wait.
    Low,
    /// Medium: needs attention soon.
    Medium,
    /// High: blocking, needs immediate attention.
    High,
    /// Critical: system-level failure, requires intervention.
    Critical,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── TriggerDispatch tests ──────────────────────────────────────────

    #[test]
    fn trigger_dispatch_basic() {
        let mut dispatch = TriggerDispatch::new();
        dispatch.add_rule(
            TriggerEvent::PrOpened,
            vec![TriggerAction {
                agent_template: "reviewer".to_string(),
                priority: 1,
                condition: None,
            }],
            true,
        );

        let actions = dispatch.dispatch(&TriggerEvent::PrOpened);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].agent_template, "reviewer");
    }

    #[test]
    fn trigger_dispatch_disabled_rule() {
        let mut dispatch = TriggerDispatch::new();
        dispatch.add_rule(
            TriggerEvent::CommitPush,
            vec![TriggerAction {
                agent_template: "linter".to_string(),
                priority: 0,
                condition: None,
            }],
            false, // disabled
        );

        let actions = dispatch.dispatch(&TriggerEvent::CommitPush);
        assert!(actions.is_empty());
    }

    #[test]
    fn trigger_dispatch_priority_ordering() {
        let mut dispatch = TriggerDispatch::new();
        dispatch.add_rule(
            TriggerEvent::PrOpened,
            vec![
                TriggerAction {
                    agent_template: "low-priority".to_string(),
                    priority: 10,
                    condition: None,
                },
                TriggerAction {
                    agent_template: "high-priority".to_string(),
                    priority: 1,
                    condition: None,
                },
            ],
            true,
        );

        let actions = dispatch.dispatch(&TriggerEvent::PrOpened);
        assert_eq!(actions[0].agent_template, "high-priority");
        assert_eq!(actions[1].agent_template, "low-priority");
    }

    #[test]
    fn trigger_dispatch_no_match() {
        let dispatch = TriggerDispatch::new();
        let actions = dispatch.dispatch(&TriggerEvent::BlockerRaised);
        assert!(actions.is_empty());
    }

    // ─── DependencyGraph tests ──────────────────────────────────────────

    #[test]
    fn dag_topological_sort_linear() {
        let mut graph = DependencyGraph::new();
        graph.add_node(TaskNode {
            id: "a".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec![],
            context: "first".to_string(),
        });
        graph.add_node(TaskNode {
            id: "b".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["a".to_string()],
            context: "second".to_string(),
        });
        graph.add_node(TaskNode {
            id: "c".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["b".to_string()],
            context: "third".to_string(),
        });

        let batches = graph.topological_batches().unwrap();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], vec!["a"]);
        assert_eq!(batches[1], vec!["b"]);
        assert_eq!(batches[2], vec!["c"]);
    }

    #[test]
    fn dag_topological_sort_parallel() {
        let mut graph = DependencyGraph::new();
        graph.add_node(TaskNode {
            id: "root".to_string(),
            agent_template: "planner".to_string(),
            depends_on: vec![],
            context: "root".to_string(),
        });
        graph.add_node(TaskNode {
            id: "w1".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["root".to_string()],
            context: "worker 1".to_string(),
        });
        graph.add_node(TaskNode {
            id: "w2".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["root".to_string()],
            context: "worker 2".to_string(),
        });
        graph.add_node(TaskNode {
            id: "final".to_string(),
            agent_template: "aggregator".to_string(),
            depends_on: vec!["w1".to_string(), "w2".to_string()],
            context: "aggregate".to_string(),
        });

        let batches = graph.topological_batches().unwrap();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], vec!["root"]);
        assert_eq!(batches[1], vec!["w1", "w2"]); // parallel
        assert_eq!(batches[2], vec!["final"]);
    }

    #[test]
    fn dag_cycle_detection() {
        let mut graph = DependencyGraph::new();
        graph.add_node(TaskNode {
            id: "a".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["b".to_string()],
            context: "".to_string(),
        });
        graph.add_node(TaskNode {
            id: "b".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["a".to_string()],
            context: "".to_string(),
        });

        let err = graph.topological_batches().unwrap_err();
        assert!(err.contains("cycle"));
    }

    #[test]
    fn dag_unknown_dependency() {
        let mut graph = DependencyGraph::new();
        graph.add_node(TaskNode {
            id: "a".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["nonexistent".to_string()],
            context: "".to_string(),
        });

        let err = graph.topological_batches().unwrap_err();
        assert!(err.contains("unknown task"));
    }

    #[test]
    fn dag_root_tasks() {
        let mut graph = DependencyGraph::new();
        graph.add_node(TaskNode {
            id: "a".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec![],
            context: "".to_string(),
        });
        graph.add_node(TaskNode {
            id: "b".to_string(),
            agent_template: "worker".to_string(),
            depends_on: vec!["a".to_string()],
            context: "".to_string(),
        });

        let roots = graph.root_tasks();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "a");
    }

    // ─── PlanThenSwarm tests ────────────────────────────────────────────

    #[test]
    fn plan_then_swarm_lifecycle() {
        let mut swarm = PlanThenSwarm::new("planner-agent", 4);
        assert_eq!(swarm.phase, SwarmPhase::Planning);

        let items = vec![
            SwarmWorkItem {
                id: "w1".to_string(),
                description: "Task 1".to_string(),
                agent_template: "worker".to_string(),
                context: "".to_string(),
                depends_on: vec![],
                status: SwarmWorkItemStatus::Pending,
            },
            SwarmWorkItem {
                id: "w2".to_string(),
                description: "Task 2".to_string(),
                agent_template: "worker".to_string(),
                context: "".to_string(),
                depends_on: vec!["w1".to_string()],
                status: SwarmWorkItemStatus::Pending,
            },
        ];
        swarm.set_work_items(items);
        assert_eq!(swarm.phase, SwarmPhase::Executing);

        // Only w1 is ready (w2 depends on w1)
        let ready = swarm.ready_items();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "w1");

        assert!(!swarm.is_complete());
    }

    #[test]
    fn plan_then_swarm_ready_after_completion() {
        let mut swarm = PlanThenSwarm::new("planner", 2);
        swarm.set_work_items(vec![
            SwarmWorkItem {
                id: "a".to_string(),
                description: "".to_string(),
                agent_template: "worker".to_string(),
                context: "".to_string(),
                depends_on: vec![],
                status: SwarmWorkItemStatus::Completed,
            },
            SwarmWorkItem {
                id: "b".to_string(),
                description: "".to_string(),
                agent_template: "worker".to_string(),
                context: "".to_string(),
                depends_on: vec!["a".to_string()],
                status: SwarmWorkItemStatus::Pending,
            },
        ]);

        let ready = swarm.ready_items();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "b");
    }

    // ─── BuilderValidator tests ─────────────────────────────────────────

    #[test]
    fn builder_validator_accept_on_first_try() {
        let mut bv = BuilderValidator::new("builder", "validator", 3);
        assert_eq!(bv.phase, BuilderValidatorPhase::Building);
        assert_eq!(bv.current_iteration(), 1);

        bv.record_validation(ValidationResult {
            passed: true,
            findings: vec![],
            iteration: 1,
        });

        assert_eq!(bv.phase, BuilderValidatorPhase::Accepted);
        assert!(bv.is_terminal());
    }

    #[test]
    fn builder_validator_reject_and_retry() {
        let mut bv = BuilderValidator::new("builder", "validator", 2);

        // First iteration fails
        bv.record_validation(ValidationResult {
            passed: false,
            findings: vec![ValidationFinding {
                severity: ValidationSeverity::Error,
                message: "Missing error handling".to_string(),
                location: Some("src/main.rs:42".to_string()),
            }],
            iteration: 1,
        });
        assert_eq!(bv.phase, BuilderValidatorPhase::Building); // sent back for rebuild

        // Second iteration also fails — max iterations reached
        bv.record_validation(ValidationResult {
            passed: false,
            findings: vec![ValidationFinding {
                severity: ValidationSeverity::Error,
                message: "Still broken".to_string(),
                location: None,
            }],
            iteration: 2,
        });
        assert_eq!(bv.phase, BuilderValidatorPhase::Rejected);
        assert!(bv.is_terminal());
    }

    #[test]
    fn builder_validator_accept_on_retry() {
        let mut bv = BuilderValidator::new("builder", "validator", 3);

        bv.record_validation(ValidationResult {
            passed: false,
            findings: vec![],
            iteration: 1,
        });
        assert_eq!(bv.phase, BuilderValidatorPhase::Building);

        bv.record_validation(ValidationResult {
            passed: true,
            findings: vec![],
            iteration: 2,
        });
        assert_eq!(bv.phase, BuilderValidatorPhase::Accepted);
    }

    // ─── FewShotDispatch tests ──────────────────────────────────────────

    #[test]
    fn few_shot_select_by_tags() {
        let mut dispatch = FewShotDispatch::new();
        dispatch.add_example(FewShotExample {
            id: "ex1".to_string(),
            tags: vec!["rust".to_string(), "refactoring".to_string()],
            input: "Refactor this function".to_string(),
            output: "Here is the refactored version".to_string(),
            quality_score: 0.9,
        });
        dispatch.add_example(FewShotExample {
            id: "ex2".to_string(),
            tags: vec!["python".to_string()],
            input: "Write a script".to_string(),
            output: "Here is the script".to_string(),
            quality_score: 0.8,
        });
        dispatch.add_example(FewShotExample {
            id: "ex3".to_string(),
            tags: vec!["rust".to_string(), "error_handling".to_string()],
            input: "Add error handling".to_string(),
            output: "Added Result types".to_string(),
            quality_score: 0.95,
        });

        let selected = dispatch.select(&["rust"], 2);
        assert_eq!(selected.len(), 2);
        // Sorted by quality_score descending
        assert_eq!(selected[0].id, "ex3");
        assert_eq!(selected[1].id, "ex1");
    }

    #[test]
    fn few_shot_no_matching_tags() {
        let mut dispatch = FewShotDispatch::new();
        dispatch.add_example(FewShotExample {
            id: "ex1".to_string(),
            tags: vec!["python".to_string()],
            input: "".to_string(),
            output: "".to_string(),
            quality_score: 1.0,
        });

        let selected = dispatch.select(&["rust"], 5);
        assert!(selected.is_empty());
    }

    #[test]
    fn few_shot_format_prompt_block() {
        let mut dispatch = FewShotDispatch::new();
        dispatch.add_example(FewShotExample {
            id: "ex1".to_string(),
            tags: vec!["rust".to_string()],
            input: "Input text".to_string(),
            output: "Output text".to_string(),
            quality_score: 1.0,
        });

        let block = dispatch.format_prompt_block(&["rust"], 1);
        assert!(block.contains("## Examples"));
        assert!(block.contains("Input text"));
        assert!(block.contains("Output text"));
    }

    #[test]
    fn few_shot_empty_format() {
        let dispatch = FewShotDispatch::new();
        let block = dispatch.format_prompt_block(&["rust"], 5);
        assert!(block.is_empty());
    }

    // ─── HumanCheckpoint characterization tests ─────────────────────────

    #[test]
    fn human_checkpoint_characterization() {
        // Characterization test: the HumanCheckpoint type documents that
        // human-checkpoint-protocol@1.0.0 is implemented via GateNode.
        let checkpoint = HumanCheckpoint {
            gate_name: "review-gate".to_string(),
            checkpoint_description: "Review the implementation before merge".to_string(),
            timeout_secs: 3600,
            approved: None,
        };
        assert_eq!(checkpoint.gate_name, "review-gate");
        assert!(checkpoint.approved.is_none());
    }

    #[test]
    fn human_checkpoint_approved() {
        let checkpoint = HumanCheckpoint {
            gate_name: "deploy-gate".to_string(),
            checkpoint_description: "Approve deployment".to_string(),
            timeout_secs: 600,
            approved: Some(true),
        };
        assert_eq!(checkpoint.approved, Some(true));
    }

    // ─── HumanEscalation characterization tests ─────────────────────────

    #[test]
    fn human_escalation_via_feedback() {
        // Characterization test: the HumanEscalation type documents that
        // human-escalation-artifact@1.0.0 is implemented via FeedbackRequest + AgentBlocker.
        let escalation = HumanEscalation {
            feedback_request_id: Some("fr-123".to_string()),
            blocker_id: None,
            reason: "Agent unable to resolve merge conflict".to_string(),
            severity: EscalationSeverity::High,
            artifact_ids: vec!["art-1".to_string(), "art-2".to_string()],
        };
        assert_eq!(escalation.severity, EscalationSeverity::High);
        assert_eq!(escalation.artifact_ids.len(), 2);
    }

    #[test]
    fn human_escalation_via_blocker() {
        let escalation = HumanEscalation {
            feedback_request_id: None,
            blocker_id: Some("blk-456".to_string()),
            reason: "Build infrastructure down".to_string(),
            severity: EscalationSeverity::Critical,
            artifact_ids: vec![],
        };
        assert_eq!(escalation.severity, EscalationSeverity::Critical);
        assert!(escalation.feedback_request_id.is_none());
    }
}
