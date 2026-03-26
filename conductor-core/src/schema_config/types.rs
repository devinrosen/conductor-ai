use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

/// A parsed output schema definition from a `.yaml` file.
#[derive(Debug, Clone)]
pub struct OutputSchema {
    /// Schema name (from file stem).
    pub name: String,
    /// Top-level field definitions.
    pub fields: Vec<FieldDef>,
    /// Optional custom marker derivation rules.
    /// When present, only these rules apply (no implicit derivation).
    pub markers: Option<HashMap<String, String>>,
}

/// Definition of a single field in the schema.
#[derive(Debug, Clone)]
pub struct FieldDef {
    /// Field name (without `?` suffix).
    pub name: String,
    /// Whether this field is required.
    pub required: bool,
    /// Field type.
    pub field_type: FieldType,
    /// Human-readable description (included in prompt).
    pub desc: Option<String>,
    /// Example values (included in prompt).
    pub examples: Option<Vec<String>>,
}

/// Supported field types.
#[derive(Debug, Clone)]
pub enum FieldType {
    String,
    Number,
    Boolean,
    /// Enum with allowed values.
    Enum(Vec<String>),
    /// Array of items defined by sub-fields, a scalar type, or untyped.
    Array {
        items: ArrayItems,
    },
    /// Nested object with named fields.
    Object {
        fields: Vec<FieldDef>,
    },
}

/// Describes the element shape of an `Array` field.
///
/// The three variants are mutually exclusive by construction — no invalid
/// state is representable.
#[derive(Debug, Clone)]
pub enum ArrayItems {
    /// Scalar element type (e.g. `string`, `number`, `enum(…)`).
    Scalar(Box<FieldType>),
    /// Object-shaped sub-fields for each array element.
    Object(Vec<FieldDef>),
    /// Untyped / empty array — no item schema specified.
    Untyped,
}

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
