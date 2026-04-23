use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::dsl::{detect_workflow_cycles, GateType, ValidationError, WorkflowDef, WorkflowNode};
use crate::engine::{run_workflow_engine, ExecutionState};
use crate::engine_error::EngineError;
use crate::traits::action_executor::{ActionExecutor, ActionRegistry};
use crate::traits::gate_resolver::{GateResolver, GateResolverRegistry};
use crate::traits::item_provider::{ItemProvider, ItemProviderRegistry};
use crate::traits::script_env_provider::{NoOpScriptEnvProvider, ScriptEnvProvider};
use crate::types::WorkflowResult;

/// Closure type for loading a sub-workflow by name — placeholder until #2349.
type WorkflowLoaderFn = dyn Fn(&str) -> std::result::Result<WorkflowDef, String> + Send + Sync;

// ---------------------------------------------------------------------------
// EngineBundle (kept for source compatibility)
// ---------------------------------------------------------------------------

/// Produced by earlier versions of `FlowEngineBuilder::build()`.
///
/// Kept so that importers of `runkon_flow::EngineBundle` continue to compile.
/// New code should use `FlowEngine` instead.
pub struct EngineBundle {
    pub action_registry: ActionRegistry,
    pub script_env_provider: Arc<dyn ScriptEnvProvider>,
}

// ---------------------------------------------------------------------------
// FlowEngine
// ---------------------------------------------------------------------------

/// The primary harness for running and validating workflows.
///
/// Produced by [`FlowEngineBuilder::build()`].
pub struct FlowEngine {
    pub(crate) action_registry: ActionRegistry,
    pub(crate) item_provider_registry: ItemProviderRegistry,
    pub(crate) gate_resolver_registry: GateResolverRegistry,
    /// Held for future use when FlowEngine constructs ExecutionState directly.
    #[allow(dead_code)]
    pub(crate) script_env_provider: Arc<dyn ScriptEnvProvider>,
    // TODO(#2349): replace with WorkflowResolver trait
    pub(crate) workflow_loader: Option<Arc<WorkflowLoaderFn>>,
}

impl FlowEngine {
    /// Validate a workflow definition against the registered executors, providers,
    /// and gate resolvers.
    ///
    /// Collects all errors before returning. Returns `Ok(())` when valid, or
    /// `Err(errors)` with one entry per problem found. Public so CI lint tools
    /// can call it without actually running the workflow.
    pub fn validate(&self, def: &WorkflowDef) -> Result<(), Vec<ValidationError>> {
        self.validate_with_registries(
            &self.action_registry,
            &self.item_provider_registry,
            &self.gate_resolver_registry,
            def,
        )
    }

    /// Run a workflow definition with a pre-built execution state.
    ///
    /// Validates against the FlowEngine's own registries before execution so
    /// that no side effects occur when the workflow is invalid. Uses the same
    /// registries as `validate()` to avoid asymmetry between the two paths.
    pub fn run(
        &self,
        def: &WorkflowDef,
        state: &mut ExecutionState,
    ) -> crate::engine_error::Result<WorkflowResult> {
        if let Err(validation_errors) = self.validate(def) {
            let joined = validation_errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(EngineError::Workflow(format!(
                "workflow '{}' failed validation:\n{}",
                def.name, joined
            )));
        }
        run_workflow_engine(state, def)
    }

    /// Inner validation implementation. Accepts explicit registry references so
    /// both `validate()` (uses builder registries) and `run()` (uses execution
    /// state registries) can call the same logic without risk of divergence.
    fn validate_with_registries(
        &self,
        action_registry: &ActionRegistry,
        item_provider_registry: &ItemProviderRegistry,
        gate_resolver_registry: &GateResolverRegistry,
        def: &WorkflowDef,
    ) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // Cycle / depth detection — only when a workflow loader is configured.
        // Without a loader we cannot traverse sub-workflows, so we degrade gracefully.
        if let Some(loader) = &self.workflow_loader {
            let loader_arc = Arc::clone(loader);
            let root_name = def.name.clone();
            let root_def_clone = def.clone();
            // Inject the root def so detect_workflow_cycles can resolve it by name.
            let cycle_loader = move |name: &str| -> std::result::Result<WorkflowDef, String> {
                if name == root_name.as_str() {
                    Ok(root_def_clone.clone())
                } else {
                    loader_arc(name)
                }
            };
            if let Err(cycle_msg) = detect_workflow_cycles(&def.name, &cycle_loader) {
                errors.push(ValidationError {
                    message: cycle_msg,
                    hint: None,
                });
            }
        }

