use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConductorError, Result};

use super::types::{ArrayItems, FieldDef, FieldType, OutputSchema};

// ---------------------------------------------------------------------------
// YAML deserialization intermediates
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
// Loading and resolution
// ---------------------------------------------------------------------------

/// Load a schema by reference.
pub fn load_schema(
    worktree_path: &str,
    repo_path: &str,
    schema_ref: &super::types::SchemaRef,
    workflow_name: Option<&str>,
) -> Result<OutputSchema> {
    match schema_ref {
        super::types::SchemaRef::Name(name) => {
            load_schema_by_name(worktree_path, repo_path, name, workflow_name)
        }
        super::types::SchemaRef::Path(rel_path) => load_schema_by_path(repo_path, rel_path),
    }
}

/// Check that a schema or workflow name does not contain path traversal components.
pub(super) fn validate_name_segment(name: &str, label: &str) -> Result<()> {
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
pub(crate) fn load_schema_by_path(repo_path: &str, rel_path: &str) -> Result<OutputSchema> {
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
pub(super) fn find_schema_path(bases: &[&str], subdir: &Path, filename: &str) -> Option<PathBuf> {
    bases.iter().find_map(|base| {
        let path = PathBuf::from(base).join(subdir).join(filename);
        path.is_file().then_some(path)
    })
}

/// Parse a schema YAML file into an `OutputSchema`.
pub(super) fn parse_schema_file(path: &Path) -> Result<OutputSchema> {
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

pub(super) fn parse_raw_fields(raw: &HashMap<String, RawFieldDef>) -> Result<Vec<FieldDef>> {
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
                        if matches!(scalar, FieldType::Array { .. } | FieldType::Object { .. }) {
                            return Err(ConductorError::Schema(format!(
                                "Field '{name}': array items type must be a scalar (string, number, boolean, enum), got '{type_str}'"
                            )));
                        }
                        FieldType::Array {
                            items: ArrayItems::Scalar(Box::new(scalar)),
                        }
                    }
                    Some(RawArrayItems::Object(items_map)) => FieldType::Array {
                        items: ArrayItems::Object(parse_raw_fields(items_map)?),
                    },
                    None => FieldType::Array {
                        items: ArrayItems::Untyped,
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
            items: ArrayItems::Untyped,
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
