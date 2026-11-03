//! Pure parser for Claude Code session transcripts (RFC 021 §9, #3165).
//!
//! The transcript is internal to Claude Code and undocumented, so everything here is
//! defensive: unknown entry types, missing fields and non-JSON lines are skipped and
//! counted, never reported as errors. No database access.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

/// `toolUseResult` of the user entry written when a permission prompt is denied.
pub(crate) const TOOL_REJECTED_RESULT: &str = "User rejected tool use";
/// Prefix of the user text entry written when the developer interrupts (Esc).
pub(crate) const INTERRUPT_PREFIX: &str = "[Request interrupted by user";

/// Upper bound on `parentUuid` hops when resolving an entry's prompt.
const MAX_CHAIN_STEPS: usize = 10_000;
/// Block size used to skip over a single line larger than the read budget.
const SKIP_BLOCK: usize = 64 * 1024;

/// New bytes read from a transcript, ending at a line boundary.
#[derive(Debug, Default)]
pub(crate) struct Chunk {
    pub bytes: Vec<u8>,
    /// Offset the read began at (after a truncation reset this may be 0).
    #[cfg_attr(not(test), allow(dead_code))] // asserted in tests; callers only need `end`
    pub start: u64,
    /// Offset just past the last consumed byte. Equals `start` when there is nothing to
    /// consume yet (no new data, or only a partial last line).
    pub end: u64,
}

/// Read from `offset` up to the last complete line, at most `max_bytes`.
///
/// - A file shorter than `offset` was truncated or replaced: restart from 0.
/// - A full buffer with no newline is one oversize line (for example `api-request-blob`):
///   skip it so it can never wedge the reader. The returned `bytes` are empty and `end`
///   is past that line. If EOF arrives before its newline the line is still being
///   written, so `end == start`.
pub(crate) fn read_chunk(path: &Path, offset: u64, max_bytes: usize) -> io::Result<Chunk> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = if len < offset {
        tracing::debug!(path = %path.display(), offset, len, "transcript shrank; restarting at 0");
        0
    } else {
        offset
    };
    if len == start {
        return Ok(Chunk {
            bytes: Vec::new(),
            start,
            end: start,
        });
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    (&mut file).take(max_bytes as u64).read_to_end(&mut buf)?;
    match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => {
            buf.truncate(i + 1);
            let end = start + buf.len() as u64;
            Ok(Chunk {
                bytes: buf,
                start,
                end,
            })
        }
        None if buf.len() >= max_bytes => {
            // One line larger than the budget: find its end and skip it.
            let mut pos = start + buf.len() as u64;
            let mut block = vec![0u8; SKIP_BLOCK];
            file.seek(SeekFrom::Start(pos))?;
            loop {
                let n = file.read(&mut block)?;
                if n == 0 {
                    // Still being written.
                    return Ok(Chunk {
                        bytes: Vec::new(),
                        start,
                        end: start,
                    });
                }
                if let Some(i) = block[..n].iter().position(|b| *b == b'\n') {
                    pos += i as u64 + 1;
                    return Ok(Chunk {
                        bytes: Vec::new(),
                        start,
                        end: pos,
                    });
                }
                pos += n as u64;
            }
        }
        None => Ok(Chunk {
            bytes: Vec::new(),
            start,
            end: start,
        }),
    }
}

/// Token counts from `modelUsage`, summed across models.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

/// A `cost-state` entry: cumulative session totals.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct CostState {
    pub cost_usd: Option<f64>,
    pub api_duration_ms: Option<i64>,
    pub tokens: Tokens,
}