        let mut visited: HashSet<String> = HashSet::new();
        validate_nodes_impl(
            action_registry,
            item_provider_registry,
            gate_resolver_registry,
            &self.workflow_loader,
            &def.body,
            &mut errors,
            &mut visited,
        );
        validate_nodes_impl(
            action_registry,
            item_provider_registry,
            gate_resolver_registry,
            &self.workflow_loader,
            &def.always,
            &mut errors,
            &mut visited,
        );

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn validate_nodes_impl(
    action_registry: &ActionRegistry,
    item_provider_registry: &ItemProviderRegistry,
    gate_resolver_registry: &GateResolverRegistry,
    workflow_loader: &Option<Arc<WorkflowLoaderFn>>,
    nodes: &[WorkflowNode],
    errors: &mut Vec<ValidationError>,
    visited: &mut HashSet<String>,
) {
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                let name = n.agent.label();
                if !action_registry.has_action(name) {
                    errors.push(ValidationError {
                        message: format!(
                            "call '{}': no registered ActionExecutor for '{}'",
                            n.agent.step_key(),
                            name
                        ),
                        hint: Some(format!(
                            "register an executor named '{}' or add a fallback executor",
                            name
                        )),
                    });
                }
            }
            WorkflowNode::Parallel(n) => {
                for agent_ref in &n.calls {
                    let name = agent_ref.label();
                    if !action_registry.has_action(name) {
                        errors.push(ValidationError {
                            message: format!(
                                "parallel call '{}': no registered ActionExecutor for '{}'",
                                agent_ref.step_key(),
                                name
                            ),
                            hint: Some(format!(
                                "register an executor named '{}' or add a fallback executor",
                                name
                            )),
                        });
                    }
                }
            }
            WorkflowNode::ForEach(n) => {
                if item_provider_registry.get(&n.over).is_none() {
                    errors.push(ValidationError {
                        message: format!(
                            "foreach '{}': no registered ItemProvider for '{}'",
                            n.name, n.over
                        ),
                        hint: Some(format!(
                            "register a provider with name '{}' via FlowEngineBuilder::item_provider()",
                            n.over
                        )),
                    });
                }
            }
            WorkflowNode::Gate(n) => {
                // QualityGate is evaluated inline and never goes through a GateResolver.
                if n.gate_type != GateType::QualityGate {
                    let type_str = n.gate_type.to_string();
                    if !gate_resolver_registry.has_type(&type_str) {
                        errors.push(ValidationError {
                            message: format!(
                                "gate '{}': no registered GateResolver for type '{}'",
                                n.name, type_str
                            ),
                            hint: Some(format!(
                                "register a resolver with gate_type() == '{}' via FlowEngineBuilder::gate_resolver()",
                                type_str
                            )),
                        });
                    }
                }
            }
            WorkflowNode::CallWorkflow(n) => {
                if !visited.contains(&n.workflow) {
                    visited.insert(n.workflow.clone());
                    if let Some(loader) = workflow_loader {
                        match loader(&n.workflow) {
                            Ok(sub_def) => {
                                let mut sub_errors = Vec::new();
                                validate_nodes_impl(
                                    action_registry,
                                    item_provider_registry,
                                    gate_resolver_registry,
                                    workflow_loader,
                                    &sub_def.body,
                                    &mut sub_errors,
                                    visited,
                                );
                                validate_nodes_impl(
                                    action_registry,
                                    item_provider_registry,
                                    gate_resolver_registry,
                                    workflow_loader,
                                    &sub_def.always,
                                    &mut sub_errors,
                                    visited,
                                );
                                for sub_err in sub_errors {
                                    errors.push(ValidationError {
                                        message: format!(
                                            "in sub-workflow '{}': {}",
                                            n.workflow, sub_err.message
                                        ),
                                        hint: sub_err.hint,
                                    });
                                }
                            }
                            Err(e) => {
                                // Report every load failure so all missing sub-workflows
                                // surface in one pass, not just the first one.
                                errors.push(ValidationError {
                                    message: format!(
                                        "call workflow '{}': sub-workflow could not be loaded: {}",
                                        n.workflow, e
                                    ),
                                    hint: None,
                                });
                            }
                        }
                    }
                }
            }
            WorkflowNode::If(n) => {
                validate_nodes_impl(
                    action_registry,
                    item_provider_registry,
                    gate_resolver_registry,
                    workflow_loader,
                    &n.body,
                    errors,
                    visited,
                );
            }
            WorkflowNode::Unless(n) => {
                validate_nodes_impl(
                    action_registry,
                    item_provider_registry,
                    gate_resolver_registry,
                    workflow_loader,
                    &n.body,
                    errors,
                    visited,
                );
            }
            WorkflowNode::While(n) => {
                validate_nodes_impl(
                    action_registry,
                    item_provider_registry,
                    gate_resolver_registry,
                    workflow_loader,
                    &n.body,
                    errors,
                    visited,
                );
            }
            WorkflowNode::DoWhile(n) => {
                validate_nodes_impl(
                    action_registry,
                    item_provider_registry,
                    gate_resolver_registry,
                    workflow_loader,
                    &n.body,
                    errors,
                    visited,
                );
            }
            WorkflowNode::Do(n) => {
                validate_nodes_impl(
                    action_registry,
                    item_provider_registry,
                    gate_resolver_registry,
                    workflow_loader,
                    &n.body,
                    errors,
                    visited,
                );
            }
            WorkflowNode::Always(n) => {
                validate_nodes_impl(
                    action_registry,
                    item_provider_registry,
                    gate_resolver_registry,
                    workflow_loader,
                    &n.body,
                    errors,
                    visited,
                );
            }
            WorkflowNode::Script(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// FlowEngineBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing a [`FlowEngine`].
///
/// Register action executors with `.action()` / `.action_fallback()`,
/// foreach item sources with `.item_provider()`, gate resolvers with
/// `.gate_resolver()`, and environment injection with `.script_env_provider()`.
pub struct FlowEngineBuilder {
    named: HashMap<String, Box<dyn ActionExecutor>>,
    fallback: Option<Box<dyn ActionExecutor>>,
    script_env_provider: Box<dyn ScriptEnvProvider>,
    item_providers: ItemProviderRegistry,
    gate_resolvers: GateResolverRegistry,
    workflow_loader: Option<Arc<WorkflowLoaderFn>>,
}

impl FlowEngineBuilder {
    pub fn new() -> Self {
        Self {
            named: HashMap::new(),
            fallback: None,
            script_env_provider: Box::new(NoOpScriptEnvProvider),
            item_providers: ItemProviderRegistry::new(),
            gate_resolvers: GateResolverRegistry::new(),
            workflow_loader: None,
        }
    }

    /// Register a named executor. The executor's `name()` is used as the lookup key.
    #[allow(dead_code)]
    pub fn action(mut self, executor: Box<dyn ActionExecutor>) -> Self {
        self.named.insert(executor.name().to_string(), executor);
        self
    }

    /// Register the fallback (catch-all) executor.
    ///
    /// Returns `Err` if called more than once — only one fallback is allowed.
    pub fn action_fallback(
        mut self,
        executor: Box<dyn ActionExecutor>,
    ) -> Result<Self, EngineError> {
        if self.fallback.is_some() {
            return Err(EngineError::Workflow(
                "action_fallback already set — only one fallback executor is allowed".to_string(),
            ));
        }
        self.fallback = Some(executor);
        Ok(self)
    }

    /// Register an item provider for foreach fan-outs.
    pub fn item_provider<P: ItemProvider + 'static>(mut self, provider: P) -> Self {
        self.item_providers.register(provider);
        self
    }

    /// Register a gate resolver for a specific gate type.
    pub fn gate_resolver<R: GateResolver + 'static>(mut self, resolver: R) -> Self {
        self.gate_resolvers.register(resolver);
        self
    }

