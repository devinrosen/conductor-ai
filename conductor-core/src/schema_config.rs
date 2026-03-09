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
    /// Array of items defined by sub-fields.
    Array {
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

#[derive(Debug, Deserialize)]
struct RawFieldObject {
    #[serde(rename = "type")]
    field_type: Option<String>,
    desc: Option<String>,
    examples: Option<Vec<String>>,
    /// Sub-fields for `array` items.
    items: Option<HashMap<String, RawFieldDef>>,
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
    /// Values containing `/` are treated as explicit paths; otherwise as names.
    pub fn from_str_value(s: &str) -> Self {
        if s.contains('/') {
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

/// Resolve a schema by short name using the search order.
fn load_schema_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Result<OutputSchema> {
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
                "array" => {
                    let items = if let Some(ref items_map) = obj.items {
                        parse_raw_fields(items_map)?
                    } else {
                        Vec::new()
                    };
                    FieldType::Array { items }
                }
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
        Ok(FieldType::Array { items: Vec::new() })
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
        FieldType::Array { items } if items.is_empty() => "[]".to_string(),
        FieldType::Array { items } => {
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
            FieldType::Array { items } if !items.is_empty() => {
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

/// Parse the `<<<CONDUCTOR_OUTPUT>>>` block as structured JSON, validate against
/// the schema, and derive markers.
pub fn parse_structured_output(
    text: &str,
    schema: &OutputSchema,
) -> std::result::Result<StructuredOutput, String> {
    let start_marker = "<<<CONDUCTOR_OUTPUT>>>";
    let end_marker = "<<<END_CONDUCTOR_OUTPUT>>>";

    let start = text
        .rfind(start_marker)
        .ok_or("No <<<CONDUCTOR_OUTPUT>>> block found in agent output")?;
    let json_start = start + start_marker.len();
    let end = text[json_start..]
        .find(end_marker)
        .ok_or("No <<<END_CONDUCTOR_OUTPUT>>> found after start marker")?;
    let raw = text[json_start..json_start + end].trim();

    // Lenient parsing: strip markdown code fences
    let cleaned = strip_code_fences(raw);
    // Strip trailing commas (common LLM artifact)
    let cleaned = strip_trailing_commas(&cleaned);

    let value: serde_json::Value = serde_json::from_str(&cleaned)
        .map_err(|e| format!("Invalid JSON in CONDUCTOR_OUTPUT: {e}"))?;

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

    let json_string = serde_json::to_string(&value).unwrap_or_default();

    Ok(StructuredOutput {
        value,
        markers,
        context,
        json_string,
    })
}

/// Strip markdown code fences (```json ... ```) from the output.
fn strip_code_fences(s: &str) -> String {
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

fn validate_value(
    value: &serde_json::Value,
    fields: &[FieldDef],
) -> std::result::Result<(), String> {
    let obj = value
        .as_object()
        .ok_or("CONDUCTOR_OUTPUT must be a JSON object")?;

    for field in fields {
        match obj.get(&field.name) {
            None if field.required => {
                return Err(format!("Missing required field: '{}'", field.name));
            }
            None => continue,
            Some(val) => validate_field_value(val, field)?,
        }
    }

    Ok(())
}

fn validate_field_value(
    value: &serde_json::Value,
    field: &FieldDef,
) -> std::result::Result<(), String> {
    match &field.field_type {
        FieldType::String => {
            if !value.is_string() {
                return Err(format!(
                    "Field '{}' expected string, got {}",
                    field.name,
                    json_type_name(value)
                ));
            }
        }
        FieldType::Number => {
            if !value.is_number() {
                return Err(format!(
                    "Field '{}' expected number, got {}",
                    field.name,
                    json_type_name(value)
                ));
            }
        }
        FieldType::Boolean => {
            if !value.is_boolean() {
                return Err(format!(
                    "Field '{}' expected boolean, got {}",
                    field.name,
                    json_type_name(value)
                ));
            }
        }
        FieldType::Enum(variants) => {
            let s = value.as_str().ok_or_else(|| {
                format!(
                    "Field '{}' expected enum string, got {}",
                    field.name,
                    json_type_name(value)
                )
            })?;
            if !variants.contains(&s.to_string()) {
                return Err(format!(
                    "Field '{}' value '{}' is not one of: {}",
                    field.name,
                    s,
                    variants.join(", ")
                ));
            }
        }
        FieldType::Array { items } => {
            let arr = value.as_array().ok_or_else(|| {
                format!(
                    "Field '{}' expected array, got {}",
                    field.name,
                    json_type_name(value)
                )
            })?;
            if !items.is_empty() {
                for (i, elem) in arr.iter().enumerate() {
                    validate_value(elem, items)
                        .map_err(|e| format!("In '{}[{}]': {e}", field.name, i))?;
                }
            }
        }
        FieldType::Object { fields } => {
            if !value.is_object() {
                return Err(format!(
                    "Field '{}' expected object, got {}",
                    field.name,
                    json_type_name(value)
                ));
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
        if let FieldType::Array { items } = &findings.field_type {
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
        assert!(result.unwrap_err().contains("approved"));
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
        assert!(result.unwrap_err().contains("approved"));
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
        assert!(result.unwrap_err().contains("extreme"));
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
}