/// One deduplicated assistant message.
#[derive(Debug, Clone)]
pub(crate) struct AssistantUsage {
    pub line_index: usize,
    pub parent_uuid: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub model: Option<String>,
    pub tokens: Tokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DenialKind {
    Rejected,
    Interrupted,
}

#[derive(Debug, Clone)]
pub(crate) struct Denial {
    pub timestamp: DateTime<Utc>,
    pub kind: DenialKind,
    pub prompt_id: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    parents: HashMap<String, Option<String>>,
    prompt_of: HashMap<String, String>,
    /// `(line_index, state)`, in file order.
    pub cost_states: Vec<(usize, CostState)>,
    /// Keyed by `message.id`; the last entry for an id wins. Iterate via [`Parsed::messages`].
    messages: HashMap<String, AssistantUsage>,
    message_order: Vec<String>,
    pub denials: Vec<Denial>,
    /// Lines that were not JSON objects, or had an unknown shape.
    pub skipped: usize,
}

fn tok(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0).max(0)
}

/// Parse an RFC 3339 timestamp (any offset) to UTC. Shared by the transcript parser and the
/// ingester, which compare times stored in different formats (`+00:00` vs `Z`).
pub(crate) fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

impl Tokens {
    /// Add `other`'s counts into `self`.
    pub(crate) fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}

fn cost_state(e: &Entry) -> CostState {
    let mut t = Tokens::default();
    if let Some(Value::Object(models)) = raw_value(e.model_usage) {
        for m in models.values() {
            t.add(&Tokens {
                input: tok(m, "inputTokens"),
                // `thinkingTokens` are deliberately not added to output: the API reports
                // them inside `outputTokens` already, so adding would double count.
                output: tok(m, "outputTokens"),
                cache_read: tok(m, "cacheReadInputTokens"),
                cache_creation: tok(m, "cacheCreationInputTokens"),
            });
        }
    }
    CostState {
        cost_usd: raw_value(e.total_cost).and_then(|v| v.as_f64()),
        api_duration_ms: raw_value(e.total_api_duration)
            .and_then(|v| v.as_f64())
            .map(|d| d.round() as i64),
        tokens: t,
    }
}

/// The text of the first `text` block, or the content string itself.
fn first_text(content: &Value) -> Option<&str> {
    match content {
        Value::String(s) => Some(s),
        Value::Array(a) => a.iter().find_map(|b| {
            (b.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| b.get("text").and_then(Value::as_str))
                .flatten()
        }),
        _ => None,
    }
}

/// The fields of one transcript line that are read. Everything else (tool output, file
/// bodies, request blobs) is skipped by the deserializer without allocating. Values whose
/// type is not known up front stay as unparsed [`RawValue`] slices and are decoded lazily,
/// so an unexpected type in one field costs that field, not the line.
#[derive(Deserialize)]
struct Entry<'a> {
    #[serde(borrow, default, rename = "type")]
    kind: Option<&'a RawValue>,
    #[serde(borrow, default)]
    uuid: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "parentUuid")]
    parent_uuid: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "promptId")]
    prompt_id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    timestamp: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "toolUseResult")]
    tool_use_result: Option<&'a RawValue>,
    #[serde(borrow, default)]
    message: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "modelUsage")]
    model_usage: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "totalCostUSD")]
    total_cost: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "totalAPIDuration")]
    total_api_duration: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow, default)]
    id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    model: Option<&'a RawValue>,
    #[serde(borrow, default)]
    usage: Option<&'a RawValue>,
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
}

fn raw_value(r: Option<&RawValue>) -> Option<Value> {
    serde_json::from_str(r?.get()).ok()
}

fn raw_string(r: Option<&RawValue>) -> Option<String> {
    serde_json::from_str(r?.get()).ok()
}

