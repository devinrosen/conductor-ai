use super::types::{ArrayItems, FieldDef, FieldType, OutputSchema};

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
            items: ArrayItems::Scalar(ft),
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
        FieldType::Array {
            items: ArrayItems::Object(fields),
        } if fields.is_empty() => "[]".to_string(),
        FieldType::Array {
            items: ArrayItems::Object(fields),
        } => {
            let item_json = generate_json_example(fields, indent + 1);
            format!("[\n{item_json}\n{inner_pad}]")
        }
        FieldType::Array {
            items: ArrayItems::Untyped,
        } => "[]".to_string(),
        FieldType::Object { fields } if fields.is_empty() => "{}".to_string(),
        FieldType::Object { fields } => generate_json_example(fields, indent),
    }
}

pub(crate) fn generate_field_hints(fields: &[FieldDef], prefix: &str) -> String {
    let mut hints = Vec::new();
    for field in fields {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{prefix}.{}", field.name)
        };

        let optional_tag = if !field.required { " (optional)" } else { "" };

        let push_examples = |hints: &mut Vec<std::string::String>, field: &FieldDef| {
            if let Some(ref examples) = field.examples {
                let examples_str = examples
                    .iter()
                    .map(|e| format!("\"{e}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                hints.push(format!("  examples: [{examples_str}]"));
            }
        };

        match &field.field_type {
            FieldType::Array {
                items: ArrayItems::Scalar(ft),
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
                push_examples(&mut hints, field);
            }
            FieldType::Array {
                items: ArrayItems::Object(sub_fields),
            } if !sub_fields.is_empty() => {
                if let Some(ref desc) = field.desc {
                    hints.push(format!("\"{full_name}\"{optional_tag}: {desc}"));
                }
                let sub_hints = generate_field_hints(sub_fields, &format!("{full_name}[]"));
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
                push_examples(&mut hints, field);
                if field.desc.is_none() && !field.required {
                    hints.push(format!("\"{full_name}\" is optional and may be omitted."));
                }
            }
        }
    }
    hints.join("\n")
}
