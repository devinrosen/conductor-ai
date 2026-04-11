use std::path::Path;
use std::sync::Arc;

use rmcp::model::{CallToolResult, Content, RawResource, Resource};
use serde_json::{json, Value};

/// Helper: turn an error into a tool result with `is_error: true`.
pub(crate) fn tool_err(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}

/// Helper: turn a string into a successful tool result.
pub(crate) fn tool_ok(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

/// Helper: build a `Resource` with a URI and human-readable name.
pub(crate) fn make_resource(
    uri: impl Into<String>,
    name: impl Into<String>,
    description: impl Into<String>,
) -> Resource {
    Resource {
        raw: RawResource {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: Some(description.into()),
            mime_type: Some("text/plain".into()),
            size: None,
            icons: None,
            meta: None,
        },
        annotations: None,
    }
}

/// Helper: build a JSON Schema input_schema for a Tool.
/// fields: (name, description, required) — all fields are typed as "string".
pub(crate) fn schema(fields: &[(&str, &str, bool)]) -> Arc<serde_json::Map<String, Value>> {
    schema_typed(
        &fields
            .iter()
            .map(|(n, d, r)| (*n, "string", *d, *r))
            .collect::<Vec<_>>(),
    )
}

/// Like `schema`, but each field includes an explicit JSON Schema type.
/// fields: (name, json_type, description, required)
pub(crate) fn schema_typed(
    fields: &[(&str, &str, &str, bool)],
) -> Arc<serde_json::Map<String, Value>> {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, field_type, desc, req) in fields {
        props.insert(
            name.to_string(),
            json!({ "type": field_type, "description": desc }),
        );
        if *req {
            required.push(Value::String(name.to_string()));
        }
    }
    let mut schema_obj = serde_json::Map::new();
    schema_obj.insert("type".into(), Value::String("object".into()));
    schema_obj.insert("properties".into(), Value::Object(props));
    schema_obj.insert("required".into(), Value::Array(required));
    Arc::new(schema_obj)
}

/// Helper: extract an optional string arg from tool call arguments.
pub(crate) fn get_arg<'a>(args: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

/// Helper: extract an optional usize arg, accepting both JSON number and string representations.
pub(crate) fn get_arg_usize(args: &serde_json::Map<String, Value>, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| {
        if let Some(n) = v.as_u64() {
            Some(n as usize)
        } else {
            v.as_str().and_then(|s| s.parse().ok())
        }
    })
}

/// Macro: extract a required string arg; returns `tool_err` early if missing.
macro_rules! require_arg {
    ($args:expr, $key:literal) => {
        match get_arg($args, $key) {
            Some(s) => s,
            None => return tool_err(concat!("Missing required argument: ", $key)),
        }
    };
}

/// Returns a pagination hint string like "Showing 0–50 (limit 50). Pass offset=50 for more."
/// Call this when the result count equals the page limit, then append the result to your output.
pub(crate) fn pagination_hint(offset: usize, count: usize, limit: usize) -> String {
    format!(
        "Showing {offset}–{end} (limit {limit}). Pass offset={next} for more.",
        end = offset + count,
        next = offset + limit,
    )
}

/// Helper: open the database and load config.
pub(crate) fn open_db_and_config(
    db_path: &Path,
) -> anyhow::Result<(rusqlite::Connection, conductor_core::config::Config)> {
    use conductor_core::config::load_config;
    use conductor_core::db::open_database;
    let conn = open_database(db_path)?;
    let config = load_config()?;
    Ok((conn, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(val: Value) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("limit".into(), val);
        m
    }

    #[test]
    fn get_arg_usize_accepts_json_number() {
        let a = args(json!(10));
        assert_eq!(get_arg_usize(&a, "limit"), Some(10));
    }

    #[test]
    fn get_arg_usize_accepts_string() {
        let a = args(json!("25"));
        assert_eq!(get_arg_usize(&a, "limit"), Some(25));
    }

    #[test]
    fn get_arg_usize_returns_none_for_missing_key() {
        let m = serde_json::Map::new();
        assert_eq!(get_arg_usize(&m, "limit"), None);
    }

    #[test]
    fn get_arg_usize_returns_none_for_invalid_string() {
        let a = args(json!("not_a_number"));
        assert_eq!(get_arg_usize(&a, "limit"), None);
    }
}
