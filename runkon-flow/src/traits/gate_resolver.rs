use crate::dsl::ApprovalMode;
use crate::engine_error::EngineError;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Outcome of a single poll tick from a `GateResolver`.
#[derive(Debug)]
pub enum GatePoll {
    Approved(Option<String>),
    Rejected(String),
    Pending,
}

/// All gate configuration passed to `GateResolver::poll`.
#[allow(dead_code)] // fields are available for resolver use; not all are consumed in Phase 1
pub struct GateParams {
    pub gate_name: String,
    pub prompt: Option<String>,
    pub min_approvals: u32,
    pub approval_mode: ApprovalMode,
    /// Resolved options list (StepRef already expanded by the dispatcher).
    pub options: Vec<String>,
    pub timeout_secs: u64,
    pub bot_name: Option<String>,
    pub step_id: String,
}

/// Transient context passed to each `GateResolver::poll` call.
#[allow(dead_code)]
pub struct GateContext {
    pub working_dir: String,
    pub default_bot_name: Option<String>,
}

// ---------------------------------------------------------------------------
// GateResolver trait
// ---------------------------------------------------------------------------

pub trait GateResolver: Send + Sync {
    fn gate_type(&self) -> &str;
    fn poll(
        &self,
        run_id: &str,
        params: &GateParams,
        ctx: &GateContext,
    ) -> Result<GatePoll, EngineError>;
}
