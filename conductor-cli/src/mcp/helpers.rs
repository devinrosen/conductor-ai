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
/// fields: (name, description, required)
pub(crate) fn schema(fields: &[(&str, &str, bool)]) -> Arc<serde_json::Map<String, Value>> {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, desc, req) in fields {
        props.insert(
            name.to_string(),
            json!({ "type": "string", "description": desc }),
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

/// Macro: extract a required string arg; returns `tool_err` early if missing.
macro_rules! require_arg {
    ($args:expr, $key:literal) => {
        match get_arg($args, $key) {
            Some(s) => s,
            None => return tool_err(concat!("Missing required argument: ", $key)),
        }
    };
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
