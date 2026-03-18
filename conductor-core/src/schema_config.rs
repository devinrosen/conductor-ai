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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConductorError, Result};

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
    /// Array of items defined by sub-fields or a scalar type.
    ///
    /// Invariant: `item_type` and `items` are mutually exclusive — at most one
    /// may be populated.  `item_type.is_some()` implies `items.is_empty()` and
    /// vice-versa.  The parser enforces this by construction.
    Array {
        /// Scalar element type (e.g. `string`, `number`). Set when `items` is a
        /// bare type string rather than an object map.
        item_type: Option<Box<FieldType>>,
        /// Object-shaped sub-fields for each array element.
        items: Vec<FieldDef>,
    },
    /// Nested object with named fields.
    Object {
        fields: Vec<FieldDef>,
    },
}

// ---------------------------------------------------------------------------
// YAML deserialization (intermediate)
// ---------------------------------------------------------------------------

/// Raw YAML representation of a schema file.
#[derive(Debug, Deserialize)]
struct RawSchema {
    fields: HashMap<String, RawFieldDef>,
    #[serde(default)]
    markers: Option<HashMap<String, String>>,
}

/// A field definition can be either a short form (just the type string) or
/// an object form with `type`, `desc`, `examples`, `items`, etc.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawFieldDef {
    /// Short form: `field_name: string`
    Short(String),
    /// Object form: `{ type: string, desc: "...", examples: [...] }`
    Object(RawFieldObject),
}

/// Array `items` can be either a scalar type string (`items: string`) or an
/// object map of named sub-fields (existing behaviour).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawArrayItems {
    /// Scalar type string, e.g. `"string"`, `"number"`, `"enum(a,b)"`.
    Scalar(String),
    /// Object map of named sub-fields.
    Object(HashMap<String, RawFieldDef>),
}

#[derive(Debug, Deserialize)]
struct RawFieldObject {
    #[serde(rename = "type")]
    field_type: Option<String>,
    desc: Option<String>,
    examples: Option<Vec<String>>,
    /// Sub-fields (or scalar type) for `array` items.
    items: Option<RawArrayItems>,
    /// Sub-fields for `object` type.
    fields: Option<HashMap<String, RawFieldDef>>,
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

// ---------------------------------------------------------------------------
// Loading and resolution
// ---------------------------------------------------------------------------

/// Load a schema by reference.
pub fn load_schema(
    worktree_path: &str,
    repo_path: &str,
    schema_ref: &SchemaRef,
    workflow_name: Option<&str>,
) -> Result<OutputSchema> {
    match schema_ref {
        SchemaRef::Name(name) => load_schema_by_name(worktree_path, repo_path, name, workflow_name),
        SchemaRef::Path(rel_path) => load_schema_by_path(repo_path, rel_path),
    }
}

/// Check that a schema or workflow name does not contain path traversal components.
fn validate_name_segment(name: &str, label: &str) -> Result<()> {
    if name.contains("..") || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(ConductorError::Schema(format!(
            "{label} '{name}' contains invalid characters (path separators or '..' are not allowed)"
        )));
    }
    Ok(())
}

