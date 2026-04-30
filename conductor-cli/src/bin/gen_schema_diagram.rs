//! gen-schema-diagram: Regenerate docs/diagrams/database-schema.mmd from live migrations.
//!
//! Opens an in-memory SQLite database, applies all .sql migration files in
//! filename-sorted order, then introspects the schema via PRAGMA to build
//! Mermaid erDiagram entity blocks. The relationship section (lines starting
//! with `||--`) is preserved verbatim from the existing file.
//!
//! Usage:
//!   cargo run --bin gen-schema-diagram
//!   cargo run --bin gen-schema-diagram -- --migrations-dir path/to/migrations --output path/to/file.mmd

use rusqlite::{Connection, Result as SqliteResult};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Columns whose SQLite type is INTEGER but represent boolean values.
/// SQLite has no native BOOL type; booleans are stored as INTEGER 0/1.
/// PRAGMA table_info cannot distinguish them from numeric integers, so
/// we maintain this hard-coded list. Add new names here when migrations
/// introduce new boolean columns.
const BOOL_COLUMNS: &[&str] = &[
    "allow_agent_issue_creation",
    "dry_run",
    "can_commit",
    "condition_met",
    "read",
];

fn map_type(sqlite_type: &str, col_name: &str) -> &'static str {
    // Check bool list first (before the INTEGER check)
    if BOOL_COLUMNS.contains(&col_name) {
        return "bool";
    }
    let upper = sqlite_type.to_uppercase();
    if upper.starts_with("TEXT") || upper.starts_with("VARCHAR") || upper.starts_with("CHAR") {
        return "text";
    }
    if upper.starts_with("INTEGER") || upper.starts_with("INT") {
        return "int";
    }
    if upper.starts_with("REAL") || upper.starts_with("FLOAT") || upper.starts_with("DOUBLE") {
        return "real";
    }
    if upper.starts_with("BLOB") {
        return "blob";
    }
    // Unknown or empty type defaults to text
    "text"
}

fn apply_migrations(conn: &Connection, migrations_dir: &Path) -> SqliteResult<usize> {
    // Enable foreign keys (matches production config)
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    // Bootstrap the meta table that the production runner creates before applying
    // versioned .sql migrations. Without this, any migration that references
    // _conductor_meta would fail.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _conductor_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;

    let mut entries: Vec<_> = fs::read_dir(migrations_dir)
        .expect("Failed to read migrations directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "sql").unwrap_or(false))
        .collect();

    // Sort by filename (matches the migration runner's lexicographic order)
    entries.sort_by_key(|e| e.file_name());

    let count = entries.len();
    for entry in entries {
        let sql = fs::read_to_string(entry.path())
            .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", entry.path(), e));
        conn.execute_batch(&sql)
            .unwrap_or_else(|e| panic!("Failed to apply {:?}: {}", entry.path(), e));
    }

    Ok(count)
}

fn list_tables(conn: &Connection) -> SqliteResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names: SqliteResult<Vec<String>> = stmt.query_map([], |row| row.get(0))?.collect();
    names
}

struct ColumnInfo {
    name: String,
    col_type: String,
    is_pk: bool,
    is_fk: bool,
}

fn get_columns(conn: &Connection, table: &str) -> SqliteResult<Vec<ColumnInfo>> {
    // Collect FK columns
    let mut fk_stmt = conn.prepare(&format!("PRAGMA foreign_key_list(\"{}\")", table))?;
    let fk_cols: HashSet<String> = fk_stmt
        .query_map([], |row| row.get::<_, String>(3))? // column "from"
        .filter_map(|r| r.ok())
        .collect();

    // Collect column info
    let mut info_stmt = conn.prepare(&format!("PRAGMA table_info(\"{}\")", table))?;
    let cols: SqliteResult<Vec<ColumnInfo>> = info_stmt
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let col_type: String = row.get(2)?;
            let pk: i32 = row.get(5)?;
            Ok((name, col_type, pk))
        })?
        .map(|r| {
            r.map(|(name, col_type, pk)| {
                let is_fk = fk_cols.contains(&name);
                ColumnInfo {
                    name,
                    col_type,
                    is_pk: pk > 0,
                    is_fk,
                }
            })
        })
        .collect();
    cols
}

