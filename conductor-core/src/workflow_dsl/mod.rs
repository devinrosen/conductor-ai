//! Recursive descent parser for the `.wf` workflow DSL.
//!
//! Converts `.conductor/workflows/<name>.wf` files into a `WorkflowDef` with a
//! tree-structured body of `WorkflowNode`s.
//!
//! Grammar (informal):
//! ```text
//! workflow_file := "workflow" IDENT "{" meta? inputs? node* "}"
//! meta          := "meta" "{" kv* "}"
//! inputs        := "inputs" "{" input_decl* "}"
//! input_decl    := IDENT ("required" | "default" "=" STRING)
//! node          := call | call_workflow | if_node | while_node | do_while | do | parallel | gate | always
//! call          := "call" agent_ref ("{" kv* "}")?
//! call_workflow := "call" "workflow" IDENT ("{" inputs? kv* "}")?
//! if_node       := "if" condition "{" kv* node* "}"
//! while_node    := "while" condition "{" kv* node* "}"
//! do_while      := "do" "{" kv* node* "}" "while" condition
//! do            := "do" "{" kv* node* "}"
//! parallel      := "parallel" "{" kv* ("call" agent_ref ("{" kv* "}")?)*  "}"
//! gate          := "gate" IDENT "{" kv* "}"
//! always        := "always" "{" node* "}"
//! condition     := IDENT "." IDENT
//! kv            := IDENT "=" (STRING | NUMBER | IDENT)
//! agent_ref     := IDENT | STRING
//! ```
//!
//! `agent_ref` is a bare identifier (short name resolved via search order) or a
//! quoted string (explicit path relative to the repo root).

mod api;
mod lexer;
mod parser;
mod script_utils;
mod types;
mod validation;

// Re-export everything that is currently public
pub use types::{
    collect_agent_names, collect_workflow_refs, AgentRef, AlwaysNode, ApprovalMode, CallNode,
    CallWorkflowNode, Condition, DoNode, DoWhileNode, GateNode, GateOptions, GateType, IfNode,
    InputDecl, InputType, OnFailAction, OnMaxIter, OnTimeout, ParallelNode, ScriptNode,
    UnlessNode, WhileNode, WorkflowDef, WorkflowNode, WorkflowTrigger, WorkflowWarning,
};
// QualityGateConfig is a field type on the public GateNode struct and must be
// part of the public API so callers can name the type. It is only constructed
// in tests within this crate, hence the allow.
#[allow(unused_imports)]
pub use types::QualityGateConfig;
// Tree-walking helpers used in tests and available for external callers
pub use api::{
    detect_workflow_cycles, load_workflow_by_name, load_workflow_defs, validate_workflow_name,
    MAX_WORKFLOW_DEPTH,
};
pub(crate) use parser::parse_duration_str;
pub use parser::parse_workflow_str;
pub use script_utils::{default_skills_dir, make_script_resolver, resolve_script_path};
#[cfg(test)]
pub(crate) use types::{collect_bot_names, collect_schema_refs, collect_snippet_refs, count_nodes};
pub use validation::{
    validate_script_steps, validate_workflow_semantics, ValidationError, ValidationReport,
};

#[cfg(test)]
mod tests;
