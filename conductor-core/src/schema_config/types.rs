use std::collections::HashMap;

use serde::Deserialize;

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
// YAML deserialization (intermediate)
// ---------------------------------------------------------------------------

/// Raw YAML representation of a schema file.
#[derive(Debug, Deserialize)]
pub(super) struct RawSchema {
    pub(super) fields: HashMap<String, RawFieldDef>,
    #[serde(default)]
    pub(super) markers: Option<HashMap<String, String>>,
}

/// A field definition can be either a short form (just the type string) or
/// an object form with `type`, `desc`, `examples`, `items`, etc.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum RawFieldDef {
    /// Short form: `field_name: string`
    Short(String),
    /// Object form: `{ type: string, desc: "...", examples: [...] }`
    Object(RawFieldObject),
}

/// Array `items` can be either a scalar type string (`items: string`) or an
/// object map of named sub-fields (existing behaviour).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum RawArrayItems {
    /// Scalar type string, e.g. `"string"`, `"number"`, `"enum(a,b)"`.
    Scalar(String),
    /// Object map of named sub-fields.
    Object(HashMap<String, RawFieldDef>),
}

#[derive(Debug, Deserialize)]
pub(super) struct RawFieldObject {
    #[serde(rename = "type")]
    pub(super) field_type: Option<String>,
    pub(super) desc: Option<String>,
    pub(super) examples: Option<Vec<String>>,
    /// Sub-fields (or scalar type) for `array` items.
    pub(super) items: Option<RawArrayItems>,
    /// Sub-fields for `object` type.
    pub(super) fields: Option<HashMap<String, RawFieldDef>>,
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
