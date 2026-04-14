use crate::error::{ConductorError, Result};

use super::types::{ArrayItems, FieldDef, FieldType, OutputSchema};

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
pub fn find_conductor_output_start(text: &str, marker: &str) -> Option<usize> {
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
pub fn extract_output_block(text: &str) -> Option<String> {
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
    let cleaned = extract_output_block(text).ok_or_else(|| {
        ConductorError::Schema("No <<<CONDUCTOR_OUTPUT>>> block found in agent output".to_string())
    })?;

    // Strip trailing commas (common LLM artifact)
    let cleaned = strip_trailing_commas(&cleaned);
    // Fix invalid backslash escapes (e.g. Swift key-paths, regex, Windows paths)
    let cleaned = fix_invalid_backslash_escapes(&cleaned);

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

/// Derive [`StructuredOutput`] from a pre-validated [`serde_json::Value`].
///
/// This is used by the direct API execution path (see `api_call.rs`) where the
/// Anthropic API has already enforced schema conformance via `tool_use`. There is
/// no `<<<CONDUCTOR_OUTPUT>>>` block to extract and no JSON validation step needed —
/// the value is already a clean, schema-conformant JSON object.
pub fn derive_output_from_value(
    value: serde_json::Value,
    schema: &OutputSchema,
) -> StructuredOutput {
    let markers = derive_markers(&value, schema);

    let context = value
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let json_string = serde_json::to_string(&value)
        .expect("re-serializing a valid serde_json::Value should never fail");

    StructuredOutput {
        value,
        markers,
        context,
        json_string,
    }
}

/// Strip markdown code fences (```json ... ```) from the output.
pub fn strip_code_fences(s: &str) -> String {
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

/// Fix invalid backslash escapes inside JSON string literals.
///
/// Walks the input character-by-character, tracking JSON string boundaries.
/// When inside a string, a `\` followed by an invalid JSON escape character
/// (`"`, `\`, `/`, `b`, `f`, `n`, `r`, `t`, `u` are the valid ones) is
/// doubled to `\\`, making it a valid JSON escaped backslash.  Valid escape
/// sequences (including `\\`, `\"`, `\uXXXX`) are emitted verbatim.
///
/// Backslashes outside string literals are passed through unchanged.
pub(crate) fn fix_invalid_backslash_escapes(s: &str) -> String {
    const VALID_ESCAPE: &[char] = &['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

    let mut chars = s.chars().peekable();
    let mut result = String::with_capacity(s.len() + 16);
    let mut in_string = false;

    while let Some(c) = chars.next() {
        if !in_string {
            result.push(c);
            if c == '"' {
                in_string = true;
            }
        } else {
            match c {
                '"' => {
                    // Closing quote — exit string
                    result.push(c);
                    in_string = false;
                }
                '\\' => {
                    if chars.peek().is_some_and(|nc| VALID_ESCAPE.contains(nc)) {
                        // Valid escape sequence — emit both chars as a unit and advance past them.
                        // Advancing past the escaped char (e.g. `"` in `\"`) is critical: it
                        // prevents the escaped `"` from being misinterpreted as a string boundary.
                        result.push('\\');
                        result.push(chars.next().unwrap());
                    } else {
                        // Invalid escape — double the backslash to make it a literal `\`
                        result.push('\\');
                        result.push('\\');
                    }
                }
                _ => {
                    result.push(c);
                }
            }
        }
    }
    result
}

/// Remove trailing commas before `}` or `]` (common LLM artifact).
pub(crate) fn strip_trailing_commas(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            // Collect any whitespace between the comma and the next non-ws char
            let mut ws_buf = String::new();
            while chars.peek().is_some_and(|p| p.is_whitespace()) {
                ws_buf.push(chars.next().unwrap());
            }
            // If next non-ws char is a closing bracket, drop the comma but keep whitespace
            if chars.peek().is_some_and(|p| *p == '}' || *p == ']') {
                result.push_str(&ws_buf);
                continue;
            }
            // Otherwise keep the comma and the whitespace
            result.push(c);
            result.push_str(&ws_buf);
        } else {
            result.push(c);
        }
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
        FieldType::Array { items } => {
            let arr = value.as_array().ok_or_else(|| {
                ConductorError::Schema(format!(
                    "Field '{}' expected array, got {}",
                    field.name,
                    json_type_name(value)
                ))
            })?;
            match items {
                ArrayItems::Scalar(ft) => {
                    // Scalar-typed array: validate each element against the scalar type.
                    // Hoist the FieldDef outside the loop so we allocate O(1)
                    // instead of O(n) for enum variants.
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
                }
                ArrayItems::Object(sub_fields) if !sub_fields.is_empty() => {
                    for (i, elem) in arr.iter().enumerate() {
                        validate_value(elem, sub_fields).map_err(|e| {
                            ConductorError::Schema(format!("In '{}[{}]': {e}", field.name, i))
                        })?;
                    }
                }
                _ => {}
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
pub(crate) fn evaluate_marker_expr(value: &serde_json::Value, expr: &str) -> bool {
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
pub(crate) fn derive_default_markers(value: &serde_json::Value) -> Vec<String> {
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
