// Re-export runkon-flow output schema types so conductor-core and runkon-flow
// share a single type definition.
pub use runkon_flow::output_schema::{ArrayItems, FieldDef, FieldType, OutputSchema};

// ---------------------------------------------------------------------------
// Schema reference (how schemas are referenced from workflows)
// ---------------------------------------------------------------------------

/// How to locate a schema — either a short name or an explicit path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaRef {
    /// Short name (e.g. `review-findings`) resolved via search order.
    Name(String),
    /// Explicit path relative to the repo root (e.g. `./custom/schemas/my-review.yaml`).
    Path(String),
}

impl SchemaRef {
    /// Create a `SchemaRef` from a raw string value.
    ///
    /// Values containing `/` or `\` are treated as explicit paths; otherwise as names.
    pub fn from_str_value(s: &str) -> Self {
        if s.contains('/') || s.contains('\\') {
            Self::Path(s.to_string())
        } else {
            Self::Name(s.to_string())
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &str {
        match self {
            Self::Name(s) | Self::Path(s) => s.as_str(),
        }
    }
}