pub(crate) fn parse(bytes: &[u8]) -> Parsed {
    let mut p = Parsed::default();
    for (idx, line) in bytes.split(|b| *b == b'\n').enumerate() {
        let Some(first) = line.iter().find(|b| !b.is_ascii_whitespace()) else {
            continue;
        };
        // A struct would also deserialize from a JSON array; only objects are entries.
        if *first != b'{' {
            p.skipped += 1;
            continue;
        }
        let Ok(e) = serde_json::from_slice::<Entry>(line) else {
            p.skipped += 1;
            continue;
        };
        let uuid = raw_string(e.uuid);
        let parent = raw_string(e.parent_uuid);
        let prompt_id = raw_string(e.prompt_id);
        let timestamp = raw_string(e.timestamp).as_deref().and_then(parse_utc);
        if let Some(u) = &uuid {
            p.parents.insert(u.clone(), parent.clone());
            if let Some(pid) = &prompt_id {
                p.prompt_of.insert(u.clone(), pid.clone());
            }
        }
        match raw_string(e.kind).as_deref() {
            Some("cost-state") => p.cost_states.push((idx, cost_state(&e))),
            Some("assistant") => {
                let msg: Option<Message> =
                    e.message.and_then(|m| serde_json::from_str(m.get()).ok());
                let (Some(id), Some(usage)) = (
                    msg.as_ref().and_then(|m| raw_string(m.id)),
                    msg.as_ref().and_then(|m| raw_value(m.usage)),
                ) else {
                    p.skipped += 1;
                    continue;
                };
                let entry = AssistantUsage {
                    line_index: idx,
                    parent_uuid: parent,
                    timestamp,
                    model: msg.as_ref().and_then(|m| raw_string(m.model)),
                    tokens: Tokens {
                        input: tok(&usage, "input_tokens"),
                        output: tok(&usage, "output_tokens"),
                        cache_read: tok(&usage, "cache_read_input_tokens"),
                        cache_creation: tok(&usage, "cache_creation_input_tokens"),
                    },
                };
                if p.messages.insert(id.clone(), entry).is_none() {
                    p.message_order.push(id);
                }
            }
            Some("user") => {
                let kind = if raw_string(e.tool_use_result).as_deref() == Some(TOOL_REJECTED_RESULT)
                {
                    Some(DenialKind::Rejected)
                } else if is_interrupt(&e) {
                    Some(DenialKind::Interrupted)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    match timestamp {
                        // A rejection entry carries no promptId itself; resolved lazily.
                        Some(timestamp) => p.denials.push(Denial {
                            timestamp,
                            kind,
                            prompt_id,
                        }),
                        None => {
                            // A format change here would silently disable denial detection.
                            tracing::debug!(
                                ?kind,
                                "transcript denial entry without a usable timestamp"
                            );
                            p.skipped += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    p
}

/// Whether a `user` entry is the interrupt marker. The raw message is only decoded when the
/// marker text occurs in it, so tool results (file bodies) are never parsed.
fn is_interrupt(e: &Entry) -> bool {
    let Some(raw) = e.message.map(RawValue::get) else {
        return false;
    };
    if !raw.contains(INTERRUPT_PREFIX) {
        return false;
    }
    serde_json::from_str::<Message>(raw)
        .ok()
        .and_then(|m| raw_value(m.content))
        .is_some_and(|c| first_text(&c).is_some_and(|t| t.starts_with(INTERRUPT_PREFIX)))
}

impl Parsed {
    /// Deduplicated assistant messages in first-seen order.
    pub(crate) fn messages(&self) -> impl Iterator<Item = &AssistantUsage> {
        self.message_order
            .iter()
            .filter_map(|id| self.messages.get(id))
    }

    /// The `promptId` of the user entry that `m`'s `parentUuid` chain leads to. `None` when
    /// the chain leaves this chunk before reaching one (or is cyclic).
    pub(crate) fn prompt_id_for(&self, m: &AssistantUsage) -> Option<String> {
        let mut seen = HashSet::new();
        let mut cur = m.parent_uuid.as_deref();
        for _ in 0..MAX_CHAIN_STEPS {
            let u = cur?;
            if let Some(p) = self.prompt_of.get(u) {
                return Some(p.clone());
            }
            if !seen.insert(u) {
                return None;
            }
            cur = self.parents.get(u)?.as_deref();
        }
        None
    }

    /// Latest cost-state and its line index.
    pub(crate) fn latest_cost_state(&self) -> Option<&(usize, CostState)> {
        self.cost_states.last()
    }
}

#[cfg(test)]
pub(crate) const FIXTURE: &str = include_str!("testdata/transcript-cc-2.1.317.jsonl");

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &tempfile::TempDir, name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn fixture_latest_cost_state_and_dedup() {
        let p = parse(FIXTURE.as_bytes());
        assert_eq!(p.cost_states.len(), 2);
        let (_, cs) = p.latest_cost_state().unwrap();
        assert_eq!(cs.cost_usd, Some(0.5));
        assert_eq!(cs.tokens.input, 2252);
        assert_eq!(cs.tokens.output, 330);
        assert_eq!(cs.api_duration_ms, Some(41000));
        // 9 assistant entries, one repeated message.id: 8 messages.
        assert_eq!(p.messages().count(), 8);
        assert!(p.skipped >= 1);
    }

    #[test]
    fn fixture_attribution_through_attachment_hop() {
        let p = parse(FIXTURE.as_bytes());
        let ids: Vec<_> = p.messages().map(|m| p.prompt_id_for(m)).collect();
        assert!(ids.iter().all(Option::is_some));
        assert!(ids.contains(&Some("P1".to_string())));
        assert!(ids.contains(&Some("P2".to_string())));
    }

    #[test]
    fn fixture_denials() {
        let p = parse(FIXTURE.as_bytes());
        let kinds: Vec<_> = p.denials.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, vec![DenialKind::Rejected, DenialKind::Interrupted]);
    }

    #[test]
    fn garbage_is_tolerated() {
        let data = b"not json\n[1,2]\n{\"type\":\"mystery\"}\n{\"type\":\"assistant\"}\n{\"type\":\"user\",\"message\":5}\n\n";
        let p = parse(data);
        assert!(p.messages().count() == 0 && p.cost_states.is_empty());
        assert!(p.skipped >= 3);
    }

    #[test]
    fn cycle_in_parent_chain_terminates() {
        let data = b"{\"type\":\"attachment\",\"uuid\":\"a\",\"parentUuid\":\"b\"}\n{\"type\":\"attachment\",\"uuid\":\"b\",\"parentUuid\":\"a\"}\n{\"type\":\"assistant\",\"uuid\":\"c\",\"parentUuid\":\"a\",\"message\":{\"id\":\"m\",\"usage\":{}}}\n";
        let p = parse(data);
        let m = p.messages().next().unwrap();
        assert_eq!(p.prompt_id_for(m), None);
    }

    #[test]
    fn read_chunk_stops_at_last_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "t.jsonl", b"{\"a\":1}\n{\"b\":2}\n{\"c\":");
        let c = read_chunk(&path, 0, 1 << 20).unwrap();
        assert_eq!(c.end, 16);
        assert_eq!(c.bytes.len(), 16);
        // Only a partial line remains.
        let c2 = read_chunk(&path, 16, 1 << 20).unwrap();
        assert_eq!((c2.start, c2.end), (16, 16));
        assert!(c2.bytes.is_empty());
        // Complete it and read the rest.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"3}\n").unwrap();
        let c3 = read_chunk(&path, 16, 1 << 20).unwrap();
        assert_eq!(c3.bytes, b"{\"c\":3}\n");
    }

    #[test]
    fn read_chunk_skips_oversize_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = vec![b'x'; 300];
        data.push(b'\n');
        data.extend_from_slice(b"{\"ok\":1}\n");
        let path = write(&dir, "t.jsonl", &data);
        let c = read_chunk(&path, 0, 100).unwrap();
        assert!(c.bytes.is_empty());
        assert_eq!(c.end, 301);
        let c = read_chunk(&path, c.end, 100).unwrap();
        assert_eq!(c.bytes, b"{\"ok\":1}\n");
    }

    #[test]
    fn read_chunk_oversize_line_still_being_written_waits() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "t.jsonl", &vec![b'x'; 300]);
        let c = read_chunk(&path, 0, 100).unwrap();
        assert_eq!((c.start, c.end), (0, 0));
    }

    #[test]
    fn read_chunk_restarts_after_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "t.jsonl", b"{\"a\":1}\n");
        let c = read_chunk(&path, 999, 1 << 20).unwrap();
        assert_eq!((c.start, c.end), (0, 8));
    }
}
