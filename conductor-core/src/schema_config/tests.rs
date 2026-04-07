use super::*;
use std::fs;

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
    if let FieldType::Array {
        items: ArrayItems::Object(items),
    } = &findings.field_type
    {
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
    let value: serde_json::Value =
        serde_json::from_str(r#"{"findings": [{"severity": "critical"}, {"severity": "high"}]}"#)
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
        parse_schema_content("fields:\n  approved: boolean\n  summary: string\n", "test").unwrap();
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
        load_schema_by_path(repo.path().to_str().unwrap(), "custom/schemas/review.yaml").unwrap();
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
    assert!(result.unwrap_err().to_string().contains("CONDUCTOR_OUTPUT"));
}

#[test]
fn test_malformed_marker_expressions_return_false() {
    let value: serde_json::Value = serde_json::from_str(r#"{"name": "test", "count": 5}"#).unwrap();

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
        FieldType::Array {
            items: ArrayItems::Scalar(ft),
        } => {
            assert!(matches!(ft.as_ref(), &FieldType::String));
        }
        _ => panic!("expected Array with Scalar items"),
    }
}

#[test]
fn test_parse_scalar_array_number() {
    let yaml = "fields:\n  scores:\n    type: array\n    items: number\n";
    let schema = parse_schema_content(yaml, "test").unwrap();
    let field = schema.fields.iter().find(|f| f.name == "scores").unwrap();
    match &field.field_type {
        FieldType::Array {
            items: ArrayItems::Scalar(ft),
        } => {
            assert!(matches!(ft.as_ref(), &FieldType::Number));
        }
        _ => panic!("expected Array with Scalar items"),
    }
}

#[test]
fn test_parse_scalar_array_boolean() {
    let yaml = "fields:\n  flags:\n    type: array\n    items: boolean\n";
    let schema = parse_schema_content(yaml, "test").unwrap();
    let field = schema.fields.iter().find(|f| f.name == "flags").unwrap();
    match &field.field_type {
        FieldType::Array {
            items: ArrayItems::Scalar(ft),
        } => {
            assert!(matches!(ft.as_ref(), &FieldType::Boolean));
        }
        _ => panic!("expected Array with Scalar items"),
    }
}

#[test]
fn test_parse_scalar_array_enum() {
    let yaml = "fields:\n  levels:\n    type: array\n    items: \"enum(a, b, c)\"\n";
    let schema = parse_schema_content(yaml, "test").unwrap();
    let field = schema.fields.iter().find(|f| f.name == "levels").unwrap();
    match &field.field_type {
        FieldType::Array {
            items: ArrayItems::Scalar(ft),
        } => {
            if let FieldType::Enum(variants) = ft.as_ref() {
                assert_eq!(variants, &["a", "b", "c"]);
            } else {
                panic!("expected Enum item type");
            }
        }
        _ => panic!("expected Array with Scalar items"),
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
        FieldType::Array {
            items: ArrayItems::Scalar(ft),
        } => {
            assert!(matches!(ft.as_ref(), &FieldType::String));
        }
        _ => panic!("expected Array with Scalar items for tags"),
    }

    let findings = schema.fields.iter().find(|f| f.name == "findings").unwrap();
    match &findings.field_type {
        FieldType::Array {
            items: ArrayItems::Object(fields),
        } => {
            assert_eq!(fields.len(), 2);
        }
        _ => panic!("expected Array with Object items for findings"),
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

#[test]
fn test_parse_invalid_scalar_item_type() {
    let yaml = "fields:\n  tags:\n    type: array\n    items: bad_type\n";
    let result = parse_schema_content(yaml, "test");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("Unknown field type"),
        "expected 'Unknown field type' error, got: {msg}"
    );
}

#[test]
fn test_parse_rejects_array_as_scalar_item_type() {
    let yaml = "fields:\n  tags:\n    type: array\n    items: array\n";
    let result = parse_schema_content(yaml, "test");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("must be a scalar"),
        "expected 'must be a scalar' error, got: {msg}"
    );
}

#[test]
fn test_parse_rejects_object_as_scalar_item_type() {
    let yaml = "fields:\n  tags:\n    type: array\n    items: object\n";
    let result = parse_schema_content(yaml, "test");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("must be a scalar"),
        "expected 'must be a scalar' error, got: {msg}"
    );
}

#[test]
fn test_hints_scalar_array_with_examples() {
    let yaml = r#"
fields:
  tags:
    type: array
    items: string
    examples: ["foo", "bar"]
"#;
    let schema = parse_schema_content(yaml, "test").unwrap();
    let hints = generate_field_hints(&schema.fields, "");
    assert!(hints.contains("array of string"), "got: {hints}");
    assert!(
        hints.contains("examples: ["),
        "expected examples line, got: {hints}"
    );
    assert!(hints.contains("\"foo\""), "got: {hints}");
    assert!(hints.contains("\"bar\""), "got: {hints}");
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

#[test]
fn test_example_scalar_array_number() {
    let schema = parse_schema_content(
        "fields:\n  scores:\n    type: array\n    items: number\n",
        "test",
    )
    .unwrap();
    let prompt = generate_prompt_instructions(&schema);
    assert!(prompt.contains("[0, 0]"), "expected number array example");
}

#[test]
fn test_example_scalar_array_boolean() {
    let schema = parse_schema_content(
        "fields:\n  flags:\n    type: array\n    items: boolean\n",
        "test",
    )
    .unwrap();
    let prompt = generate_prompt_instructions(&schema);
    assert!(
        prompt.contains("[true, false]"),
        "expected boolean array example"
    );
}

#[test]
fn test_example_scalar_array_enum() {
    let schema = parse_schema_content(
        "fields:\n  levels:\n    type: array\n    items: \"enum(low, medium, high)\"\n",
        "test",
    )
    .unwrap();
    let prompt = generate_prompt_instructions(&schema);
    assert!(
        prompt.contains("[\"low|medium|high\"]"),
        "expected enum array example"
    );
}
