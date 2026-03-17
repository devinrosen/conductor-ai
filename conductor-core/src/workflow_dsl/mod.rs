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

mod types;
mod lexer;
mod parser;
mod api;
pub mod validation;

// Re-export everything that is currently public
pub use types::{
    WorkflowDef, WorkflowWarning, WorkflowTrigger, InputType, InputDecl,
    WorkflowNode, CallNode, CallWorkflowNode, IfNode, UnlessNode, WhileNode,
    DoWhileNode, DoNode, ParallelNode, GateNode, AlwaysNode, ScriptNode,
    AgentRef, Condition, GateType, ApprovalMode, OnTimeout, OnMaxIter,
    collect_agent_names, collect_workflow_refs,
};
// Tree-walking helpers used in tests and available for external callers
#[allow(unused_imports)]
pub use types::{collect_snippet_refs, collect_schema_refs, collect_bot_names, count_nodes};
pub use parser::parse_workflow_str;
#[allow(unused_imports)]
pub use parser::parse_workflow_file;
pub(crate) use parser::parse_duration_str;
pub use api::{
    load_workflow_defs, load_workflow_by_name, validate_workflow_name,
    detect_workflow_cycles, MAX_WORKFLOW_DEPTH,
};
pub use validation::{
    ValidationError, ValidationReport,
    validate_workflow_semantics, validate_script_steps,
};

#[cfg(test)]
mod tests;
