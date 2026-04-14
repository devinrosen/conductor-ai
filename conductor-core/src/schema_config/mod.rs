//! Schema-based structured output for workflow agents.
//!
//! Schemas live in `.conductor/schemas/<name>.yaml` and define the JSON shape
//! that an agent's `CONDUCTOR_OUTPUT` block must conform to. The workflow engine
//! uses schemas to:
//!
//! 1. Generate schema-specific output instructions in the agent prompt
//! 2. Parse and validate the structured JSON output
//! 3. Derive markers from field values for `if`/`while` conditions
//!
//! Resolution order for short names (first match wins):
//! 1. `.conductor/workflows/<workflow-name>/schemas/<name>.yaml` — workflow-local override
//! 2. `.conductor/schemas/<name>.yaml` — shared schemas

mod issues;
mod loader;
mod prompt;
mod tool_definition;
mod types;
mod validation;

#[cfg(test)]
mod tests;

// Re-export all public items to preserve the existing `crate::schema_config::X` API.
pub use issues::{check_schemas, find_missing_schemas, SchemaIssue};
pub use loader::{load_schema, parse_schema_content};
pub use prompt::generate_prompt_instructions;
pub use tool_definition::schema_to_tool_json;
pub use types::{ArrayItems, FieldDef, FieldType, OutputSchema, SchemaRef};
pub use validation::{derive_output_from_value, parse_structured_output, StructuredOutput};

// Items that are pub(crate) in the original file — re-export with same visibility.
pub(crate) use validation::extract_output_block;
pub(crate) use validation::fix_invalid_backslash_escapes;
pub(crate) use validation::strip_trailing_commas;

// Items used by tests that are not otherwise public — re-export under cfg(test).
#[cfg(test)]
pub(crate) use loader::load_schema_by_path;
#[cfg(test)]
pub(crate) use prompt::generate_field_hints;
#[cfg(test)]
pub(crate) use validation::{derive_default_markers, evaluate_marker_expr, strip_code_fences};
