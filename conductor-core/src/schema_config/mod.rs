//! Schema-based structured output for workflow agents.
//!
//! Schemas live in `.conductor/schemas/<name>.yaml` and define the JSON shape
//! that an agent's `FLOW_OUTPUT` block must conform to. This module generates
//! the schema-specific output instructions for the agent prompt.
//!
//! Parsing and validating the agent's output, and deriving markers from it,
//! happen in `runkon-flow` once the schema is handed to the engine.
//!
//! Resolution order for short names (first match wins):
//! 1. `.conductor/workflows/<workflow-name>/schemas/<name>.yaml` — workflow-local override
//! 2. `.conductor/schemas/<name>.yaml` — shared schemas

mod issues;
mod loader;
mod prompt;
mod tool_definition;
mod types;

#[cfg(test)]
mod tests;

// Re-export all public items to preserve the existing `crate::schema_config::X` API.
pub use issues::{check_schemas, find_missing_schemas, SchemaIssue};
pub use loader::{load_schema, parse_schema_content};
pub use prompt::generate_prompt_instructions;
pub use tool_definition::schema_to_tool_json;
pub use types::{ArrayItems, FieldDef, FieldType, OutputSchema, SchemaRef};

// Items used by tests that are not otherwise public — re-export under cfg(test).
#[cfg(test)]
pub(crate) use loader::load_schema_by_path;
#[cfg(test)]
pub(crate) use prompt::generate_field_hints;
#[cfg(test)]
pub(crate) use runkon_flow::helpers::{fix_backslash_escapes, strip_trailing_commas};