    /// Set the script env provider. Defaults to `NoOpScriptEnvProvider`.
    pub fn script_env_provider(mut self, provider: Box<dyn ScriptEnvProvider>) -> Self {
        self.script_env_provider = provider;
        self
    }

    /// Set the workflow loader for sub-workflow validation and cycle detection.
    ///
    /// When present, `FlowEngine::validate()` uses it to load and recursively validate
    /// `call workflow` nodes. TODO(#2349): replace with `WorkflowResolver` trait.
    pub fn workflow_loader<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> std::result::Result<WorkflowDef, String> + Send + Sync + 'static,
    {
        self.workflow_loader = Some(Arc::new(f));
        self
    }

    /// Consume the builder and produce a [`FlowEngine`].
    pub fn build(self) -> Result<FlowEngine, EngineError> {
        Ok(FlowEngine {
            action_registry: ActionRegistry::new(self.named, self.fallback),
            item_provider_registry: self.item_providers,
            gate_resolver_registry: self.gate_resolvers,
            script_env_provider: Arc::from(self.script_env_provider),
            workflow_loader: self.workflow_loader,
        })
    }
}

impl Default for FlowEngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{
        AgentRef, ApprovalMode, CallNode, CallWorkflowNode, ForEachNode, GateNode, GateType,
        OnChildFail, OnCycle, OnTimeout, WorkflowTrigger,
    };
    use crate::engine_error::EngineError;
    use crate::test_helpers::{make_ectx, make_params};
    use crate::traits::action_executor::{ActionOutput, ActionParams, ExecutionContext};
    use crate::traits::gate_resolver::{GateContext, GateParams, GatePoll};
    use crate::traits::item_provider::{FanOutItem, ProviderContext};
    use crate::traits::run_context::RunContext;
    use std::collections::HashMap;

    // --- test executors / providers / resolvers ---

    struct AlphaExecutor;
    impl ActionExecutor for AlphaExecutor {
        fn name(&self) -> &str {
            "alpha"
        }
        fn execute(
            &self,
            _ectx: &ExecutionContext,
            _params: &ActionParams,
        ) -> Result<ActionOutput, EngineError> {
            Ok(ActionOutput {
                markers: vec!["alpha".to_string()],
                ..Default::default()
            })
        }
    }

    struct BetaExecutor;
    impl ActionExecutor for BetaExecutor {
        fn name(&self) -> &str {
            "beta"
        }
        fn execute(
            &self,
            _ectx: &ExecutionContext,
            _params: &ActionParams,
        ) -> Result<ActionOutput, EngineError> {
            Ok(ActionOutput {
                markers: vec!["beta".to_string()],
                ..Default::default()
            })
        }
    }

    struct TicketsProvider;
    impl crate::traits::item_provider::ItemProvider for TicketsProvider {
        fn name(&self) -> &str {
            "tickets"
        }
        fn items(
            &self,
            _ctx: &ProviderContext,
            _scope: Option<&crate::dsl::ForeachScope>,
            _filter: &HashMap<String, String>,
            _existing_set: &std::collections::HashSet<String>,
        ) -> Result<Vec<FanOutItem>, EngineError> {
            Ok(vec![])
        }
    }

    struct HumanApprovalResolver;
    impl crate::traits::gate_resolver::GateResolver for HumanApprovalResolver {
        fn gate_type(&self) -> &str {
            "human_approval"
        }
        fn poll(
            &self,
            _run_id: &str,
            _params: &GateParams,
            _ctx: &GateContext,
        ) -> Result<GatePoll, EngineError> {
            Ok(GatePoll::Approved(None))
        }
    }

    // --- helpers ---

    #[allow(dead_code)]
    fn make_def(name: &str, body: Vec<WorkflowNode>) -> WorkflowDef {
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

    fn call_node(agent: &str) -> WorkflowNode {
        WorkflowNode::Call(CallNode {
            agent: AgentRef::Name(agent.to_string()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec![],
            bot_name: None,
            plugin_dirs: vec![],
        })
    }

    fn foreach_node(step: &str, over: &str) -> WorkflowNode {
        WorkflowNode::ForEach(ForEachNode {
            name: step.to_string(),
            over: over.to_string(),
            scope: None,
            filter: HashMap::new(),
            ordered: false,
            on_cycle: OnCycle::Fail,
            max_parallel: 1,
            workflow: "child_wf".to_string(),
            inputs: HashMap::new(),
            on_child_fail: OnChildFail::Halt,
        })
    }

    fn gate_node(name: &str, gate_type: GateType) -> WorkflowNode {
        WorkflowNode::Gate(GateNode {
            name: name.to_string(),
            gate_type,
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

    // --- existing FlowEngineBuilder tests (now produce FlowEngine) ---

    #[test]
    fn build_with_named_executor() {
        let engine = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .build()
            .unwrap();
        let output = engine
            .action_registry
            .dispatch("alpha", &make_ectx(), &make_params("alpha"))
            .unwrap();
        assert_eq!(output.markers, vec!["alpha"]);
    }

    #[test]
    fn build_with_fallback() {
        let engine = FlowEngineBuilder::new()
            .action_fallback(Box::new(BetaExecutor))
            .unwrap()
            .build()
            .unwrap();
        let output = engine
            .action_registry
            .dispatch("anything", &make_ectx(), &make_params("anything"))
            .unwrap();
        assert_eq!(output.markers, vec!["beta"]);
    }

    #[test]
    fn named_takes_precedence_over_fallback() {
        let engine = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .action_fallback(Box::new(BetaExecutor))
            .unwrap()
            .build()
            .unwrap();
        let output = engine
            .action_registry
            .dispatch("alpha", &make_ectx(), &make_params("alpha"))
            .unwrap();
        assert_eq!(output.markers, vec!["alpha"]);
    }

    #[test]
    fn second_action_fallback_returns_err() {
        let result = FlowEngineBuilder::new()
            .action_fallback(Box::new(AlphaExecutor))
            .unwrap()
            .action_fallback(Box::new(BetaExecutor));
        assert!(result.is_err(), "second action_fallback should return Err");
    }

    #[test]
    fn custom_script_env_provider_is_stored_in_bundle() {
        struct FixedEnvProvider;
        impl ScriptEnvProvider for FixedEnvProvider {
            fn env(&self, _ctx: &dyn RunContext) -> HashMap<String, String> {
                let mut m = HashMap::new();
                m.insert("CUSTOM_VAR".to_string(), "42".to_string());
                m
            }
        }

        struct NoopCtx;
        impl RunContext for NoopCtx {
            fn injected_variables(&self) -> HashMap<&'static str, String> {
                HashMap::new()
            }
            fn working_dir(&self) -> &std::path::Path {
                std::path::Path::new("/tmp")
            }
            fn repo_path(&self) -> &std::path::Path {
                std::path::Path::new("/tmp")
            }
        }

        let engine = FlowEngineBuilder::new()
            .script_env_provider(Box::new(FixedEnvProvider))
            .build()
            .unwrap();

        let env = engine.script_env_provider.env(&NoopCtx);
        assert_eq!(env.get("CUSTOM_VAR").map(String::as_str), Some("42"));
    }

    // --- validate() acceptance criteria ---

    // AC1: missing action name produces error
    #[test]
    fn validate_missing_action_name_produces_error() {
        let def = make_def("wf", vec![call_node("missing_agent")]);
        let engine = FlowEngineBuilder::new().build().unwrap();

        let errors = engine.validate(&def).unwrap_err();
        assert!(
            !errors.is_empty(),
            "expected at least one error for missing action"
        );
        assert!(
            errors.iter().any(|e| e.message.contains("missing_agent")),
            "error should name the missing executor; got: {:?}",
            errors
        );
    }

    // AC2: missing item provider produces error
    #[test]
    fn validate_missing_item_provider_produces_error() {
        let def = make_def("wf", vec![foreach_node("items", "tickets")]);
        let engine = FlowEngineBuilder::new().build().unwrap();

        let errors = engine.validate(&def).unwrap_err();
        assert!(
            errors.iter().any(|e| e.message.contains("tickets")),
            "error should mention the missing provider name; got: {:?}",
            errors
        );
    }

    // AC3: missing gate type produces error
    #[test]
    fn validate_missing_gate_type_produces_error() {
        let def = make_def("wf", vec![gate_node("approval", GateType::HumanApproval)]);
        let engine = FlowEngineBuilder::new().build().unwrap();

        let errors = engine.validate(&def).unwrap_err();
        assert!(
            errors.iter().any(|e| e.message.contains("human_approval")),
            "error should mention the missing gate type; got: {:?}",
            errors
        );
    }

    // AC3b: QualityGate is excluded from resolver checks
    #[test]
    fn validate_quality_gate_does_not_require_resolver() {
        use crate::dsl::{GateNode, GateType, OnFailAction, OnTimeout, QualityGateConfig};
        let gate = WorkflowNode::Gate(GateNode {
            name: "qg".to_string(),
            gate_type: GateType::QualityGate,
            prompt: None,
            min_approvals: 1,
            approval_mode: ApprovalMode::default(),
            timeout_secs: 0,
            on_timeout: OnTimeout::Fail,
            bot_name: None,
            quality_gate: Some(QualityGateConfig {
                source: "step1".to_string(),
                threshold: 80,
                on_fail_action: OnFailAction::Fail,
            }),
            options: None,
        });
        // Also need call step1 to be produced, but validate() only checks harness
        // registrations, not semantic step ordering — so just test the gate alone.
        let def = make_def("wf", vec![gate]);
        let engine = FlowEngineBuilder::new().build().unwrap();
        // No gate resolver registered — but QualityGate must not trigger an error.
        let result = engine.validate(&def);
        // QualityGate check should not produce a gate-resolver error.
        // (There may be no errors at all if no actions are referenced.)
        if let Err(errors) = result {
            assert!(
                !errors.iter().any(|e| e.message.contains("quality_gate")),
                "QualityGate should not produce a resolver error; got: {:?}",
                errors
            );
        }
    }

    // AC4: valid workflow with all registrations passes
    #[test]
    fn validate_valid_workflow_passes() {
        let def = make_def(
            "wf",
            vec![
                call_node("alpha"),
                foreach_node("items", "tickets"),
                gate_node("approval", GateType::HumanApproval),
            ],
        );
        let engine = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .item_provider(TicketsProvider)
            .gate_resolver(HumanApprovalResolver)
            .build()
            .unwrap();

        assert!(
            engine.validate(&def).is_ok(),
            "all registrations present — validation should pass"
        );
    }

    // AC5: sub-workflow validation errors surface with path prefix
    #[test]
    fn validate_sub_workflow_errors_have_path_prefix() {
        let sub_def = make_def("sub_wf", vec![call_node("missing_in_sub")]);
        let engine = FlowEngineBuilder::new()
            .workflow_loader(move |name| {
                if name == "sub_wf" {
                    Ok(sub_def.clone())
                } else {
                    Err(format!("not found: {name}"))
                }
            })
            .build()
            .unwrap();

        let root_def = make_def(
            "root",
            vec![WorkflowNode::CallWorkflow(CallWorkflowNode {
                workflow: "sub_wf".to_string(),
                inputs: HashMap::new(),
                retries: 0,
                on_fail: None,
                bot_name: None,
            })],
        );

        let errors = engine.validate(&root_def).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("in sub-workflow") && e.message.contains("sub_wf")),
            "sub-workflow errors should be prefixed with the sub-workflow name; got: {:?}",
            errors
        );
        assert!(
            errors.iter().any(|e| e.message.contains("missing_in_sub")),
            "error should mention the missing executor from the sub-workflow; got: {:?}",
            errors
        );
    }

    // AC6: cycle detection triggers ValidationError
    #[test]
    fn validate_cycle_detection_triggers_error() {
        // A workflow that calls itself creates a cycle.
        let cycle_def = make_def(
            "cycle_wf",
            vec![WorkflowNode::CallWorkflow(CallWorkflowNode {
                workflow: "cycle_wf".to_string(),
                inputs: HashMap::new(),
                retries: 0,
                on_fail: None,
                bot_name: None,
            })],
        );
        let cycle_def_clone = cycle_def.clone();
        let engine = FlowEngineBuilder::new()
            .workflow_loader(move |name| {
                if name == "cycle_wf" {
                    Ok(cycle_def_clone.clone())
                } else {
                    Err(format!("not found: {name}"))
                }
            })
            .build()
            .unwrap();

        let errors = engine.validate(&cycle_def).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("Circular") || e.message.contains("cycle")),
            "cycle detection should produce an error; got: {:?}",
            errors
        );
    }

    // AC7: run() rejects invalid workflows before any side effects
    #[test]
    fn run_rejects_invalid_workflow_before_side_effects() {
        use crate::engine::{ExecutionState, WorktreeContext};
        use crate::persistence_memory::InMemoryWorkflowPersistence;
        use crate::traits::script_env_provider::NoOpScriptEnvProvider;
        use crate::types::WorkflowExecConfig;

        let def = make_def("wf", vec![call_node("unregistered_agent")]);
        let engine = FlowEngineBuilder::new().build().unwrap();

        let mut state = ExecutionState {
            persistence: Arc::new(InMemoryWorkflowPersistence::new()),
            action_registry: Arc::new(ActionRegistry::new(HashMap::new(), None)),
            script_env_provider: Arc::new(NoOpScriptEnvProvider),
            workflow_run_id: "test-run".to_string(),
            workflow_name: "wf".to_string(),
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
        };

        let result = engine.run(&def, &mut state);
        assert!(result.is_err(), "run() must reject an invalid workflow");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("validation"),
            "error should mention validation; got: {err}"
        );
        assert_eq!(
            state.position, 0,
            "no side effects: position must be unchanged when validation fails"
        );
        assert!(
            state.step_results.is_empty(),
            "no side effects: step_results must be empty when validation fails"
        );
    }
}