fn build_entity_block(table: &str, columns: &[ColumnInfo]) -> String {
    let mut lines = Vec::new();
    lines.push(format!("    {} {{", table));
    for col in columns {
        let mmd_type = map_type(&col.col_type, &col.name);
        // FK takes precedence over PK: a column in a composite PK that is also a
        // foreign key is most usefully annotated FK for the relationship diagram.
        // Pure PKs (with no FK relationship) are annotated PK.
        let annotation = if col.is_fk {
            " FK"
        } else if col.is_pk {
            " PK"
        } else {
            ""
        };
        lines.push(format!("        {} {}{}", mmd_type, col.name, annotation));
    }
    lines.push("    }".to_string());
    lines.join("\n")
}

/// Split the existing mmd file into (header+entity_section, relationship_section).
/// The relationship section starts at the first line matching `||--`.
fn split_mmd(content: &str) -> (&str, &str) {
    for (i, line) in content.lines().enumerate() {
        if line.contains("||--") {
            // Find the byte offset of this line
            let offset: usize = content
                .lines()
                .take(i)
                .map(|l| l.len() + 1) // +1 for '\n'
                .sum();
            return (&content[..offset], &content[offset..]);
        }
    }
    // No relationship section found — keep everything as entity section
    (content, "")
}

fn main() {
    // Parse CLI args
    let args: Vec<String> = std::env::args().collect();
    let mut migrations_dir = String::from("conductor-core/src/db/migrations");
    let mut output = String::from("docs/diagrams/database-schema.mmd");

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--migrations-dir" => {
                i += 1;
                migrations_dir = args[i].clone();
            }
            "--output" => {
                i += 1;
                output = args[i].clone();
            }
            _ => {}
        }
        i += 1;
    }

    let migrations_path = Path::new(&migrations_dir);
    let output_path = Path::new(&output);

    // Apply migrations to in-memory SQLite
    let conn = Connection::open_in_memory().expect("Failed to open in-memory SQLite");
    let migration_count =
        apply_migrations(&conn, migrations_path).expect("Failed to apply migrations");

    eprintln!("Applied {} migrations", migration_count);

    // Introspect schema
    let tables = list_tables(&conn).expect("Failed to list tables");
    eprintln!("Found {} tables", tables.len());

    // Build entity blocks
    let mut entity_blocks = Vec::new();
    for table in &tables {
        let columns = get_columns(&conn, table).expect("Failed to get columns");
        entity_blocks.push(build_entity_block(table, &columns));
    }

    // Read existing file to preserve relationship section
    let existing = fs::read_to_string(output_path).unwrap_or_else(|_| String::new());

    let (_, relationship_section) = split_mmd(&existing);

    // Build updated date
    let today = {
        // Use a simple date via env or derive from build time.
        // We use the SOURCE_DATE_EPOCH env var for reproducibility in CI,
        // falling back to the system date via a simple epoch calculation.
        std::env::var("DIAGRAM_DATE").unwrap_or_else(|_| {
            // Derive from SystemTime
            use std::time::{SystemTime, UNIX_EPOCH};
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Simple Gregorian date calculation
            let days = secs / 86400;
            // Days since 1970-01-01
            let (y, m, d) = days_to_ymd(days);
            format!("{:04}-{:02}-{:02}", y, m, d)
        })
    };

    // Compose output
    let header = format!(
        "%% Updated {} — Entity-relationship diagram for conductor.db ({} migrations)\nerDiagram",
        today, migration_count
    );

    let mut output_lines = vec![header];
    for block in &entity_blocks {
        output_lines.push(String::new());
        output_lines.push(block.clone());
    }
    output_lines.push(String::new()); // blank line before relationships

    let mut content = output_lines.join("\n");
    if !relationship_section.is_empty() {
        content.push_str(relationship_section);
    }

    // Ensure file ends with a newline
    if !content.ends_with('\n') {
        content.push('\n');
    }

    fs::write(output_path, &content)
        .unwrap_or_else(|e| panic!("Failed to write {:?}: {}", output_path, e));

    eprintln!("Wrote {}", output_path.display());
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