/// Resolve a schema by short name using the search order.
fn load_schema_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Result<OutputSchema> {
    validate_name_segment(name, "Schema name")?;
    if let Some(wf) = workflow_name {
        validate_name_segment(wf, "Workflow name")?;
    }

    let filename = format!("{name}.yaml");
    let bases = [worktree_path, repo_path];

    // 1. Workflow-local override (worktree, then repo)
    if let Some(wf_name) = workflow_name {
        let subdir = Path::new(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("schemas");
        if let Some(path) = find_schema_path(&bases, &subdir, &filename) {
            return parse_schema_file(&path);
        }
    }

    // 2. Shared conductor schemas (worktree, then repo)
    if let Some(path) = find_schema_path(&bases, Path::new(".conductor/schemas"), &filename) {
        return parse_schema_file(&path);
    }

    Err(ConductorError::Schema(format!(
        "Schema '{name}' not found. Searched:\n\
         {}  .conductor/schemas/{filename}",
        if let Some(wf) = workflow_name {
            format!("  .conductor/workflows/{wf}/schemas/{filename}\n")
        } else {
            String::new()
        }
    )))
}

/// Resolve a schema from an explicit path relative to the repo root.
fn load_schema_by_path(repo_path: &str, rel_path: &str) -> Result<OutputSchema> {
    if Path::new(rel_path).is_absolute() {
        return Err(ConductorError::Schema(format!(
            "Explicit schema path '{rel_path}' must be relative, not absolute"
        )));
    }

    let repo_root = PathBuf::from(repo_path);
    let joined = repo_root.join(rel_path);

    let canonical = joined.canonicalize().map_err(|_| {
        ConductorError::Schema(format!(
            "Schema file not found: '{rel_path}' (resolved relative to repo root '{repo_path}')"
        ))
    })?;

    let canonical_repo = repo_root.canonicalize().map_err(|e| {
        ConductorError::Schema(format!(
            "Failed to canonicalize repo root '{repo_path}': {e}"
        ))
    })?;

    if !canonical.starts_with(&canonical_repo) {
        return Err(ConductorError::Schema(format!(
            "Schema path '{rel_path}' escapes the repository root — path traversal is not allowed"
        )));
    }

    parse_schema_file(&canonical)
}

/// Return the first path that is a file, checking each base.
fn find_schema_path(bases: &[&str], subdir: &Path, filename: &str) -> Option<PathBuf> {
    bases.iter().find_map(|base| {
        let path = PathBuf::from(base).join(subdir).join(filename);
        path.is_file().then_some(path)
    })
}

/// Parse a schema YAML file into an `OutputSchema`.
fn parse_schema_file(path: &Path) -> Result<OutputSchema> {
    let content = fs::read_to_string(path).map_err(|e| {
        ConductorError::Schema(format!(
            "Failed to read schema file {}: {e}",
            path.display()
        ))
    })?;

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    parse_schema_content(&content, &name)
}

/// Parse schema YAML content into an `OutputSchema`.
pub fn parse_schema_content(content: &str, name: &str) -> Result<OutputSchema> {
    let raw: RawSchema = serde_yml::from_str(content)
        .map_err(|e| ConductorError::Schema(format!("Invalid schema YAML for '{name}': {e}")))?;

    let fields = parse_raw_fields(&raw.fields)?;

    Ok(OutputSchema {
        name: name.to_string(),
        fields,
        markers: raw.markers,
    })
}

// ---------------------------------------------------------------------------
// Field parsing
// ---------------------------------------------------------------------------

fn parse_raw_fields(raw: &HashMap<String, RawFieldDef>) -> Result<Vec<FieldDef>> {
    let mut fields = Vec::new();
    // Sort keys for deterministic output
    let mut keys: Vec<&String> = raw.keys().collect();
    keys.sort();

    for key in keys {
        let raw_def = &raw[key];
        let (field_name, required) = if key.ends_with('?') {
            (key.trim_end_matches('?').to_string(), false)
        } else {
            (key.to_string(), true)
        };

        let field_def = parse_single_field(&field_name, required, raw_def)?;
        fields.push(field_def);
    }
    Ok(fields)
}

fn parse_single_field(name: &str, required: bool, raw: &RawFieldDef) -> Result<FieldDef> {
    match raw {
        RawFieldDef::Short(type_str) => {
            let field_type = parse_type_str(type_str)?;
            Ok(FieldDef {
                name: name.to_string(),
                required,
                field_type,
                desc: None,
                examples: None,
            })
        }
        RawFieldDef::Object(obj) => {
            let type_str = obj.field_type.as_deref().unwrap_or("object");
            let field_type = match type_str {
                "array" => match &obj.items {
                    Some(RawArrayItems::Scalar(type_str)) => {
                        let scalar = parse_type_str(type_str)?;
                        FieldType::Array {
                            item_type: Some(Box::new(scalar)),
                            items: Vec::new(),
                        }
                    }
                    Some(RawArrayItems::Object(items_map)) => FieldType::Array {
                        item_type: None,
                        items: parse_raw_fields(items_map)?,
                    },
                    None => FieldType::Array {
                        item_type: None,
                        items: Vec::new(),
                    },
                },
                "object" => {
                    let sub_fields = if let Some(ref fields_map) = obj.fields {
                        parse_raw_fields(fields_map)?
                    } else {
                        Vec::new()
                    };
                    FieldType::Object { fields: sub_fields }
                }
                _ => parse_type_str(type_str)?,
            };
            Ok(FieldDef {
                name: name.to_string(),
                required,
                field_type,
                desc: obj.desc.clone(),
                examples: obj.examples.clone(),
            })
        }
    }
}

fn parse_type_str(s: &str) -> Result<FieldType> {
    let s = s.trim();
    if s == "string" {
        Ok(FieldType::String)
    } else if s == "number" {
        Ok(FieldType::Number)
    } else if s == "boolean" {
        Ok(FieldType::Boolean)
    } else if s == "array" {
        Ok(FieldType::Array {
            item_type: None,
            items: Vec::new(),
        })
    } else if s == "object" {
        Ok(FieldType::Object { fields: Vec::new() })
    } else if let Some(inner) = s.strip_prefix("enum(").and_then(|s| s.strip_suffix(')')) {
        let variants: Vec<String> = inner.split(',').map(|v| v.trim().to_string()).collect();
        if variants.is_empty() {
            return Err(ConductorError::Schema(
                "enum() must have at least one variant".to_string(),
            ));
        }
        Ok(FieldType::Enum(variants))
    } else {
        Err(ConductorError::Schema(format!(
            "Unknown field type: '{s}'. Expected one of: string, number, boolean, enum(...), array, object"
        )))
    }
}

// ---------------------------------------------------------------------------
// Prompt generation
// ---------------------------------------------------------------------------

/// Generate schema-specific output instructions to append to an agent prompt.
pub fn generate_prompt_instructions(schema: &OutputSchema) -> String {
    let mut out = String::new();
    out.push_str(
        "When you have finished your work, output the following block exactly as the\n\
         last thing in your response. Do not include this block in code examples or\n\
         anywhere else — only as the final output.\n\n\
         <<<CONDUCTOR_OUTPUT>>>\n",
    );

    let json_example = generate_json_example(&schema.fields, 0);
    out.push_str(&json_example);

    out.push_str("\n<<<END_CONDUCTOR_OUTPUT>>>\n");

    // Add field descriptions as hints
    let hints = generate_field_hints(&schema.fields, "");
    if !hints.is_empty() {
        out.push('\n');
        out.push_str(&hints);
    }

    out
}

fn generate_json_example(fields: &[FieldDef], indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    let mut lines = Vec::new();

    lines.push(format!("{pad}{{"));
    for (i, field) in fields.iter().enumerate() {
        let comma = if i + 1 < fields.len() { "," } else { "" };
        let value = generate_field_example_value(field, indent + 1);
        lines.push(format!("{inner_pad}\"{}\": {value}{comma}", field.name));
    }
    lines.push(format!("{pad}}}"));

    lines.join("\n")
}

fn generate_field_example_value(field: &FieldDef, indent: usize) -> String {
    let inner_pad = "  ".repeat(indent + 1);
    match &field.field_type {
        FieldType::String => {
            if let Some(ref desc) = field.desc {
                format!("\"{}\"", desc)
            } else {
                "\"...\"".to_string()
            }
        }
        FieldType::Number => "0".to_string(),
        FieldType::Boolean => "true".to_string(),
        FieldType::Enum(variants) => {
            let joined = variants.join("|");
            format!("\"{joined}\"")
        }
        FieldType::Array {
            item_type: Some(ft),
            ..
        } => {
            let example = match ft.as_ref() {
                FieldType::String => "\"...\", \"...\"",
                FieldType::Number => "0, 0",
                FieldType::Boolean => "true, false",
                FieldType::Enum(variants) => {
                    let joined = variants.join("|");
                    return format!("[\"{joined}\"]");
                }
                _ => return "[]".to_string(),
            };
            format!("[{example}]")
        }
        FieldType::Array { items, .. } if items.is_empty() => "[]".to_string(),
        FieldType::Array { items, .. } => {
            let item_json = generate_json_example(items, indent + 1);
            format!("[\n{item_json}\n{inner_pad}]")
        }
        FieldType::Object { fields } if fields.is_empty() => "{}".to_string(),
        FieldType::Object { fields } => generate_json_example(fields, indent),
    }
}

fn generate_field_hints(fields: &[FieldDef], prefix: &str) -> String {
    let mut hints = Vec::new();
    for field in fields {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{prefix}.{}", field.name)
        };

        let optional_tag = if !field.required { " (optional)" } else { "" };

        match &field.field_type {
            FieldType::Array {
                item_type: Some(ft),
                ..
            } => {
                let type_label = match ft.as_ref() {
                    FieldType::String => "string".to_owned(),
                    FieldType::Number => "number".to_owned(),
                    FieldType::Boolean => "boolean".to_owned(),
                    FieldType::Enum(v) => {
                        let joined = v.join(", ");
                        format!("enum({joined})")
                    }
                    _ => "unknown".to_owned(),
                };
                if let Some(ref desc) = field.desc {
                    hints.push(format!(
                        "\"{full_name}\"{optional_tag}: {desc} (array of {type_label})"
                    ));
                } else {
                    hints.push(format!(
                        "\"{full_name}\"{optional_tag}: array of {type_label}"
                    ));
                }
                if let Some(ref examples) = field.examples {
                    let examples_str = examples
                        .iter()
                        .map(|e| format!("\"{e}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    hints.push(format!("  examples: [{examples_str}]"));
                }
            }
            FieldType::Array { items, .. } if !items.is_empty() => {
                if let Some(ref desc) = field.desc {
                    hints.push(format!("\"{full_name}\"{optional_tag}: {desc}"));
                }
                let sub_hints = generate_field_hints(items, &format!("{full_name}[]"));
                if !sub_hints.is_empty() {
                    hints.push(sub_hints);
                }
            }
            FieldType::Object { fields: sub } if !sub.is_empty() => {
                if let Some(ref desc) = field.desc {
                    hints.push(format!("\"{full_name}\"{optional_tag}: {desc}"));
                }
                let sub_hints = generate_field_hints(sub, &full_name);
                if !sub_hints.is_empty() {
                    hints.push(sub_hints);
                }
            }
            _ => {
                if let Some(ref desc) = field.desc {
                    hints.push(format!("\"{full_name}\"{optional_tag}: {desc}"));
                }
                if let Some(ref examples) = field.examples {
                    let examples_str = examples
                        .iter()
                        .map(|e| format!("\"{e}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    hints.push(format!("  examples: [{examples_str}]"));
                }
                if field.desc.is_none() && !field.required {
                    hints.push(format!("\"{full_name}\" is optional and may be omitted."));
                }
            }
        }
    }
    hints.join("\n")
}

// ---------------------------------------------------------------------------
// Structured output validation
// ---------------------------------------------------------------------------

/// Validated structured output from an agent.
#[derive(Debug, Clone)]
pub struct StructuredOutput {
    /// The raw parsed JSON value.
    pub value: serde_json::Value,
    /// Derived markers for `if`/`while` conditions.
    pub markers: Vec<String>,
    /// Context string for `{{prior_context}}` (from `summary` field if present).
    pub context: String,
    /// The full JSON as a string for storage.
    pub json_string: String,
}

/// Find the start position of the real `<<<CONDUCTOR_OUTPUT>>>` block.
///
/// Returns the position of the last occurrence of `marker` where the immediately
/// following content (after trimming whitespace) starts with `{`, `[`, or a markdown
/// code fence (`` ` ``). This is the real block delimiter because:
/// - Occurrences inside sentences or code examples are not followed by JSON
/// - Occurrences inside a JSON field value appear mid-string, not at a JSON boundary
/// - The real block start is always immediately followed by JSON or a code-fenced JSON block
pub(crate) fn find_conductor_output_start(text: &str, marker: &str) -> Option<usize> {
    let mut last_valid = None;
    let mut search_pos = 0;
    while let Some(rel) = text[search_pos..].find(marker) {
        let abs = search_pos + rel;
        let after = text[abs + marker.len()..].trim_start();
        if after.starts_with('{') || after.starts_with('[') || after.starts_with('`') {
            last_valid = Some(abs);
        }
        search_pos = abs + 1;
    }
    last_valid
}

/// Extract and clean the raw JSON string from a `<<<CONDUCTOR_OUTPUT>>>` block.
///
/// Finds the last valid start marker occurrence, slices to the end marker,
/// trims whitespace, and strips markdown code fences. Returns `None` if no
/// valid block is found.
///
/// Trailing-comma stripping is intentionally omitted here — callers that need
/// it (e.g. `parse_structured_output`) apply it themselves.
pub(crate) fn extract_output_block(text: &str) -> Option<String> {
    let start_marker = "<<<CONDUCTOR_OUTPUT>>>";
    let end_marker = "<<<END_CONDUCTOR_OUTPUT>>>";

    let start = find_conductor_output_start(text, start_marker)?;
    let json_start = start + start_marker.len();
    let end = text[json_start..].find(end_marker)?;
    let raw = text[json_start..json_start + end].trim();

    Some(strip_code_fences(raw))
}

/// Parse the `<<<CONDUCTOR_OUTPUT>>>` block as structured JSON, validate against
/// the schema, and derive markers.
pub fn parse_structured_output(text: &str, schema: &OutputSchema) -> Result<StructuredOutput> {
    let start_marker = "<<<CONDUCTOR_OUTPUT>>>";
    let end_marker = "<<<END_CONDUCTOR_OUTPUT>>>";

    let start = find_conductor_output_start(text, start_marker).ok_or_else(|| {
        ConductorError::Schema("No <<<CONDUCTOR_OUTPUT>>> block found in agent output".to_string())
    })?;
    let json_start = start + start_marker.len();
    let end = text[json_start..].find(end_marker).ok_or_else(|| {
        ConductorError::Schema(
            "No <<<END_CONDUCTOR_OUTPUT>>> end marker found in agent output".to_string(),
        )
    })?;
    let raw = text[json_start..json_start + end].trim();
    let cleaned = strip_code_fences(raw);

    // Strip trailing commas (common LLM artifact)
    let cleaned = strip_trailing_commas(&cleaned);

    let value: serde_json::Value = serde_json::from_str(&cleaned)
        .map_err(|e| ConductorError::Schema(format!("Invalid JSON in CONDUCTOR_OUTPUT: {e}")))?;

    // Validate against schema
    validate_value(&value, &schema.fields)?;

    // Derive markers
    let markers = derive_markers(&value, schema);

    // Extract context from summary field
    let context = value
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let json_string = serde_json::to_string(&value)
        .expect("re-serializing a valid serde_json::Value should never fail");

    Ok(StructuredOutput {
        value,
        markers,
        context,
        json_string,
    })
}

/// Strip markdown code fences (```json ... ```) from the output.
pub(crate) fn strip_code_fences(s: &str) -> String {
    let s = s.trim();
    // Handle ```json\n...\n``` or ```\n...\n```
    if let Some(rest) = s.strip_prefix("```") {
        // Skip optional language tag (everything before first newline)
        let body = if let Some(idx) = rest.find('\n') {
            &rest[idx + 1..]
        } else {
            // No newline after opening fence — not a valid code block
            return s.to_string();
        };
        if let Some(content) = body.strip_suffix("```") {
            return content.trim().to_string();
        }
    }
    s.to_string()
}

/// Remove trailing commas before `}` or `]` (common LLM artifact).
fn strip_trailing_commas(s: &str) -> String {
    // Simple regex-like replacement: comma followed by optional whitespace then } or ]
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            // Look ahead past whitespace for } or ]
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                // Skip the comma, keep whitespace and closing bracket
                i += 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_value(value: &serde_json::Value, fields: &[FieldDef]) -> Result<()> {
    let obj = value.as_object().ok_or_else(|| {
        ConductorError::Schema("CONDUCTOR_OUTPUT must be a JSON object".to_string())
    })?;

    for field in fields {
        match obj.get(&field.name) {
            None if field.required => {
                return Err(ConductorError::Schema(format!(
                    "Missing required field: '{}'",
                    field.name
                )));
            }
            None => continue,
            Some(val) => validate_field_value(val, field)?,
        }
    }

    Ok(())
}

fn validate_field_value(value: &serde_json::Value, field: &FieldDef) -> Result<()> {
    match &field.field_type {
        FieldType::String => {
            if !value.is_string() {
                return Err(ConductorError::Schema(format!(
                    "Field '{}' expected string, got {}",
                    field.name,
                    json_type_name(value)
                )));
            }
        }
        FieldType::Number => {
            if !value.is_number() {
                return Err(ConductorError::Schema(format!(
                    "Field '{}' expected number, got {}",
                    field.name,
                    json_type_name(value)
                )));
            }
        }
        FieldType::Boolean => {
            if !value.is_boolean() {
                return Err(ConductorError::Schema(format!(
                    "Field '{}' expected boolean, got {}",
                    field.name,
                    json_type_name(value)
                )));
            }
        }
        FieldType::Enum(variants) => {
            let s = value.as_str().ok_or_else(|| {
                ConductorError::Schema(format!(
                    "Field '{}' expected enum string, got {}",
                    field.name,
                    json_type_name(value)
                ))
            })?;
            if !variants.contains(&s.to_string()) {
                return Err(ConductorError::Schema(format!(
                    "Field '{}' value '{}' is not one of: {}",
                    field.name,
                    s,
                    variants.join(", ")
                )));
            }
        }
        FieldType::Array { item_type, items } => {
            debug_assert!(
                item_type.is_none() || items.is_empty(),
                "FieldType::Array invariant violated: item_type and items are mutually exclusive"
            );
            let arr = value.as_array().ok_or_else(|| {
                ConductorError::Schema(format!(
                    "Field '{}' expected array, got {}",
                    field.name,
                    json_type_name(value)
                ))
            })?;
            if let Some(ft) = item_type {
                // Scalar-typed array: validate each element against the scalar type.
                // Hoist the FieldType clone and FieldDef outside the loop so we
                // allocate O(1) instead of O(n) for enum variants.
                let mut synthetic = FieldDef {
                    name: String::new(),
                    required: true,
                    field_type: *ft.clone(),
                    desc: None,
                    examples: None,
                };
                for (i, elem) in arr.iter().enumerate() {
                    synthetic.name = format!("{}[{}]", field.name, i);
                    validate_field_value(elem, &synthetic)?;
                }
            } else if !items.is_empty() {
                for (i, elem) in arr.iter().enumerate() {
                    validate_value(elem, items).map_err(|e| {
                        ConductorError::Schema(format!("In '{}[{}]': {e}", field.name, i))
                    })?;
                }
            }
        }
        FieldType::Object { fields } => {
            if !value.is_object() {
                return Err(ConductorError::Schema(format!(
                    "Field '{}' expected object, got {}",
                    field.name,
                    json_type_name(value)
                )));
            }
            if !fields.is_empty() {
                validate_value(value, fields)?;
            }
        }
    }
    Ok(())
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Marker derivation
// ---------------------------------------------------------------------------

/// Derive markers from structured output based on schema rules.
fn derive_markers(value: &serde_json::Value, schema: &OutputSchema) -> Vec<String> {
    if let Some(ref rules) = schema.markers {
        // Explicit rules only
        let mut markers = Vec::new();
        for (marker_name, expr) in rules {
            if evaluate_marker_expr(value, expr) {
                markers.push(marker_name.clone());
            }
        }
        markers.sort();
        markers
    } else {
        // Default implicit derivation
        derive_default_markers(value)
    }
}

/// Evaluate a marker derivation expression against a JSON value.
///
/// Supported expressions:
/// - `field == value` — equality check
/// - `field.length > 0` — array/string length comparison
/// - `field[subfield == value].length > 0` — filtered array length
fn evaluate_marker_expr(value: &serde_json::Value, expr: &str) -> bool {
    let expr = expr.trim();

    // Pattern: field[subfield == value].length > 0
    if let Some(result) = try_eval_filtered_length(value, expr) {
        return result;
    }

    // Pattern: field.length > 0 or field.length == 0
    if let Some(result) = try_eval_length(value, expr) {
        return result;
    }

    // Pattern: field == value or field == false/true
    if let Some(result) = try_eval_equality(value, expr) {
        return result;
    }

    // Pattern: field < N (numeric comparison)
    if let Some(result) = try_eval_numeric_comparison(value, expr) {
        return result;
    }

    false
}

/// Try to evaluate `field.length > N` or `field.length == N`.
fn try_eval_length(value: &serde_json::Value, expr: &str) -> Option<bool> {
    // Match: <field>.length <op> <number>
    let (field_part, rest) = expr.split_once(".length")?;
    let rest = rest.trim();

    let field_val = value.get(field_part.trim())?;
    let len = match field_val {
        serde_json::Value::Array(arr) => arr.len(),
        serde_json::Value::String(s) => s.len(),
        _ => return None,
    };

    eval_comparison(len, rest)
}

/// Try to evaluate `field[subfield == value].length > N`.
fn try_eval_filtered_length(value: &serde_json::Value, expr: &str) -> Option<bool> {
    // Match: <field>[<subfield> == <value>].length <op> <number>
    let bracket_start = expr.find('[')?;
    let bracket_end = expr.find(']')?;
    if bracket_start >= bracket_end {
        return None;
    }

    let field_name = expr[..bracket_start].trim();
    let filter_expr = expr[bracket_start + 1..bracket_end].trim();
    let after_bracket = expr[bracket_end + 1..].trim();

    // After bracket must be .length <op> <number>
    let rest = after_bracket.strip_prefix(".length")?;
    let rest = rest.trim();

    // Parse filter: subfield == value
    let (sub_field, sub_value) = filter_expr.split_once("==")?;
    let sub_field = sub_field.trim();
    let sub_value = sub_value.trim();

    let arr = value.get(field_name)?.as_array()?;
    let filtered_count = arr
        .iter()
        .filter(|item| {
            item.get(sub_field)
                .and_then(|v| v.as_str())
                .is_some_and(|s| s == sub_value)
        })
        .count();

    eval_comparison(filtered_count, rest)
}

/// Try to evaluate `field == value`.
fn try_eval_equality(value: &serde_json::Value, expr: &str) -> Option<bool> {
    let (field, rhs) = expr.split_once("==")?;
    let field = field.trim();
    let rhs = rhs.trim();

    let field_val = value.get(field)?;

    Some(match rhs {
        "true" => field_val.as_bool() == Some(true),
        "false" => field_val.as_bool() == Some(false),
        _ => {
            // String comparison
            if let Some(s) = field_val.as_str() {
                s == rhs
            } else if let Some(n) = field_val.as_f64() {
                rhs.parse::<f64>()
                    .ok()
                    .is_some_and(|rn| (n - rn).abs() < f64::EPSILON)
            } else {
                false
            }
        }
    })
}

/// Try to evaluate `field < N` or `field > N`.
fn try_eval_numeric_comparison(value: &serde_json::Value, expr: &str) -> Option<bool> {
    // Try < first, then >
    for op in ["<", ">"] {
        if let Some((field, rhs)) = expr.split_once(op) {
            let field = field.trim();
            let rhs = rhs.trim();
            // Make sure this isn't == which we already handled
            if field.ends_with('=') || rhs.starts_with('=') {
                continue;
            }
            let field_val = value.get(field)?.as_f64()?;
            let rhs_val: f64 = rhs.parse().ok()?;
            return Some(match op {
                "<" => field_val < rhs_val,
                ">" => field_val > rhs_val,
                _ => false,
            });
        }
    }
    None
}

/// Evaluate a comparison like `> 0`, `== 0`, `< 5`.
fn eval_comparison(len: usize, rest: &str) -> Option<bool> {
    let rest = rest.trim();
    if let Some(rhs) = rest.strip_prefix('>') {
        let n: usize = rhs.trim().parse().ok()?;
        Some(len > n)
    } else if let Some(rhs) = rest.strip_prefix("==") {
        let n: usize = rhs.trim().parse().ok()?;
        Some(len == n)
    } else if let Some(rhs) = rest.strip_prefix('<') {
        let n: usize = rhs.trim().parse().ok()?;
        Some(len < n)
    } else {
        None
    }
}

/// Default marker derivation when no explicit rules are defined.
fn derive_default_markers(value: &serde_json::Value) -> Vec<String> {
    let mut markers = Vec::new();
    let obj = match value.as_object() {
        Some(o) => o,
        None => return markers,
    };

    // approved: false → not_approved
    if let Some(approved) = obj.get("approved") {
        if approved.as_bool() == Some(false) {
            markers.push("not_approved".to_string());
        }
    }

    // findings array non-empty → has_findings
    if let Some(findings) = obj.get("findings") {
        if let Some(arr) = findings.as_array() {
            if !arr.is_empty() {
                markers.push("has_findings".to_string());
            }
            // Check severity levels
            for item in arr {
                if let Some(severity) = item.get("severity").and_then(|v| v.as_str()) {
                    match severity {
                        "critical" => {
                            if !markers.contains(&"has_critical_findings".to_string()) {
                                markers.push("has_critical_findings".to_string());
                            }
                        }
                        "high" => {
                            if !markers.contains(&"has_high_findings".to_string()) {
                                markers.push("has_high_findings".to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    markers.sort();
    markers
}

/// An issue detected when checking a schema reference during validation.
#[derive(Debug)]
pub enum SchemaIssue {
    /// Schema file was not found at any of the search paths.
    Missing(String),
    /// Schema file was found but could not be parsed or is structurally invalid.
    Invalid { name: String, error: String },
}

/// Locate the filesystem path for a schema short-name without parsing it.
///
/// Returns `None` if the file does not exist at any expected location.
fn locate_schema_name_path(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Option<PathBuf> {
    let filename = format!("{name}.yaml");
    let bases = [worktree_path, repo_path];
    if let Some(wf_name) = workflow_name {
        let subdir = Path::new(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("schemas");
        if let Some(path) = find_schema_path(&bases, &subdir, &filename) {
            return Some(path);
        }
    }
    find_schema_path(&bases, Path::new(".conductor/schemas"), &filename)
}

/// Check all referenced output schemas and return any issues found.
///
/// Unlike [`find_missing_schemas`], this function distinguishes between schemas
/// that cannot be found and schemas that exist but contain invalid content.
pub fn check_schemas(
    worktree_path: &str,
    repo_path: &str,
    schema_names: &[String],
    workflow_name: Option<&str>,
) -> Vec<SchemaIssue> {
    schema_names
        .iter()
        .filter_map(|name| {
            let schema_ref = SchemaRef::from_str_value(name);
            let path = match &schema_ref {
                SchemaRef::Name(n) => {
                    if let Err(e) = validate_name_segment(n, "Schema name") {
                        return Some(SchemaIssue::Invalid {
                            name: name.clone(),
                            error: e.to_string(),
                        });
                    }
                    if let Some(wf) = workflow_name {
                        if let Err(e) = validate_name_segment(wf, "Workflow name") {
                            return Some(SchemaIssue::Invalid {
                                name: name.clone(),
                                error: e.to_string(),
                            });
                        }
                    }
                    match locate_schema_name_path(worktree_path, repo_path, n, workflow_name) {
                        None => return Some(SchemaIssue::Missing(name.clone())),
                        Some(p) => p,
                    }
                }
                SchemaRef::Path(rel) => {
                    if Path::new(rel).is_absolute() {
                        return Some(SchemaIssue::Invalid {
                            name: name.clone(),
                            error: format!(
                                "Explicit schema path '{rel}' must be relative, not absolute"
                            ),
                        });
                    }
                    let repo_root = PathBuf::from(repo_path);
                    let joined = repo_root.join(rel);
                    if !joined.is_file() {
                        return Some(SchemaIssue::Missing(name.clone()));
                    }
                    // Enforce the same path-traversal guard as load_schema_by_path
                    if let (Ok(canonical), Ok(canonical_repo)) =
                        (joined.canonicalize(), repo_root.canonicalize())
                    {
                        if !canonical.starts_with(&canonical_repo) {
                            return Some(SchemaIssue::Invalid {
                                name: name.clone(),
                                error: format!(
                                    "Schema path '{rel}' escapes the repository root — path traversal is not allowed"
                                ),
                            });
                        }
                        canonical
                    } else {
                        joined
                    }
                }
            };
            match parse_schema_file(&path) {
                Ok(_) => None,
                Err(e) => Some(SchemaIssue::Invalid {
                    name: name.clone(),
                    error: e.to_string(),
                }),
            }
        })
        .collect()
}

/// Check whether all referenced output schemas exist (for validation).
///
/// Returns a list of schema names/paths that are truly missing (not found at
/// any search path). Schemas that exist but contain invalid YAML are **not**
/// included; use [`check_schemas`] to surface those separately.
pub fn find_missing_schemas(
    worktree_path: &str,
    repo_path: &str,
    schema_names: &[String],
    workflow_name: Option<&str>,
) -> Vec<String> {
    check_schemas(worktree_path, repo_path, schema_names, workflow_name)
        .into_iter()
        .filter_map(|issue| match issue {
            SchemaIssue::Missing(name) => Some(name),
            SchemaIssue::Invalid { .. } => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCHEMA_YAML: &str = r#"
fields:
  findings:
    type: array
    items:
      file: string
      line: number
      severity: enum(critical, high, medium, low, info)
      category:
        type: string
        desc: "OWASP category or general area"
        examples: ["injection", "auth", "config", "cryptography"]
      message: string
      suggestion?:
        type: string
        desc: "Suggested fix or remediation"
  approved: boolean
  summary: string

markers:
  has_findings: "findings.length > 0"
  has_critical_findings: "findings[severity == critical].length > 0"
  has_high_findings: "findings[severity == high].length > 0"
  not_approved: "approved == false"
"#;

    #[test]
    fn test_parse_schema() {
        let schema = parse_schema_content(TEST_SCHEMA_YAML, "review-findings").unwrap();
        assert_eq!(schema.name, "review-findings");
        assert_eq!(schema.fields.len(), 3);

        // Check approved field
        let approved = schema.fields.iter().find(|f| f.name == "approved").unwrap();
        assert!(approved.required);
        assert!(matches!(approved.field_type, FieldType::Boolean));

        // Check findings field
        let findings = schema.fields.iter().find(|f| f.name == "findings").unwrap();
        assert!(findings.required);
        if let FieldType::Array { items, .. } = &findings.field_type {
            assert!(items.len() >= 5);
            let severity = items.iter().find(|f| f.name == "severity").unwrap();
            if let FieldType::Enum(variants) = &severity.field_type {
                assert_eq!(variants.len(), 5);
                assert!(variants.contains(&"critical".to_string()));
            } else {
                panic!("severity should be enum");
            }
            // suggestion should be optional
            let suggestion = items.iter().find(|f| f.name == "suggestion").unwrap();
            assert!(!suggestion.required);
        } else {
            panic!("findings should be array");
        }

        // Check markers
        assert!(schema.markers.is_some());
        let markers = schema.markers.as_ref().unwrap();
        assert_eq!(markers.len(), 4);
        assert_eq!(markers["has_findings"], "findings.length > 0");
    }

    #[test]
    fn test_parse_short_form_fields() {
        let yaml = r#"
fields:
  name: string
  count: number
  active: boolean
  status: enum(open, closed)
"#;
        let schema = parse_schema_content(yaml, "test").unwrap();
        assert_eq!(schema.fields.len(), 4);
        assert!(matches!(
            schema
                .fields
                .iter()
                .find(|f| f.name == "name")
                .unwrap()
                .field_type,
            FieldType::String
        ));
        assert!(matches!(
            schema
                .fields
                .iter()
                .find(|f| f.name == "count")
                .unwrap()
                .field_type,
            FieldType::Number
        ));
        assert!(matches!(
            schema
                .fields
                .iter()
                .find(|f| f.name == "active")
                .unwrap()
                .field_type,
            FieldType::Boolean
        ));
    }

    #[test]
    fn test_validate_valid_output() {
        let schema = parse_schema_content(TEST_SCHEMA_YAML, "test").unwrap();
        let json = r#"
<<<CONDUCTOR_OUTPUT>>>
{
  "findings": [
    {
      "file": "src/auth.rs",
      "line": 42,
      "severity": "high",
      "category": "injection",
      "message": "SQL injection risk"
    }
  ],
  "approved": false,
  "summary": "Found 1 high severity issue"
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(json, &schema).unwrap();
        assert_eq!(result.context, "Found 1 high severity issue");
        assert!(result.markers.contains(&"has_findings".to_string()));
        assert!(result.markers.contains(&"has_high_findings".to_string()));
        assert!(result.markers.contains(&"not_approved".to_string()));
    }

    #[test]
    fn test_validate_missing_required_field() {
        let schema = parse_schema_content(TEST_SCHEMA_YAML, "test").unwrap();
        let json = r#"
<<<CONDUCTOR_OUTPUT>>>
{
  "findings": [],
  "summary": "All good"
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(json, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("approved"));
    }

    #[test]
    fn test_validate_wrong_type() {
        let schema = parse_schema_content(TEST_SCHEMA_YAML, "test").unwrap();
        let json = r#"
<<<CONDUCTOR_OUTPUT>>>
{
  "findings": [],
  "approved": "yes",
  "summary": "All good"
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(json, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("approved"));
    }

    #[test]
    fn test_validate_invalid_enum() {
        let schema = parse_schema_content(TEST_SCHEMA_YAML, "test").unwrap();
        let json = r#"
<<<CONDUCTOR_OUTPUT>>>
{
  "findings": [
    {
      "file": "test.rs",
      "line": 1,
      "severity": "extreme",
      "category": "test",
      "message": "test"
    }
  ],
  "approved": true,
  "summary": "test"
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(json, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("extreme"));
    }

    #[test]
    fn test_lenient_parsing_code_fences() {
        let schema =
            parse_schema_content("fields:\n  name: string\n  count: number\n", "test").unwrap();
        let json = r#"
<<<CONDUCTOR_OUTPUT>>>
```json
{
  "name": "hello",
  "count": 42
}
```
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(json, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn test_lenient_parsing_trailing_commas() {
        let schema =
            parse_schema_content("fields:\n  name: string\n  count: number\n", "test").unwrap();
        let json = r#"
<<<CONDUCTOR_OUTPUT>>>
{
  "name": "hello",
  "count": 42,
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(json, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn test_default_markers_approved_false() {
        let value: serde_json::Value = serde_json::from_str(r#"{"approved": false}"#).unwrap();
        let markers = derive_default_markers(&value);
        assert!(markers.contains(&"not_approved".to_string()));
    }

    #[test]
    fn test_default_markers_findings() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"findings": [{"severity": "critical"}, {"severity": "high"}]}"#,
        )
        .unwrap();
        let markers = derive_default_markers(&value);
        assert!(markers.contains(&"has_findings".to_string()));
        assert!(markers.contains(&"has_critical_findings".to_string()));
        assert!(markers.contains(&"has_high_findings".to_string()));
    }

    #[test]
    fn test_custom_marker_expressions() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "findings": [
                    {"severity": "critical", "file": "a.rs"},
                    {"severity": "low", "file": "b.rs"}
                ],
                "approved": false,
                "coverage_percent": 75
            }"#,
        )
        .unwrap();

        assert!(evaluate_marker_expr(&value, "findings.length > 0"));
        assert!(!evaluate_marker_expr(&value, "findings.length == 0"));
        assert!(evaluate_marker_expr(
            &value,
            "findings[severity == critical].length > 0"
        ));
        assert!(!evaluate_marker_expr(
            &value,
            "findings[severity == high].length > 0"
        ));
        assert!(evaluate_marker_expr(&value, "approved == false"));
        assert!(!evaluate_marker_expr(&value, "approved == true"));
        assert!(evaluate_marker_expr(&value, "coverage_percent < 80"));
        assert!(!evaluate_marker_expr(&value, "coverage_percent > 80"));
    }

    #[test]
    fn test_prompt_generation() {
        let schema =
            parse_schema_content("fields:\n  approved: boolean\n  summary: string\n", "test")
                .unwrap();
        let prompt = generate_prompt_instructions(&schema);
        assert!(prompt.contains("<<<CONDUCTOR_OUTPUT>>>"));
        assert!(prompt.contains("<<<END_CONDUCTOR_OUTPUT>>>"));
        assert!(prompt.contains("\"approved\""));
        assert!(prompt.contains("\"summary\""));
    }

    #[test]
    fn test_schema_ref_from_str() {
        assert_eq!(
            SchemaRef::from_str_value("review-findings"),
            SchemaRef::Name("review-findings".to_string())
        );
        assert_eq!(
            SchemaRef::from_str_value("./custom/schemas/review.yaml"),
            SchemaRef::Path("./custom/schemas/review.yaml".to_string())
        );
    }

    #[test]
    fn test_schema_resolution_order() {
        use tempfile::TempDir;

        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Put schema in repo shared dir
        let schemas_dir = repo.path().join(".conductor").join("schemas");
        fs::create_dir_all(&schemas_dir).unwrap();
        fs::write(
            schemas_dir.join("review.yaml"),
            "fields:\n  summary: string\n",
        )
        .unwrap();

        let schema = load_schema(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &SchemaRef::Name("review".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(schema.name, "review");

        // Workflow-local override
        let wf_schemas = worktree
            .path()
            .join(".conductor")
            .join("workflows")
            .join("my-wf")
            .join("schemas");
        fs::create_dir_all(&wf_schemas).unwrap();
        fs::write(
            wf_schemas.join("review.yaml"),
            "fields:\n  count: number\n  summary: string\n",
        )
        .unwrap();

        let schema = load_schema(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &SchemaRef::Name("review".to_string()),
            Some("my-wf"),
        )
        .unwrap();
        // Workflow-local has 2 fields, shared has 1
        assert_eq!(schema.fields.len(), 2);
    }

    #[test]
    fn test_schema_not_found() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let result = load_schema(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &SchemaRef::Name("nonexistent".to_string()),
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_optional_field_not_required() {
        let yaml = "fields:\n  name: string\n  nickname?: string\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let name_field = schema.fields.iter().find(|f| f.name == "name").unwrap();
        assert!(name_field.required);
        let nickname_field = schema.fields.iter().find(|f| f.name == "nickname").unwrap();
        assert!(!nickname_field.required);
    }

    #[test]
    fn test_strip_trailing_commas() {
        assert_eq!(
            strip_trailing_commas(r#"{"a": 1, "b": 2,}"#),
            r#"{"a": 1, "b": 2}"#
        );
        assert_eq!(strip_trailing_commas(r#"[1, 2, 3,]"#), r#"[1, 2, 3]"#);
    }

    #[test]
    fn test_strip_code_fences() {
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_code_fences(input), "{\"a\": 1}");

        let input2 = "```\n{\"a\": 1}\n```";
        assert_eq!(strip_code_fences(input2), "{\"a\": 1}");

        let input3 = "{\"a\": 1}";
        assert_eq!(strip_code_fences(input3), "{\"a\": 1}");
    }

    // -----------------------------------------------------------------------
    // load_schema_by_path tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_schema_by_path_rejects_absolute() {
        let result = load_schema_by_path("/tmp", "/etc/passwd");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("must be relative"));
    }

    #[test]
    fn test_load_schema_by_path_rejects_traversal() {
        use tempfile::TempDir;
        let repo = TempDir::new().unwrap();

        // Create a schema file outside the repo to attempt traversal
        let outside = TempDir::new().unwrap();
        fs::write(
            outside.path().join("evil.yaml"),
            "fields:\n  name: string\n",
        )
        .unwrap();

        // Build a relative path that escapes the repo root
        let repo_path = repo.path().to_str().unwrap();
        let outside_path = outside.path().to_str().unwrap();
        // Compute relative traversal from repo to outside dir
        let rel = format!(
            "../../../{}/evil.yaml",
            outside_path.trim_start_matches('/')
        );

        let result = load_schema_by_path(repo_path, &rel);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("path traversal") || msg.contains("not found"),
            "Expected path traversal or not found error, got: {msg}"
        );
    }

    #[test]
    fn test_load_schema_by_path_valid() {
        use tempfile::TempDir;
        let repo = TempDir::new().unwrap();

        let custom_dir = repo.path().join("custom").join("schemas");
        fs::create_dir_all(&custom_dir).unwrap();
        fs::write(
            custom_dir.join("review.yaml"),
            "fields:\n  verdict: string\n",
        )
        .unwrap();

        let schema =
            load_schema_by_path(repo.path().to_str().unwrap(), "custom/schemas/review.yaml")
                .unwrap();
        assert_eq!(schema.name, "review");
        assert_eq!(schema.fields.len(), 1);
        assert_eq!(schema.fields[0].name, "verdict");
    }

    // -----------------------------------------------------------------------
    // Name sanitization tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_schema_name_rejects_path_traversal() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let result = load_schema(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &SchemaRef::Name("..".to_string()),
            None,
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid characters"));
    }

    #[test]
    fn test_workflow_name_rejects_path_traversal() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let result = load_schema(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &SchemaRef::Name("review".to_string()),
            Some("../../etc"),
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid characters"));
    }

    #[test]
    fn test_schema_ref_backslash_treated_as_path() {
        assert_eq!(
            SchemaRef::from_str_value("..\\..\\etc\\passwd"),
            SchemaRef::Path("..\\..\\etc\\passwd".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Missing output block and malformed expression tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_structured_output_no_block() {
        let schema = parse_schema_content("fields:\n  name: string\n", "test").unwrap();
        let result = parse_structured_output("This output has no CONDUCTOR_OUTPUT block", &schema);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No <<<CONDUCTOR_OUTPUT>>>"));
    }

    #[test]
    fn test_parse_structured_output_missing_end_marker() {
        let schema = parse_schema_content("fields:\n  name: string\n", "test").unwrap();
        let result = parse_structured_output(
            "<<<CONDUCTOR_OUTPUT>>>\n{\"name\": \"hello\"}\nno end marker here",
            &schema,
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("END_CONDUCTOR_OUTPUT"));
    }

    #[test]
    fn test_malformed_marker_expressions_return_false() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"name": "test", "count": 5}"#).unwrap();

        // Completely invalid expressions should return false, not panic
        assert!(!evaluate_marker_expr(&value, ""));
        assert!(!evaluate_marker_expr(&value, "not a valid expression"));
        assert!(!evaluate_marker_expr(&value, "field !=! value"));
        assert!(!evaluate_marker_expr(&value, "nonexistent_field == 5"));
    }

    // -----------------------------------------------------------------------
    // check_schemas tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_check_schemas_missing_schema() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["nonexistent".to_string()],
            None,
        );
        assert_eq!(issues.len(), 1);
        assert!(matches!(&issues[0], SchemaIssue::Missing(n) if n == "nonexistent"));
    }

    #[test]
    fn test_check_schemas_no_issues_when_schema_exists() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let schemas_dir = repo.path().join(".conductor").join("schemas");
        fs::create_dir_all(&schemas_dir).unwrap();
        fs::write(
            schemas_dir.join("review.yaml"),
            "fields:\n  summary: string\n",
        )
        .unwrap();

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["review".to_string()],
            None,
        );
        assert!(issues.is_empty());
    }

    #[test]
    fn test_check_schemas_invalid_yaml() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let schemas_dir = repo.path().join(".conductor").join("schemas");
        fs::create_dir_all(&schemas_dir).unwrap();
        fs::write(
            schemas_dir.join("broken.yaml"),
            "fields: [this: is: not: valid\n",
        )
        .unwrap();

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["broken".to_string()],
            None,
        );
        assert_eq!(issues.len(), 1);
        assert!(matches!(&issues[0], SchemaIssue::Invalid { name, .. } if name == "broken"));
    }

    #[test]
    fn test_check_schemas_invalid_schema_name_returns_invalid_not_missing() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // A name with ".." should return Invalid, not Missing
        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["../etc/passwd".to_string()],
            None,
        );
        // "../etc/passwd" contains '/' so it is treated as a SchemaRef::Path — missing file
        // but a pure ".." name (no slash) is SchemaRef::Name and should be Invalid
        let issues2 = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["..".to_string()],
            None,
        );
        assert_eq!(issues2.len(), 1);
        assert!(matches!(&issues2[0], SchemaIssue::Invalid { name, error }
            if name == ".." && error.contains("invalid characters")));
        // The path variant should be Missing (file not found), not Invalid
        assert_eq!(issues.len(), 1);
        assert!(matches!(&issues[0], SchemaIssue::Missing(_)));
    }

    #[test]
    fn test_check_schemas_absolute_path_returns_invalid() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["/etc/passwd".to_string()],
            None,
        );
        assert_eq!(issues.len(), 1);
        assert!(matches!(&issues[0], SchemaIssue::Invalid { name, error }
            if name == "/etc/passwd" && error.contains("must be relative")));
    }

    #[test]
    fn test_check_schemas_path_traversal_returns_invalid() {
        use tempfile::TempDir;
        let repo = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();

        // Create a schema file outside the repo root
        let outside = TempDir::new().unwrap();
        fs::write(
            outside.path().join("evil.yaml"),
            "fields:\n  name: string\n",
        )
        .unwrap();

        // Build a relative path that traverses outside the repo
        let repo_path = repo.path().to_str().unwrap();
        let outside_path = outside.path().to_str().unwrap();
        let rel = format!(
            "../../../{}/evil.yaml",
            outside_path.trim_start_matches('/')
        );

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo_path,
            std::slice::from_ref(&rel),
            None,
        );
        assert_eq!(issues.len(), 1);
        // Either traversal rejected (Invalid) or file not found (Missing) — both are acceptable
        assert!(matches!(
            &issues[0],
            SchemaIssue::Invalid { .. } | SchemaIssue::Missing(_)
        ));
    }

    #[test]
    fn test_check_schemas_empty_input() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &[],
            None,
        );
        assert!(issues.is_empty());
    }

    #[test]
    fn test_check_schemas_path_ref_valid() {
        use tempfile::TempDir;
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let custom_dir = repo.path().join("custom").join("schemas");
        fs::create_dir_all(&custom_dir).unwrap();
        fs::write(
            custom_dir.join("review.yaml"),
            "fields:\n  verdict: string\n",
        )
        .unwrap();

        let issues = check_schemas(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &["custom/schemas/review.yaml".to_string()],
            None,
        );
        assert!(issues.is_empty());
    }

    /// Marker appears in code examples before the real block — structured path must find the real block.
    #[test]
    fn test_parse_structured_output_skips_code_examples() {
        let schema_yaml = "fields:\n  summary: string\n";
        let schema = parse_schema_content(schema_yaml, "test").unwrap();

        let text = r#"Here is how to emit output:
```bash
echo '<<<CONDUCTOR_OUTPUT>>>'
echo '{"summary": "fake"}'
echo '<<<END_CONDUCTOR_OUTPUT>>>'
```

Actual output:
<<<CONDUCTOR_OUTPUT>>>
{"summary": "real result"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(text, &schema).unwrap();
        assert_eq!(result.context, "real result");
    }

    /// Multiple complete blocks before the real one — structured path must find the last valid block.
    #[test]
    fn test_parse_structured_output_multiple_complete_blocks() {
        let schema_yaml = "fields:\n  summary: string\n";
        let schema = parse_schema_content(schema_yaml, "test").unwrap();

        let text = r#"Example 1:
<<<CONDUCTOR_OUTPUT>>>
{"summary": "first example"}
<<<END_CONDUCTOR_OUTPUT>>>

Example 2:
<<<CONDUCTOR_OUTPUT>>>
{"summary": "second example"}
<<<END_CONDUCTOR_OUTPUT>>>

Real output:
<<<CONDUCTOR_OUTPUT>>>
{"summary": "the actual result"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(text, &schema).unwrap();
        assert_eq!(result.context, "the actual result");
    }

    /// Output block wrapped in a markdown code fence — structured path must strip fences.
    #[test]
    fn test_parse_structured_output_code_fenced() {
        let schema_yaml = "fields:\n  summary: string\n";
        let schema = parse_schema_content(schema_yaml, "test").unwrap();

        let text = r#"Here is my output:
<<<CONDUCTOR_OUTPUT>>>
```json
{"summary": "fenced result"}
```
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(text, &schema).unwrap();
        assert_eq!(result.context, "fenced result");
    }

    // -----------------------------------------------------------------------
    // Scalar array tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_scalar_array_string() {
        let yaml = "fields:\n  tags:\n    type: array\n    items: string\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let field = schema.fields.iter().find(|f| f.name == "tags").unwrap();
        match &field.field_type {
            FieldType::Array { item_type, items } => {
                assert!(items.is_empty());
                assert!(matches!(item_type.as_deref(), Some(FieldType::String)));
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_parse_scalar_array_number() {
        let yaml = "fields:\n  scores:\n    type: array\n    items: number\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let field = schema.fields.iter().find(|f| f.name == "scores").unwrap();
        match &field.field_type {
            FieldType::Array { item_type, items } => {
                assert!(items.is_empty());
                assert!(matches!(item_type.as_deref(), Some(FieldType::Number)));
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_parse_scalar_array_boolean() {
        let yaml = "fields:\n  flags:\n    type: array\n    items: boolean\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let field = schema.fields.iter().find(|f| f.name == "flags").unwrap();
        match &field.field_type {
            FieldType::Array { item_type, items } => {
                assert!(items.is_empty());
                assert!(matches!(item_type.as_deref(), Some(FieldType::Boolean)));
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_parse_scalar_array_enum() {
        let yaml = "fields:\n  levels:\n    type: array\n    items: \"enum(a, b, c)\"\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let field = schema.fields.iter().find(|f| f.name == "levels").unwrap();
        match &field.field_type {
            FieldType::Array { item_type, items } => {
                assert!(items.is_empty());
                if let Some(ft) = item_type.as_deref() {
                    if let FieldType::Enum(variants) = ft {
                        assert_eq!(variants, &["a", "b", "c"]);
                    } else {
                        panic!("expected Enum item type");
                    }
                } else {
                    panic!("expected Some item_type");
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_validate_scalar_array() {
        let yaml = "fields:\n  tags:\n    type: array\n    items: string\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let json = "<<<CONDUCTOR_OUTPUT>>>\n{\"tags\": [\"a\", \"b\"]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = parse_structured_output(json, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_scalar_array_rejects_wrong_type() {
        let yaml = "fields:\n  tags:\n    type: array\n    items: string\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let json = "<<<CONDUCTOR_OUTPUT>>>\n{\"tags\": [1, 2]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = parse_structured_output(json, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expected string"));
    }

    #[test]
    fn test_prompt_scalar_array() {
        let yaml = "fields:\n  tags:\n    type: array\n    items: string\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let prompt = generate_prompt_instructions(&schema);
        assert!(prompt.contains("[\"...\", \"...\"]"));
    }

    #[test]
    fn test_validate_enum_scalar_array_valid() {
        let yaml = "fields:\n  status:\n    type: array\n    items: \"enum(a,b)\"\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let json = "<<<CONDUCTOR_OUTPUT>>>\n{\"status\": [\"a\"]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = parse_structured_output(json, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_enum_scalar_array_rejects_invalid_value() {
        let yaml = "fields:\n  status:\n    type: array\n    items: \"enum(a,b)\"\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let json = "<<<CONDUCTOR_OUTPUT>>>\n{\"status\": [\"c\"]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = parse_structured_output(json, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("is not one of"));
    }

    #[test]
    fn test_validate_enum_scalar_array_rejects_wrong_type() {
        let yaml = "fields:\n  status:\n    type: array\n    items: \"enum(a,b)\"\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let json = "<<<CONDUCTOR_OUTPUT>>>\n{\"status\": [123]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = parse_structured_output(json, &schema);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("expected enum string"));
    }

    #[test]
    fn test_mixed_schema_scalar_and_object_arrays() {
        let yaml = r#"
fields:
  tags:
    type: array
    items: string
  findings:
    type: array
    items:
      file: string
      line: number
  summary: string
"#;
        let schema = parse_schema_content(yaml, "test").unwrap();
        assert_eq!(schema.fields.len(), 3);

        let tags = schema.fields.iter().find(|f| f.name == "tags").unwrap();
        match &tags.field_type {
            FieldType::Array { item_type, items } => {
                assert!(matches!(item_type.as_deref(), Some(FieldType::String)));
                assert!(items.is_empty());
            }
            _ => panic!("expected Array for tags"),
        }

        let findings = schema.fields.iter().find(|f| f.name == "findings").unwrap();
        match &findings.field_type {
            FieldType::Array { item_type, items } => {
                assert!(item_type.is_none());
                assert_eq!(items.len(), 2);
            }
            _ => panic!("expected Array for findings"),
        }
    }

    #[test]
    fn test_hints_scalar_array() {
        let yaml = r#"
fields:
  tags:
    type: array
    items: string
    desc: "list of labels"
"#;
        let schema = parse_schema_content(yaml, "test").unwrap();
        let hints = generate_field_hints(&schema.fields, "");
        assert!(hints.contains("array of string"));
        assert!(hints.contains("list of labels"));
    }

    #[test]
    fn test_validate_scalar_array_number() {
        let yaml = "fields:\n  scores:\n    type: array\n    items: number\n";
        let schema = parse_schema_content(yaml, "test").unwrap();

        let ok = "<<<CONDUCTOR_OUTPUT>>>\n{\"scores\": [1, 2.5, 3]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        assert!(parse_structured_output(ok, &schema).is_ok());

        let bad = "<<<CONDUCTOR_OUTPUT>>>\n{\"scores\": [\"nope\"]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let err = parse_structured_output(bad, &schema)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected number"), "got: {err}");
    }

    #[test]
    fn test_validate_scalar_array_boolean() {
        let yaml = "fields:\n  flags:\n    type: array\n    items: boolean\n";
        let schema = parse_schema_content(yaml, "test").unwrap();

        let ok = "<<<CONDUCTOR_OUTPUT>>>\n{\"flags\": [true, false]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        assert!(parse_structured_output(ok, &schema).is_ok());

        let bad = "<<<CONDUCTOR_OUTPUT>>>\n{\"flags\": [\"yes\"]}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let err = parse_structured_output(bad, &schema)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected boolean"), "got: {err}");
    }

    #[test]
    fn test_hints_scalar_array_no_desc() {
        let yaml = "fields:\n  tags:\n    type: array\n    items: string\n";
        let schema = parse_schema_content(yaml, "test").unwrap();
        let hints = generate_field_hints(&schema.fields, "");
        assert!(hints.contains("array of string"), "got: {hints}");
        // Without a desc, the hint should NOT contain a colon-separated description
        assert!(!hints.contains("list of"), "got: {hints}");
    }

    /// Regression: when a field value contains the start marker string, the real block is still found.
    #[test]
    fn test_parse_structured_output_marker_in_field_value() {
        let schema_yaml = r#"
fields:
  summary: string
  description: string
"#;
        let schema = parse_schema_content(schema_yaml, "test").unwrap();

        // The description field value contains <<<CONDUCTOR_OUTPUT>>> — rfind would
        // have selected that inner occurrence as the block start, causing a parse failure.
        let text = r#"Some preamble text.
<<<CONDUCTOR_OUTPUT>>>
{
  "summary": "all good",
  "description": "output block looks like <<<CONDUCTOR_OUTPUT>>> but is inside JSON"
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_structured_output(text, &schema).unwrap();
        assert_eq!(result.context, "all good");
    }
}
