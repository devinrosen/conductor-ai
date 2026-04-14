//! Convert an [`OutputSchema`] into the Claude API `tools` array entry JSON.
//!
//! The resulting JSON is passed directly to the Anthropic Messages API with
//! `tool_choice: {type: "tool", name: schema.name}` to enforce structured output.

use super::types::{ArrayItems, FieldDef, FieldType, OutputSchema};

/// Convert an [`OutputSchema`] into a Claude API tool definition JSON value.
///
/// The returned value matches the shape required by the Anthropic Messages API:
/// ```json
/// {
///   "name": "schema_name",
///   "description": "Structured output for this workflow step",
///   "input_schema": {
///     "type": "object",
///     "properties": { ... },
///     "required": ["field1", "field2"]
///   }
/// }
/// ```
pub fn schema_to_tool_json(schema: &OutputSchema) -> serde_json::Value {
    let (properties, required) = fields_to_json_schema(&schema.fields);

    serde_json::json!({
        "name": schema.name,
        "description": "Structured output for this workflow step",
        "input_schema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

/// Convert a list of field definitions into a JSON Schema `properties` object
/// and a `required` array.
fn fields_to_json_schema(
    fields: &[FieldDef],
) -> (serde_json::Map<String, serde_json::Value>, Vec<String>) {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for field in fields {
        let mut type_schema = field_type_to_schema(&field.field_type);

        // Attach description if present
        if let Some(ref desc) = field.desc {
            if let Some(obj) = type_schema.as_object_mut() {
                obj.insert(
                    "description".to_string(),
                    serde_json::Value::String(desc.clone()),
                );
            }
        }

        properties.insert(field.name.clone(), type_schema);

        if field.required {
            required.push(field.name.clone());
        }
    }

    (properties, required)
}

/// Convert a [`FieldType`] into its JSON Schema representation.
fn field_type_to_schema(field_type: &FieldType) -> serde_json::Value {
    match field_type {
        FieldType::String => serde_json::json!({"type": "string"}),
        FieldType::Number => serde_json::json!({"type": "number"}),
        FieldType::Boolean => serde_json::json!({"type": "boolean"}),
        FieldType::Enum(variants) => {
            serde_json::json!({"type": "string", "enum": variants})
        }
        FieldType::Array { items } => match items {
            ArrayItems::Scalar(scalar_type) => {
                let items_schema = field_type_to_schema(scalar_type);
                serde_json::json!({"type": "array", "items": items_schema})
            }
            ArrayItems::Object(sub_fields) => {
                let (properties, required) = fields_to_json_schema(sub_fields);
                serde_json::json!({
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": properties,
                        "required": required
                    }
                })
            }
            ArrayItems::Untyped => serde_json::json!({"type": "array"}),
        },
        FieldType::Object { fields } => {
            let (properties, required) = fields_to_json_schema(fields);
            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_config::{ArrayItems, FieldDef, FieldType, OutputSchema};

    fn make_field(name: &str, required: bool, field_type: FieldType) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            required,
            field_type,
            desc: None,
            examples: None,
        }
    }

    fn make_field_with_desc(
        name: &str,
        required: bool,
        field_type: FieldType,
        desc: &str,
    ) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            required,
            field_type,
            desc: Some(desc.to_string()),
            examples: None,
        }
    }

    #[test]
    fn test_string_field() {
        let schema = OutputSchema {
            name: "test-schema".to_string(),
            fields: vec![make_field("summary", true, FieldType::String)],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        assert_eq!(tool["name"], "test-schema");
        assert_eq!(
            tool["input_schema"]["properties"]["summary"]["type"],
            "string"
        );
        assert_eq!(tool["input_schema"]["required"][0], "summary");
    }

    #[test]
    fn test_number_field() {
        let schema = OutputSchema {
            name: "score-schema".to_string(),
            fields: vec![make_field("score", true, FieldType::Number)],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        assert_eq!(
            tool["input_schema"]["properties"]["score"]["type"],
            "number"
        );
    }

    #[test]
    fn test_boolean_field() {
        let schema = OutputSchema {
            name: "approval-schema".to_string(),
            fields: vec![make_field("approved", true, FieldType::Boolean)],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        assert_eq!(
            tool["input_schema"]["properties"]["approved"]["type"],
            "boolean"
        );
    }

    #[test]
    fn test_enum_field() {
        let schema = OutputSchema {
            name: "severity-schema".to_string(),
            fields: vec![make_field(
                "severity",
                true,
                FieldType::Enum(vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ]),
            )],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        let props = &tool["input_schema"]["properties"]["severity"];
        assert_eq!(props["type"], "string");
        assert_eq!(props["enum"][0], "low");
        assert_eq!(props["enum"][2], "high");
    }

    #[test]
    fn test_array_scalar() {
        let schema = OutputSchema {
            name: "list-schema".to_string(),
            fields: vec![make_field(
                "tags",
                false,
                FieldType::Array {
                    items: ArrayItems::Scalar(Box::new(FieldType::String)),
                },
            )],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        let tags = &tool["input_schema"]["properties"]["tags"];
        assert_eq!(tags["type"], "array");
        assert_eq!(tags["items"]["type"], "string");
        // Optional field not in required
        let required = tool["input_schema"]["required"].as_array().unwrap();
        assert!(!required.iter().any(|v| v == "tags"));
    }

    #[test]
    fn test_array_object() {
        let schema = OutputSchema {
            name: "findings-schema".to_string(),
            fields: vec![make_field(
                "findings",
                true,
                FieldType::Array {
                    items: ArrayItems::Object(vec![
                        make_field("title", true, FieldType::String),
                        make_field(
                            "severity",
                            true,
                            FieldType::Enum(vec!["low".to_string(), "high".to_string()]),
                        ),
                    ]),
                },
            )],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        let findings = &tool["input_schema"]["properties"]["findings"];
        assert_eq!(findings["type"], "array");
        assert_eq!(findings["items"]["type"], "object");
        assert_eq!(findings["items"]["properties"]["title"]["type"], "string");
    }

    #[test]
    fn test_array_untyped() {
        let schema = OutputSchema {
            name: "untyped-schema".to_string(),
            fields: vec![make_field(
                "items",
                false,
                FieldType::Array {
                    items: ArrayItems::Untyped,
                },
            )],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        let items = &tool["input_schema"]["properties"]["items"];
        assert_eq!(items["type"], "array");
        assert!(items.get("items").is_none() || items["items"].is_null());
    }

    #[test]
    fn test_object_field() {
        let schema = OutputSchema {
            name: "nested-schema".to_string(),
            fields: vec![make_field(
                "metadata",
                true,
                FieldType::Object {
                    fields: vec![make_field("key", true, FieldType::String)],
                },
            )],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        let meta = &tool["input_schema"]["properties"]["metadata"];
        assert_eq!(meta["type"], "object");
        assert_eq!(meta["properties"]["key"]["type"], "string");
    }

    #[test]
    fn test_description_attached() {
        let schema = OutputSchema {
            name: "desc-schema".to_string(),
            fields: vec![make_field_with_desc(
                "summary",
                true,
                FieldType::String,
                "A brief summary of findings",
            )],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        assert_eq!(
            tool["input_schema"]["properties"]["summary"]["description"],
            "A brief summary of findings"
        );
    }

    #[test]
    fn test_required_vs_optional() {
        let schema = OutputSchema {
            name: "mixed-schema".to_string(),
            fields: vec![
                make_field("required_field", true, FieldType::String),
                make_field("optional_field", false, FieldType::Number),
            ],
            markers: None,
        };
        let tool = schema_to_tool_json(&schema);
        let required = tool["input_schema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "required_field"));
        assert!(!required.iter().any(|v| v == "optional_field"));
    }
}
