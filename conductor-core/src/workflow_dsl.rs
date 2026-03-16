//! Recursive descent parser for the `.wf` workflow DSL.
//!
//! Converts `.conductor/workflows/<name>.wf` files into a `WorkflowDef` with a
//! tree-structured body of `WorkflowNode`s.
//!
//! Grammar (informal):
//! ```text
//! workflow_file := "workflow" IDENT "{" meta? inputs? node* "}"
//! meta          := "meta" "{" kv* "}"
//! inputs        := "inputs" "{" input_decl* "}"
//! input_decl    := IDENT ("required" | "default" "=" STRING)
//! node          := call | call_workflow | if_node | while_node | do_while | do | parallel | gate | always
//! call          := "call" agent_ref ("{" kv* "}")?
//! call_workflow := "call" "workflow" IDENT ("{" inputs? kv* "}")?
//! if_node       := "if" condition "{" kv* node* "}"
//! while_node    := "while" condition "{" kv* node* "}"
//! do_while      := "do" "{" kv* node* "}" "while" condition
//! do            := "do" "{" kv* node* "}"
//! parallel      := "parallel" "{" kv* ("call" agent_ref ("{" kv* "}")?)*  "}"
//! gate          := "gate" IDENT "{" kv* "}"
//! always        := "always" "{" node* "}"
//! condition     := IDENT "." IDENT
//! kv            := IDENT "=" (STRING | NUMBER | IDENT)
//! agent_ref     := IDENT | STRING
//! ```
//!
//! `agent_ref` is a bare identifier (short name resolved via search order) or a
//! quoted string (explicit path relative to the repo root).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ConductorError, Result};
use crate::text_util::{resolve_conductor_subdir, resolve_conductor_subdir_for_file};

// ---------------------------------------------------------------------------
// AST types
// ---------------------------------------------------------------------------

/// A complete workflow definition parsed from a `.wf` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    pub description: String,
    pub trigger: WorkflowTrigger,
    #[serde(default)]
    pub targets: Vec<String>,
    pub inputs: Vec<InputDecl>,
    pub body: Vec<WorkflowNode>,
    pub always: Vec<WorkflowNode>,
    pub source_path: String,
}

impl WorkflowDef {
    /// Total number of nodes across body and always blocks.
    pub fn total_nodes(&self) -> usize {
        count_nodes(&self.body) + count_nodes(&self.always)
    }

    /// Collect all prompt snippet references across body and always blocks, sorted and deduplicated.
    pub fn collect_all_snippet_refs(&self) -> Vec<String> {
        let mut refs = collect_snippet_refs(&self.body);
        refs.extend(collect_snippet_refs(&self.always));
        refs.sort();
        refs.dedup();
        refs
    }

    /// Collect all output schema references across body and always blocks, sorted and deduplicated.
    pub fn collect_all_schema_refs(&self) -> Vec<String> {
        let mut refs = collect_schema_refs(&self.body);
        refs.extend(collect_schema_refs(&self.always));
        refs.sort();
        refs.dedup();
        refs
    }

    /// Collect all bot names referenced across body and always blocks, sorted and deduplicated.
    pub fn collect_all_bot_names(&self) -> Vec<String> {
        let mut names = collect_bot_names(&self.body);
        names.extend(collect_bot_names(&self.always));
        names.sort();
        names.dedup();
        names
    }
}

/// A structured parse warning produced when a `.wf` file fails to load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowWarning {
    /// The filename (e.g. `bad.wf`) that failed to parse.
    pub file: String,
    /// Human-readable description of the parse error.
    pub message: String,
}

/// Trigger type for when a workflow should run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTrigger {
    Manual,
    Pr,
    Scheduled,
}

impl std::fmt::Display for WorkflowTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manual => write!(f, "manual"),
            Self::Pr => write!(f, "pr"),
            Self::Scheduled => write!(f, "scheduled"),
        }
    }
}

impl std::str::FromStr for WorkflowTrigger {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "manual" => Ok(Self::Manual),
            "pr" => Ok(Self::Pr),
            "scheduled" => Ok(Self::Scheduled),
            _ => Err(format!("unknown trigger: {s}")),
        }
    }
}

/// An input declaration for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDecl {
    pub name: String,
    pub required: bool,
    pub default: Option<String>,
    pub description: Option<String>,
}

/// A node in the workflow execution graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowNode {
    Call(CallNode),
    CallWorkflow(CallWorkflowNode),
    If(IfNode),
    Unless(UnlessNode),
    While(WhileNode),
    DoWhile(DoWhileNode),
    Do(DoNode),
    Parallel(ParallelNode),
    Gate(GateNode),
    Always(AlwaysNode),
    Script(ScriptNode),
}

/// A script step node — runs a shell script directly (no agent/LLM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptNode {
    /// Step name used as the step key in step_results and resume skip sets.
    pub name: String,
    /// Path to the script to run (supports `{{variable}}` substitution).
    /// Resolved in order: worktree dir → repo dir → `~/.claude/skills/`.
    pub run: String,
    /// Environment variable overrides (values support `{{variable}}` substitution).
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Optional timeout in seconds. If the script does not complete within this
    /// duration it is killed and the step is marked `TimedOut`.
    pub timeout: Option<u64>,
    /// Number of retry attempts after the first failure (0 = no retries).
    #[serde(default)]
    pub retries: u32,
    /// Agent to invoke if all attempts fail.
    pub on_fail: Option<AgentRef>,
}

/// Reference to an agent — either a short name or an explicit file path.
///
/// - `Name`: bare identifier (e.g. `plan`) resolved via the search order.
/// - `Path`: quoted string (e.g. `".claude/agents/plan.md"`) resolved directly
///   relative to the repository root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AgentRef {
    Name(String),
    Path(String),
}

impl AgentRef {
    /// Human-readable label for display and logging (the inner string value).
    pub fn label(&self) -> &str {
        match self {
            Self::Name(s) | Self::Path(s) => s.as_str(),
        }
    }

    /// Key used to store and look up results in `step_results`.
    ///
    /// - `Name` variants return the name as-is.
    /// - `Path` variants return the file stem without extension
    ///   (e.g. `"plan"` from `".claude/agents/plan.md"`), so that `if`/`while`
    ///   conditions can reference path-based agents by their short name.
    pub fn step_key(&self) -> String {
        match self {
            Self::Name(s) => s.clone(),
            Self::Path(s) => Path::new(s)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(s.as_str())
                .to_string(),
        }
    }
}

impl std::fmt::Display for AgentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallNode {
    pub agent: AgentRef,
    #[serde(default)]
    pub retries: u32,
    pub on_fail: Option<AgentRef>,
    /// Optional output schema reference for structured output.
    pub output: Option<String>,
    /// Prompt snippet references to append to the agent prompt.
    #[serde(default)]
    pub with: Vec<String>,
    /// Named GitHub App bot identity to use for this call (matches `[github.apps.<name>]`).
    pub bot_name: Option<String>,
}

/// A sub-workflow invocation node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallWorkflowNode {
    pub workflow: String,
    #[serde(default)]
    pub inputs: HashMap<String, String>,
    #[serde(default)]
    pub retries: u32,
    pub on_fail: Option<AgentRef>,
    /// Named GitHub App bot identity inherited by child call nodes.
    pub bot_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfNode {
    pub step: String,
    pub marker: String,
    pub body: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnlessNode {
    pub step: String,
    pub marker: String,
    pub body: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhileNode {
    pub step: String,
    pub marker: String,
    pub max_iterations: u32,
    pub stuck_after: Option<u32>,
    pub on_max_iter: OnMaxIter,
    pub body: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoWhileNode {
    pub step: String,
    pub marker: String,
    pub max_iterations: u32,
    pub stuck_after: Option<u32>,
    pub on_max_iter: OnMaxIter,
    pub body: Vec<WorkflowNode>,
}

/// A plain sequential grouping block (`do { ... }`), with optional `output` and `with`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoNode {
    /// Optional output schema reference for structured output.
    pub output: Option<String>,
    /// Prompt snippet references applied to all calls inside the block.
    #[serde(default)]
    pub with: Vec<String>,
    pub body: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnMaxIter {
    Fail,
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelNode {
    #[serde(default = "default_true")]
    pub fail_fast: bool,
    pub min_success: Option<u32>,
    pub calls: Vec<AgentRef>,
    /// Block-level output schema reference (applies to all calls unless overridden).
    pub output: Option<String>,
    /// Per-call output schema overrides, keyed by index (as string) in `calls`.
    /// String keys are used because JSON object keys are always strings and serde_json
    /// cannot coerce them back to integer types on deserialization.
    #[serde(default)]
    pub call_outputs: HashMap<String, String>,
    /// Block-level prompt snippet references (applied to all calls).
    #[serde(default)]
    pub with: Vec<String>,
    /// Per-call prompt snippet additions, keyed by index (as string) in `calls`.
    #[serde(default)]
    pub call_with: HashMap<String, Vec<String>>,
    /// Per-call `if` conditions keyed by index (as string) in `calls`.
    /// Value is (step_name, marker_name). Run the call only if that marker is present.
    #[serde(default)]
    pub call_if: HashMap<String, (String, String)>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    MinApprovals,
    ReviewDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateNode {
    pub name: String,
    pub gate_type: GateType,
    pub prompt: Option<String>,
    #[serde(default = "default_one")]
    pub min_approvals: u32,
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    pub timeout_secs: u64,
    pub on_timeout: OnTimeout,
    /// Named GitHub App bot identity used for `gh` calls inside this gate.
    pub bot_name: Option<String>,
}

fn default_one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateType {
    HumanApproval,
    HumanReview,
    PrApproval,
    PrChecks,
}

impl std::fmt::Display for GateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HumanApproval => write!(f, "human_approval"),
            Self::HumanReview => write!(f, "human_review"),
            Self::PrApproval => write!(f, "pr_approval"),
            Self::PrChecks => write!(f, "pr_checks"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    Fail,
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlwaysNode {
    pub body: Vec<WorkflowNode>,
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Keywords
    Workflow,
    Meta,
    Inputs,
    Call,
    If,
    Unless,
    While,
    Do,
    Parallel,
    Gate,
    Always,
    Script,
    Required,
    Default,
    Description,
    // Punctuation
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Equals,
    Dot,
    // Literals
    Ident(String),
    StringLit(String),
    Int(u32),
    // End
    Eof,
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn location(&self) -> String {
        format!("line {}, col {}", self.line, self.col)
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while self.peek_char().is_some_and(|c| c.is_whitespace()) {
                self.advance();
            }
            // Skip line comments
            if self.pos + 1 < self.chars.len()
                && self.chars[self.pos] == '/'
                && self.chars[self.pos + 1] == '/'
            {
                while self.peek_char().is_some_and(|c| c != '\n') {
                    self.advance();
                }
                continue;
            }
            break;
        }
    }

    fn next_token(&mut self) -> std::result::Result<Token, String> {
        self.skip_whitespace_and_comments();

        let Some(ch) = self.peek_char() else {
            return Ok(Token::Eof);
        };

        match ch {
            '{' => {
                self.advance();
                Ok(Token::LBrace)
            }
            '}' => {
                self.advance();
                Ok(Token::RBrace)
            }
            '[' => {
                self.advance();
                Ok(Token::LBracket)
            }
            ']' => {
                self.advance();
                Ok(Token::RBracket)
            }
            ',' => {
                self.advance();
                Ok(Token::Comma)
            }
            '=' => {
                self.advance();
                Ok(Token::Equals)
            }
            '.' => {
                self.advance();
                Ok(Token::Dot)
            }
            '"' => self.read_string(),
            c if c.is_ascii_digit() => self.read_int(),
            c if c.is_ascii_alphabetic() || c == '_' => self.read_ident_or_keyword(),
            _ => Err(format!(
                "Unexpected character '{}' at {}",
                ch,
                self.location()
            )),
        }
    }

    fn read_string(&mut self) -> std::result::Result<Token, String> {
        self.advance(); // skip opening quote
        let mut value = String::new();
        loop {
            match self.advance() {
                Some('"') => return Ok(Token::StringLit(value)),
                Some('\\') => match self.advance() {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some('"') => value.push('"'),
                    Some('\\') => value.push('\\'),
                    Some(c) => value.push(c),
                    None => return Err("Unterminated string escape".to_string()),
                },
                Some(c) => value.push(c),
                None => return Err("Unterminated string literal".to_string()),
            }
        }
    }

    fn read_int(&mut self) -> std::result::Result<Token, String> {
        let mut s = String::new();
        while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            s.push(self.advance().unwrap());
        }
        s.parse::<u32>()
            .map(Token::Int)
            .map_err(|e| format!("Invalid integer '{}': {e}", s))
    }

    fn read_ident_or_keyword(&mut self) -> std::result::Result<Token, String> {
        let mut s = String::new();
        while self
            .peek_char()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            s.push(self.advance().unwrap());
        }
        Ok(match s.as_str() {
            "workflow" => Token::Workflow,
            "meta" => Token::Meta,
            "inputs" => Token::Inputs,
            "call" => Token::Call,
            "if" => Token::If,
            "unless" => Token::Unless,
            "while" => Token::While,
            "do" => Token::Do,
            "parallel" => Token::Parallel,
            "gate" => Token::Gate,
            "always" => Token::Always,
            "script" => Token::Script,
            "required" => Token::Required,
            "default" => Token::Default,
            "description" => Token::Description,
            _ => Token::Ident(s),
        })
    }
}

// ---------------------------------------------------------------------------
// Parser helpers
// ---------------------------------------------------------------------------

/// A value from a key-value pair in the DSL, remembering whether it was quoted.
///
/// This preserves the syntactic distinction between bare identifiers/numbers
/// (`Bare`) and quoted string literals (`Quoted`). When converting to an
/// `AgentRef`, a quoted value is treated as a `Path` only if it contains a
/// `/` (i.e. looks like a file path); otherwise it is treated as a `Name`.
#[derive(Debug, Clone)]
enum KvValue {
    /// Came from a quoted string literal: `"some/path.md"`.
    Quoted(String),
    /// Came from a bare identifier, keyword, or integer literal: `diagnose`, `3`.
    Bare(String),
    /// Came from a bracket-delimited array: `["a", "b"]`.
    Array(Vec<String>),
    /// Came from a brace-delimited map: `{ KEY = "value" }`.
    Map(HashMap<String, String>),
}

impl KvValue {
    fn as_str(&self) -> &str {
        match self {
            Self::Quoted(s) | Self::Bare(s) => s.as_str(),
            Self::Array(_) => unreachable!(
                "BUG: as_str() called on KvValue::Array — arrays are only valid for array-valued keys (e.g. `with`, `targets`)"
            ),
            Self::Map(_) => unreachable!(
                "BUG: as_str() called on KvValue::Map — maps are only valid for `env =` keys"
            ),
        }
    }

    fn into_string(self) -> String {
        match self {
            Self::Quoted(s) | Self::Bare(s) => s,
            Self::Array(_) => unreachable!(
                "BUG: into_string() called on KvValue::Array — arrays are only valid for array-valued keys (e.g. `with`, `targets`)"
            ),
            Self::Map(_) => unreachable!(
                "BUG: into_string() called on KvValue::Map — maps are only valid for `env =` keys"
            ),
        }
    }

    fn into_string_array(self) -> Vec<String> {
        match self {
            Self::Array(v) => v,
            Self::Quoted(s) | Self::Bare(s) => vec![s],
            Self::Map(_) => unreachable!(
                "BUG: into_string_array() called on KvValue::Map — maps are only valid for `env =` keys"
            ),
        }
    }

    /// Convert into an `AgentRef`.
    ///
    /// **Intentionally differs from `expect_agent_ref` (call position):**
    ///
    /// In a kv-value position (e.g. `on_fail = "..."`), quoting is common
    /// style for any string value and does not carry the same semantic weight
    /// as quoting in `call` position, where the user explicitly chose to write
    /// `call "..."` to signal a file path.  To avoid treating
    /// `on_fail = "diagnose"` as an explicit path, we only produce
    /// `AgentRef::Path` for values that actually look like file paths (i.e.
    /// contain a `/`).  A bare-quoted name like `"diagnose"` remains
    /// `AgentRef::Name`.
    ///
    /// Compare with [`Parser::expect_agent_ref`], where any quoted string in
    /// `call` position is unconditionally `AgentRef::Path` because the author
    /// deliberately chose the quoted form.
    fn into_agent_ref(self) -> AgentRef {
        match self {
            Self::Bare(s) => AgentRef::Name(s),
            Self::Quoted(s) if s.contains('/') => AgentRef::Path(s),
            Self::Quoted(s) => AgentRef::Name(s),
            Self::Array(_) => unreachable!(
                "BUG: into_agent_ref() called on KvValue::Array — arrays are only valid for `with` keys"
            ),
            Self::Map(_) => unreachable!(
                "BUG: into_agent_ref() called on KvValue::Map — maps are only valid for `env =` keys"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    warnings: Vec<String>,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            warnings: Vec::new(),
        }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &Token) -> std::result::Result<(), String> {
        let tok = self.advance();
        if &tok == expected {
            Ok(())
        } else {
            Err(format!("Expected {expected:?}, got {tok:?}"))
        }
    }

    fn expect_ident(&mut self) -> std::result::Result<String, String> {
        match self.advance() {
            Token::Ident(s) => Ok(s),
            // Allow keywords to be used as identifiers in certain positions
            Token::Required => Ok("required".to_string()),
            Token::Default => Ok("default".to_string()),
            Token::Description => Ok("description".to_string()),
            Token::If => Ok("if".to_string()),
            other => Err(format!("Expected identifier, got {other:?}")),
        }
    }

    fn expect_value(&mut self) -> std::result::Result<KvValue, String> {
        match self.advance() {
            Token::StringLit(s) => Ok(KvValue::Quoted(s)),
            Token::Int(n) => Ok(KvValue::Bare(n.to_string())),
            Token::Ident(s) => Ok(KvValue::Bare(s)),
            // Allow keyword tokens as values
            Token::Required => Ok(KvValue::Bare("required".to_string())),
            Token::Default => Ok(KvValue::Bare("default".to_string())),
            Token::Description => Ok(KvValue::Bare("description".to_string())),
            Token::Call => Ok(KvValue::Bare("call".to_string())),
            Token::If => Ok(KvValue::Bare("if".to_string())),
            Token::Unless => Ok(KvValue::Bare("unless".to_string())),
            Token::While => Ok(KvValue::Bare("while".to_string())),
            Token::Parallel => Ok(KvValue::Bare("parallel".to_string())),
            Token::Gate => Ok(KvValue::Bare("gate".to_string())),
            Token::Always => Ok(KvValue::Bare("always".to_string())),
            Token::Script => Ok(KvValue::Bare("script".to_string())),
            // Map literal: { KEY = "value", KEY2 = "value2" }
            Token::LBrace => {
                let kvs = self.parse_kvs()?;
                self.expect(&Token::RBrace)?;
                let map: HashMap<String, String> =
                    kvs.into_iter().map(|(k, v)| (k, v.into_string())).collect();
                Ok(KvValue::Map(map))
            }
            // Array literal: ["a", "b", "c"]
            Token::LBracket => {
                let mut items = Vec::new();
                while self.peek() != &Token::RBracket && self.peek() != &Token::Eof {
                    let item = self.expect_value()?;
                    items.push(item.into_string());
                    // Optional trailing comma
                    if self.peek() == &Token::Comma {
                        self.advance();
                    }
                }
                self.expect(&Token::RBracket)?;
                Ok(KvValue::Array(items))
            }
            other => Err(format!(
                "Expected value (string, int, ident, or array), got {other:?}"
            )),
        }
    }

    /// Parse an agent reference: either a bare identifier (Name) or a quoted
    /// string (Path).
    ///
    /// **Intentionally differs from `KvValue::into_agent_ref` (kv-value position):**
    ///
    /// In `call` position the author deliberately chooses between bare
    /// (`call plan`) and quoted (`call "..."`) syntax, so a quoted string is
    /// *always* treated as an explicit file path — even when it contains no
    /// `/`.  This is in contrast to kv values like `on_fail = "diagnose"`,
    /// where quoting is commonly used for style and a slash-based heuristic
    /// is used instead.
    fn expect_agent_ref(&mut self) -> std::result::Result<AgentRef, String> {
        match self.advance() {
            Token::Ident(s) => Ok(AgentRef::Name(s)),
            Token::Required => Ok(AgentRef::Name("required".to_string())),
            Token::Default => Ok(AgentRef::Name("default".to_string())),
            Token::Description => Ok(AgentRef::Name("description".to_string())),
            Token::StringLit(s) => Ok(AgentRef::Path(s)),
            other => Err(format!(
                "Expected agent name (identifier) or path (quoted string), got {other:?}"
            )),
        }
    }

    // Parse key-value pairs until we hit a non-kv token.
    // KV pairs look like: IDENT = VALUE
    fn parse_kvs(&mut self) -> std::result::Result<HashMap<String, KvValue>, String> {
        let mut kvs = HashMap::new();
        loop {
            // Peek ahead: if it's an ident/keyword followed by '=', it's a kv.
            if self.pos + 1 < self.tokens.len() {
                let is_kv_key = matches!(
                    self.peek(),
                    Token::Ident(_)
                        | Token::Required
                        | Token::Default
                        | Token::Description
                        | Token::If
                );
                let next_is_eq = self.tokens.get(self.pos + 1) == Some(&Token::Equals);
                if is_kv_key && next_is_eq {
                    let key = self.expect_ident()?;
                    self.expect(&Token::Equals)?;
                    let value = self.expect_value()?;
                    kvs.insert(key, value);
                    continue;
                }
            }
            break;
        }
        Ok(kvs)
    }

    fn parse_workflow(&mut self) -> std::result::Result<WorkflowDef, String> {
        self.expect(&Token::Workflow)?;
        let name = self.expect_ident()?;
        self.expect(&Token::LBrace)?;

        let mut description = String::new();
        let mut trigger = WorkflowTrigger::Manual;
        let mut targets: Vec<String> = Vec::new();
        let mut inputs = Vec::new();
        let mut body = Vec::new();
        let mut always = Vec::new();

        // Parse blocks inside the workflow
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            match self.peek() {
                Token::Meta => {
                    self.advance();
                    self.expect(&Token::LBrace)?;
                    let kvs = self.parse_kvs()?;
                    self.expect(&Token::RBrace)?;

                    if let Some(desc) = kvs.get("description") {
                        description = desc.as_str().to_string();
                    }
                    if let Some(trig) = kvs.get("trigger") {
                        let trig_str = trig.as_str();
                        trigger = trig_str
                            .parse::<WorkflowTrigger>()
                            .map_err(|e| format!("In meta block: {e}"))?;
                        if trigger != WorkflowTrigger::Manual {
                            self.warnings.push(format!(
                                "trigger '{}' is not implemented in v1; only 'manual' is active",
                                trig_str
                            ));
                        }
                    }
                    if let Some(tgts) = kvs.get("targets") {
                        targets = tgts.clone().into_string_array();
                    }
                }
                Token::Inputs => {
                    self.advance();
                    self.expect(&Token::LBrace)?;
                    while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                        let input_name = self.expect_ident()?;
                        let mut required = false;
                        let mut default: Option<String> = None;
                        let mut description: Option<String> = None;
                        // Collect optional modifiers: required, default = "...", description = "..."
                        loop {
                            match self.peek() {
                                Token::Required => {
                                    self.advance();
                                    required = true;
                                }
                                Token::Default => {
                                    self.advance();
                                    self.expect(&Token::Equals)?;
                                    default = Some(self.expect_value()?.into_string());
                                }
                                Token::Description => {
                                    self.advance();
                                    self.expect(&Token::Equals)?;
                                    description = Some(self.expect_value()?.into_string());
                                }
                                _ => break,
                            }
                        }
                        // A bare identifier with no default is treated as required.
                        // Having only a description does not make an input optional.
                        if !required && default.is_none() {
                            required = true;
                        }
                        inputs.push(InputDecl {
                            name: input_name,
                            required,
                            default,
                            description,
                        });
                    }
                    self.expect(&Token::RBrace)?;
                }
                Token::Always => {
                    self.advance();
                    self.expect(&Token::LBrace)?;
                    while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                        always.push(self.parse_node()?);
                    }
                    self.expect(&Token::RBrace)?;
                }
                _ => {
                    body.push(self.parse_node()?);
                }
            }
        }

        self.expect(&Token::RBrace)?;

        if targets.is_empty() {
            return Err(format!(
                "workflow '{name}' is missing a required `targets` field in its meta block.\n\
                 Add at least one target, e.g.: targets = [\"worktree\"]"
            ));
        }

        Ok(WorkflowDef {
            name,
            description,
            trigger,
            targets,
            inputs,
            body,
            always,
            source_path: String::new(),
        })
    }

    fn parse_node(&mut self) -> std::result::Result<WorkflowNode, String> {
        match self.peek() {
            Token::Call => {
                // Peek ahead: `call workflow <ident>` is a sub-workflow call.
                if self.tokens.get(self.pos + 1) == Some(&Token::Workflow) {
                    self.parse_call_workflow().map(WorkflowNode::CallWorkflow)
                } else {
                    self.parse_call().map(WorkflowNode::Call)
                }
            }
            Token::If => self.parse_if().map(WorkflowNode::If),
            Token::Unless => self.parse_unless().map(WorkflowNode::Unless),
            Token::While => self.parse_while().map(WorkflowNode::While),
            Token::Do => self.parse_do(),
            Token::Parallel => self.parse_parallel().map(WorkflowNode::Parallel),
            Token::Gate => self.parse_gate().map(WorkflowNode::Gate),
            Token::Always => self.parse_always().map(WorkflowNode::Always),
            Token::Script => self.parse_script().map(WorkflowNode::Script),
            other => Err(format!(
                "Expected a workflow node (call, if, unless, while, do, parallel, gate, always, script), got {other:?}"
            )),
        }
    }

    fn parse_call(&mut self) -> std::result::Result<CallNode, String> {
        self.expect(&Token::Call)?;
        let agent = self.expect_agent_ref()?;

        let mut retries = 0u32;
        let mut on_fail = None;
        let mut output = None;
        let mut with = Vec::new();
        let mut bot_name = None;

        if self.peek() == &Token::LBrace {
            self.advance();
            let mut kvs = self.parse_kvs()?;
            self.expect(&Token::RBrace)?;

            if let Some(r) = kvs.get("retries") {
                retries = r
                    .as_str()
                    .parse()
                    .map_err(|e| format!("Invalid retries: {e}"))?;
            }
            if let Some(f) = kvs.remove("on_fail") {
                on_fail = Some(f.into_agent_ref());
            }
            if let Some(o) = kvs.remove("output") {
                output = Some(o.into_string());
            }
            if let Some(w) = kvs.remove("with") {
                with = w.into_string_array();
            }
            if let Some(b) = kvs.remove("as") {
                bot_name = Some(b.into_string());
            }
        }

        Ok(CallNode {
            agent,
            retries,
            on_fail,
            output,
            with,
            bot_name,
        })
    }

    fn parse_call_workflow(&mut self) -> std::result::Result<CallWorkflowNode, String> {
        self.expect(&Token::Call)?;
        self.expect(&Token::Workflow)?;
        let workflow_name = self.expect_ident()?;

        let mut inputs = HashMap::new();
        let mut retries = 0u32;
        let mut on_fail = None;
        let mut bot_name = None;

        if self.peek() == &Token::LBrace {
            self.advance();

            // Parse kvs that may appear before inputs { } (e.g. `as = "developer"`)
            let mut kvs = self.parse_kvs()?;

            // Parse optional `inputs { ... }` block inside the braces
            if self.peek() == &Token::Inputs {
                self.advance();
                self.expect(&Token::LBrace)?;
                let input_kvs = self.parse_kvs()?;
                self.expect(&Token::RBrace)?;
                for (k, v) in input_kvs {
                    inputs.insert(k, v.into_string());
                }
            }

            // Parse any remaining kvs after inputs { } and merge
            kvs.extend(self.parse_kvs()?);
            self.expect(&Token::RBrace)?;

            if let Some(r) = kvs.get("retries") {
                retries = r
                    .as_str()
                    .parse()
                    .map_err(|e| format!("Invalid retries: {e}"))?;
            }
            if let Some(f) = kvs.remove("on_fail") {
                on_fail = Some(f.into_agent_ref());
            }
            if let Some(b) = kvs.remove("as") {
                bot_name = Some(b.into_string());
            }
        }

        Ok(CallWorkflowNode {
            workflow: workflow_name,
            inputs,
            retries,
            on_fail,
            bot_name,
        })
    }

    fn parse_condition(&mut self) -> std::result::Result<(String, String), String> {
        let step = self.expect_ident()?;
        self.expect(&Token::Dot)?;
        let marker = self.expect_ident()?;
        Ok((step, marker))
    }

    fn parse_if(&mut self) -> std::result::Result<IfNode, String> {
        self.expect(&Token::If)?;
        let (step, marker) = self.parse_condition()?;
        self.expect(&Token::LBrace)?;

        // Parse optional kvs (not used for if, but kept for grammar consistency)
        let _kvs = self.parse_kvs()?;

        let mut body = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            body.push(self.parse_node()?);
        }
        self.expect(&Token::RBrace)?;

        Ok(IfNode { step, marker, body })
    }

    fn parse_unless(&mut self) -> std::result::Result<UnlessNode, String> {
        self.expect(&Token::Unless)?;
        let (step, marker) = self.parse_condition()?;
        self.expect(&Token::LBrace)?;

        // Parse optional kvs (not used for unless, but kept for grammar consistency)
        let _kvs = self.parse_kvs()?;

        let mut body = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            body.push(self.parse_node()?);
        }
        self.expect(&Token::RBrace)?;

        Ok(UnlessNode { step, marker, body })
    }

    /// Extract the common loop KV options (`max_iterations`, `stuck_after`, `on_max_iter`)
    /// shared by `while` and `do` loops.
    fn parse_loop_options(
        kvs: &HashMap<String, KvValue>,
        loop_kind: &str,
    ) -> std::result::Result<(u32, Option<u32>, OnMaxIter), String> {
        let max_iterations = kvs
            .get("max_iterations")
            .ok_or(format!("{loop_kind} loop requires max_iterations"))?
            .as_str()
            .parse::<u32>()
            .map_err(|e| format!("Invalid max_iterations: {e}"))?;

        let stuck_after = kvs
            .get("stuck_after")
            .map(|v| v.as_str().parse::<u32>())
            .transpose()
            .map_err(|e| format!("Invalid stuck_after: {e}"))?;

        let on_max_iter = match kvs.get("on_max_iter").map(|s| s.as_str()) {
            Some("continue") => OnMaxIter::Continue,
            Some("fail") | None => OnMaxIter::Fail,
            Some(other) => return Err(format!("Invalid on_max_iter: {other}")),
        };

        Ok((max_iterations, stuck_after, on_max_iter))
    }

    fn parse_while(&mut self) -> std::result::Result<WhileNode, String> {
        self.expect(&Token::While)?;
        let (step, marker) = self.parse_condition()?;
        self.expect(&Token::LBrace)?;

        let kvs = self.parse_kvs()?;
        let (max_iterations, stuck_after, on_max_iter) = Self::parse_loop_options(&kvs, "while")?;

        let mut body = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            body.push(self.parse_node()?);
        }
        self.expect(&Token::RBrace)?;

        Ok(WhileNode {
            step,
            marker,
            max_iterations,
            stuck_after,
            on_max_iter,
            body,
        })
    }

    fn parse_do(&mut self) -> std::result::Result<WorkflowNode, String> {
        self.expect(&Token::Do)?;

        // New syntax: do { ... } [while condition]
        // Old syntax was: do x.y { ... } — give a clear error if we see an ident here.
        if matches!(self.peek(), Token::Ident(_)) {
            return Err(
                "expected `{` after `do`, found identifier\n  hint: do-while syntax is now `do { ... } while x.y`".to_string()
            );
        }
        self.expect(&Token::LBrace)?;

        let mut kvs = self.parse_kvs()?;

        let mut body = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            body.push(self.parse_node()?);
        }
        self.expect(&Token::RBrace)?;

        // Peek for optional `while` clause (one-token lookahead past `}`)
        if self.peek() == &Token::While {
            self.advance(); // consume `while`
            let (step, marker) = self.parse_condition()?;
            let (max_iterations, stuck_after, on_max_iter) = Self::parse_loop_options(&kvs, "do")?;
            Ok(WorkflowNode::DoWhile(DoWhileNode {
                step,
                marker,
                max_iterations,
                stuck_after,
                on_max_iter,
                body,
            }))
        } else {
            // Plain sequential block — only output/with allowed as options
            let output = kvs.remove("output").map(|v| v.as_str().to_string());
            let with = kvs
                .remove("with")
                .map(|v| v.into_string_array())
                .unwrap_or_default();
            if let Some(key) = kvs.keys().next() {
                return Err(format!(
                    "unknown option `{key}` in plain `do` block (only `output` and `with` are allowed)"
                ));
            }
            Ok(WorkflowNode::Do(DoNode { output, with, body }))
        }
    }

    fn parse_parallel(&mut self) -> std::result::Result<ParallelNode, String> {
        self.expect(&Token::Parallel)?;
        self.expect(&Token::LBrace)?;

        let mut kvs = self.parse_kvs()?;

        let fail_fast = kvs
            .get("fail_fast")
            .map(|v| v.as_str() == "true")
            .unwrap_or(true);

        let min_success = kvs
            .get("min_success")
            .map(|v| v.as_str().parse::<u32>())
            .transpose()
            .map_err(|e| format!("Invalid min_success: {e}"))?;

        let output = kvs.get("output").map(|v| v.as_str().to_string());

        let block_with = kvs
            .remove("with")
            .map(|v| v.into_string_array())
            .unwrap_or_default();

        let mut calls = Vec::new();
        let mut call_outputs: HashMap<String, String> = HashMap::new();
        let mut call_with: HashMap<String, Vec<String>> = HashMap::new();
        let mut call_if: HashMap<String, (String, String)> = HashMap::new();
        while self.peek() == &Token::Call {
            self.advance(); // consume "call"
            let agent = self.expect_agent_ref()?;
            let idx = calls.len().to_string();
            // Check for per-call options block { output = "...", with = [...], if = "step.marker" }
            if self.peek() == &Token::LBrace {
                self.advance();
                let mut call_kvs = self.parse_kvs()?;
                self.expect(&Token::RBrace)?;
                if let Some(o) = call_kvs.get("output") {
                    call_outputs.insert(idx.clone(), o.as_str().to_string());
                }
                if let Some(w) = call_kvs.remove("with") {
                    call_with.insert(idx.clone(), w.into_string_array());
                }
                if let Some(v) = call_kvs.get("if") {
                    let s = v.as_str();
                    let (step_name, marker_name) = s.split_once('.').ok_or_else(|| {
                        format!("if value `{s}` must be in the form `step.marker` (no dot found)")
                    })?;
                    call_if.insert(idx, (step_name.to_string(), marker_name.to_string()));
                }
            }
            calls.push(agent);
        }
        self.expect(&Token::RBrace)?;

        if calls.is_empty() {
            return Err("parallel block must contain at least one call".to_string());
        }

        Ok(ParallelNode {
            fail_fast,
            min_success,
            calls,
            output,
            call_outputs,
            with: block_with,
            call_with,
            call_if,
        })
    }

    fn parse_gate(&mut self) -> std::result::Result<GateNode, String> {
        self.expect(&Token::Gate)?;
        let name = self.expect_ident()?;

        let gate_type = match name.as_str() {
            "human_approval" => GateType::HumanApproval,
            "human_review" => GateType::HumanReview,
            "pr_approval" => GateType::PrApproval,
            "pr_checks" => GateType::PrChecks,
            _ => return Err(format!(
                "Unknown gate type: '{}'. Expected one of: human_approval, human_review, pr_approval, pr_checks",
                name
            )),
        };

        self.expect(&Token::LBrace)?;
        let kvs = self.parse_kvs()?;
        self.expect(&Token::RBrace)?;

        let prompt = kvs.get("prompt").map(|v| v.as_str().to_string());
        let min_approvals = kvs
            .get("min_approvals")
            .map(|v| v.as_str().parse::<u32>())
            .transpose()
            .map_err(|e| format!("Invalid min_approvals: {e}"))?
            .unwrap_or(1);

        let approval_mode = match kvs.get("mode").map(|v| v.as_str()) {
            Some("review_decision") => ApprovalMode::ReviewDecision,
            Some("min_approvals") | None => ApprovalMode::MinApprovals,
            Some(other) => return Err(format!("Invalid mode for pr_approval: {other}")),
        };
        if approval_mode == ApprovalMode::ReviewDecision && kvs.contains_key("min_approvals") {
            return Err(
                "Cannot specify both mode = \"review_decision\" and min_approvals".to_string(),
            );
        }

        let timeout_secs = kvs
            .get("timeout")
            .map(|v| parse_duration_str(v.as_str()))
            .transpose()?
            .unwrap_or(24 * 3600); // default 24h

        let on_timeout = match kvs.get("on_timeout").map(|s| s.as_str()) {
            Some("continue") => OnTimeout::Continue,
            Some("fail") | None => OnTimeout::Fail,
            Some(other) => return Err(format!("Invalid on_timeout: {other}")),
        };

        let bot_name = kvs.get("as").map(|v| v.as_str().to_string());

        Ok(GateNode {
            name,
            gate_type,
            prompt,
            min_approvals,
            approval_mode,
            timeout_secs,
            on_timeout,
            bot_name,
        })
    }

    fn parse_always(&mut self) -> std::result::Result<AlwaysNode, String> {
        self.expect(&Token::Always)?;
        self.expect(&Token::LBrace)?;

        let mut body = Vec::new();
        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            body.push(self.parse_node()?);
        }
        self.expect(&Token::RBrace)?;

        Ok(AlwaysNode { body })
    }

    fn parse_script(&mut self) -> std::result::Result<ScriptNode, String> {
        self.expect(&Token::Script)?;
        let name = self.expect_ident()?;
        self.expect(&Token::LBrace)?;

        let mut kvs = self.parse_kvs()?;
        self.expect(&Token::RBrace)?;

        let run = kvs
            .remove("run")
            .ok_or_else(|| format!("script '{name}' is missing required `run` field"))?
            .into_string();

        let env = kvs
            .remove("env")
            .map(|v| match v {
                KvValue::Map(m) => Ok(m),
                _ => Err(format!(
                    "script '{name}': `env` must be a map `{{ KEY = \"value\" }}`"
                )),
            })
            .transpose()?
            .unwrap_or_default();

        let timeout = kvs
            .get("timeout")
            .map(|v| v.as_str().parse::<u64>())
            .transpose()
            .map_err(|e| format!("script '{name}': invalid timeout: {e}"))?;

        let retries = kvs
            .get("retries")
            .map(|v| v.as_str().parse::<u32>())
            .transpose()
            .map_err(|e| format!("script '{name}': invalid retries: {e}"))?
            .unwrap_or(0);

        let on_fail = kvs.remove("on_fail").map(|v| v.into_agent_ref());

        Ok(ScriptNode {
            name,
            run,
            env,
            timeout,
            retries,
            on_fail,
        })
    }
}

// ---------------------------------------------------------------------------
// Duration parser
// ---------------------------------------------------------------------------

/// Parse a human-readable duration string like "2h", "48h", "30m" into seconds.
pub(crate) fn parse_duration_str(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim().trim_matches('"');
    if let Some(hours) = s.strip_suffix('h') {
        let n: u64 = hours
            .parse()
            .map_err(|e| format!("Invalid duration '{s}': {e}"))?;
        n.checked_mul(3600)
            .ok_or_else(|| format!("Duration overflow: '{s}' exceeds u64 range"))
    } else if let Some(mins) = s.strip_suffix('m') {
        let n: u64 = mins
            .parse()
            .map_err(|e| format!("Invalid duration '{s}': {e}"))?;
        n.checked_mul(60)
            .ok_or_else(|| format!("Duration overflow: '{s}' exceeds u64 range"))
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse()
            .map_err(|e| format!("Invalid duration '{s}': {e}"))
    } else {
        // Try as raw seconds
        s.parse()
            .map_err(|e| format!("Invalid duration '{s}': {e}"))
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a `.wf` file into a `WorkflowDef`.
pub fn parse_workflow_file(path: &Path) -> Result<WorkflowDef> {
    let content = fs::read_to_string(path)
        .map_err(|e| ConductorError::Workflow(format!("Failed to read {}: {e}", path.display())))?;

    parse_workflow_str(&content, path.to_string_lossy().as_ref())
}

/// Parse a `.wf` DSL string into a `WorkflowDef`.
pub fn parse_workflow_str(input: &str, source_path: &str) -> Result<WorkflowDef> {
    let mut lexer = Lexer::new(input);
    let mut tokens = Vec::new();
    loop {
        let tok = lexer
            .next_token()
            .map_err(|e| ConductorError::Workflow(format!("Lexer error in {source_path}: {e}")))?;
        let is_eof = tok == Token::Eof;
        tokens.push(tok);
        if is_eof {
            break;
        }
    }

    let mut parser = Parser::new(tokens);
    let mut def = parser
        .parse_workflow()
        .map_err(|e| ConductorError::Workflow(format!("Parse error in {source_path}: {e}")))?;
    def.source_path = source_path.to_string();

    for warning in &parser.warnings {
        tracing::warn!("Warning in {source_path}: {warning}");
    }

    Ok(def)
}

/// Load all workflow definitions from `.conductor/workflows/*.wf`.
///
/// Returns `(defs, warnings)` where `warnings` contains one [`WorkflowWarning`]
/// per file that failed to parse. Callers receive all successfully-parsed
/// definitions even when some files are broken.
pub fn load_workflow_defs(
    worktree_path: &str,
    repo_path: &str,
) -> Result<(Vec<WorkflowDef>, Vec<WorkflowWarning>)> {
    let workflows_dir = match resolve_conductor_subdir(worktree_path, repo_path, "workflows") {
        Some(dir) => dir,
        None => return Ok((Vec::new(), Vec::new())),
    };

    let mut entries: Vec<_> = fs::read_dir(&workflows_dir)
        .map_err(|e| {
            ConductorError::Workflow(format!("Failed to read {}: {e}", workflows_dir.display()))
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "wf"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let mut defs = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries {
        let path = entry.path();
        match parse_workflow_file(&path) {
            Ok(def) => defs.push(def),
            Err(e) => {
                let file = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                tracing::warn!("Failed to parse {file}: {e}");
                warnings.push(WorkflowWarning {
                    file,
                    message: e.to_string(),
                });
            }
        }
    }
    Ok((defs, warnings))
}

/// Validate that a workflow name is safe for use in filesystem paths.
///
/// Only alphanumeric characters, hyphens, underscores, and dots (but not `..`)
/// are allowed. This prevents path traversal when names are used to construct
/// file paths.
pub fn validate_workflow_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConductorError::Workflow(
            "Workflow name must not be empty".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConductorError::Workflow(format!(
            "Invalid workflow name '{name}': only alphanumeric characters, hyphens, and underscores are allowed"
        )));
    }
    Ok(())
}

/// Load a single workflow definition by name.
pub fn load_workflow_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
) -> Result<WorkflowDef> {
    validate_workflow_name(name)?;

    let filename = format!("{name}.wf");
    let workflows_dir =
        resolve_conductor_subdir_for_file(worktree_path, repo_path, "workflows", &filename)
            .ok_or_else(|| {
                ConductorError::Workflow(format!(
                    "Workflow '{name}' not found in .conductor/workflows/"
                ))
            })?;

    parse_workflow_file(&workflows_dir.join(&filename))
}

/// Count the total number of nodes in a node list (for display).
fn count_nodes(nodes: &[WorkflowNode]) -> usize {
    let mut count = 0;
    for node in nodes {
        count += 1;
        match node {
            WorkflowNode::Call(_) | WorkflowNode::CallWorkflow(_) | WorkflowNode::Script(_) => {}
            WorkflowNode::If(n) => count += count_nodes(&n.body),
            WorkflowNode::Unless(n) => count += count_nodes(&n.body),
            WorkflowNode::While(n) => count += count_nodes(&n.body),
            WorkflowNode::DoWhile(n) => count += count_nodes(&n.body),
            WorkflowNode::Do(n) => count += count_nodes(&n.body),
            WorkflowNode::Parallel(n) => count += n.calls.len(),
            WorkflowNode::Gate(_) => {}
            WorkflowNode::Always(n) => count += count_nodes(&n.body),
        }
    }
    count
}

/// Collect all agent references in a node tree (for validation before execution).
pub fn collect_agent_names(nodes: &[WorkflowNode]) -> Vec<AgentRef> {
    let mut refs = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                refs.push(n.agent.clone());
                if let Some(ref f) = n.on_fail {
                    refs.push(f.clone());
                }
            }
            WorkflowNode::CallWorkflow(n) => {
                // on_fail agents are still agent refs
                if let Some(ref f) = n.on_fail {
                    refs.push(f.clone());
                }
            }
            WorkflowNode::Script(n) => {
                // on_fail agent ref (the script itself is not an agent)
                if let Some(ref f) = n.on_fail {
                    refs.push(f.clone());
                }
            }
            WorkflowNode::If(n) => refs.extend(collect_agent_names(&n.body)),
            WorkflowNode::Unless(n) => refs.extend(collect_agent_names(&n.body)),
            WorkflowNode::While(n) => refs.extend(collect_agent_names(&n.body)),
            WorkflowNode::DoWhile(n) => refs.extend(collect_agent_names(&n.body)),
            WorkflowNode::Do(n) => refs.extend(collect_agent_names(&n.body)),
            WorkflowNode::Parallel(n) => refs.extend(n.calls.iter().cloned()),
            WorkflowNode::Gate(_) => {}
            WorkflowNode::Always(n) => refs.extend(collect_agent_names(&n.body)),
        }
    }
    refs
}

/// Collect all prompt snippet references (`with` values) from a node tree.
pub fn collect_snippet_refs(nodes: &[WorkflowNode]) -> Vec<String> {
    let mut refs = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => refs.extend(n.with.iter().cloned()),
            WorkflowNode::Parallel(n) => {
                refs.extend(n.with.iter().cloned());
                for extra in n.call_with.values() {
                    refs.extend(extra.iter().cloned());
                }
            }
            WorkflowNode::If(n) => refs.extend(collect_snippet_refs(&n.body)),
            WorkflowNode::Unless(n) => refs.extend(collect_snippet_refs(&n.body)),
            WorkflowNode::While(n) => refs.extend(collect_snippet_refs(&n.body)),
            WorkflowNode::DoWhile(n) => refs.extend(collect_snippet_refs(&n.body)),
            WorkflowNode::Do(n) => {
                refs.extend(n.with.iter().cloned());
                refs.extend(collect_snippet_refs(&n.body));
            }
            WorkflowNode::Always(n) => refs.extend(collect_snippet_refs(&n.body)),
            WorkflowNode::CallWorkflow(_) | WorkflowNode::Gate(_) | WorkflowNode::Script(_) => {}
        }
    }
    refs
}

/// Collect all `call workflow` references in a node tree (for cycle detection).
pub fn collect_workflow_refs(nodes: &[WorkflowNode]) -> Vec<String> {
    let mut refs = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Call(_) | WorkflowNode::Gate(_) | WorkflowNode::Script(_) => {}
            WorkflowNode::CallWorkflow(n) => refs.push(n.workflow.clone()),
            WorkflowNode::If(n) => refs.extend(collect_workflow_refs(&n.body)),
            WorkflowNode::Unless(n) => refs.extend(collect_workflow_refs(&n.body)),
            WorkflowNode::While(n) => refs.extend(collect_workflow_refs(&n.body)),
            WorkflowNode::DoWhile(n) => refs.extend(collect_workflow_refs(&n.body)),
            WorkflowNode::Do(n) => refs.extend(collect_workflow_refs(&n.body)),
            WorkflowNode::Parallel(_) => {} // parallel only contains agent calls
            WorkflowNode::Always(n) => refs.extend(collect_workflow_refs(&n.body)),
        }
    }
    refs
}

/// Collect all output schema references (`output =` values) from a node tree.
pub fn collect_schema_refs(nodes: &[WorkflowNode]) -> Vec<String> {
    let mut refs = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                if let Some(ref s) = n.output {
                    refs.push(s.clone());
                }
            }
            WorkflowNode::Do(n) => {
                if let Some(ref s) = n.output {
                    refs.push(s.clone());
                }
                refs.extend(collect_schema_refs(&n.body));
            }
            WorkflowNode::Parallel(n) => {
                if let Some(ref s) = n.output {
                    refs.push(s.clone());
                }
                refs.extend(n.call_outputs.values().cloned());
            }
            WorkflowNode::If(n) => refs.extend(collect_schema_refs(&n.body)),
            WorkflowNode::Unless(n) => refs.extend(collect_schema_refs(&n.body)),
            WorkflowNode::While(n) => refs.extend(collect_schema_refs(&n.body)),
            WorkflowNode::DoWhile(n) => refs.extend(collect_schema_refs(&n.body)),
            WorkflowNode::Always(n) => refs.extend(collect_schema_refs(&n.body)),
            WorkflowNode::CallWorkflow(_) | WorkflowNode::Gate(_) | WorkflowNode::Script(_) => {}
        }
    }
    refs
}

/// Collect all bot names (`bot_name =` values) from a node tree.
pub fn collect_bot_names(nodes: &[WorkflowNode]) -> Vec<String> {
    let mut names = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                if let Some(ref b) = n.bot_name {
                    names.push(b.clone());
                }
            }
            WorkflowNode::CallWorkflow(n) => {
                if let Some(ref b) = n.bot_name {
                    names.push(b.clone());
                }
            }
            WorkflowNode::Gate(n) => {
                if let Some(ref b) = n.bot_name {
                    names.push(b.clone());
                }
            }
            WorkflowNode::If(n) => names.extend(collect_bot_names(&n.body)),
            WorkflowNode::Unless(n) => names.extend(collect_bot_names(&n.body)),
            WorkflowNode::While(n) => names.extend(collect_bot_names(&n.body)),
            WorkflowNode::DoWhile(n) => names.extend(collect_bot_names(&n.body)),
            WorkflowNode::Do(n) => names.extend(collect_bot_names(&n.body)),
            WorkflowNode::Parallel(_) | WorkflowNode::Script(_) => {}
            WorkflowNode::Always(n) => names.extend(collect_bot_names(&n.body)),
        }
    }
    names
}

/// Maximum allowed workflow nesting depth.
pub const MAX_WORKFLOW_DEPTH: u32 = 5;

/// Detect circular workflow references via static reachability analysis.
///
/// Returns `Ok(())` if no cycles exist, or an error naming the cycle path.
/// The `loader` callback loads a workflow by name — this keeps the function
/// testable without touching the filesystem.
pub fn detect_workflow_cycles<F>(root_name: &str, loader: &F) -> std::result::Result<(), String>
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    let mut visited = Vec::new();
    detect_cycles_inner(root_name, loader, &mut visited)
}

fn detect_cycles_inner<F>(
    name: &str,
    loader: &F,
    stack: &mut Vec<String>,
) -> std::result::Result<(), String>
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    if stack.contains(&name.to_string()) {
        stack.push(name.to_string());
        let cycle_path = stack.join(" -> ");
        return Err(format!("Circular workflow reference: {cycle_path}"));
    }

    if stack.len() >= MAX_WORKFLOW_DEPTH as usize {
        return Err(format!(
            "Workflow nesting depth exceeds maximum of {MAX_WORKFLOW_DEPTH}: {}",
            stack.join(" -> ")
        ));
    }

    stack.push(name.to_string());

    let def = loader(name)?;
    let mut child_refs = collect_workflow_refs(&def.body);
    child_refs.extend(collect_workflow_refs(&def.always));
    child_refs.sort();
    child_refs.dedup();

    for child_name in &child_refs {
        detect_cycles_inner(child_name, loader, stack)?;
    }

    stack.pop();
    Ok(())
}

// ---------------------------------------------------------------------------
// Semantic validation
// ---------------------------------------------------------------------------

/// A single semantic validation error found during static analysis of a workflow.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub message: String,
    /// Optional hint to help the user fix the error.
    pub hint: Option<String>,
}

/// The result of running `validate_workflow_semantics`.
#[derive(Debug, Default)]
pub struct ValidationReport {
    pub errors: Vec<ValidationError>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a `WorkflowDef` semantically:
///
/// 1. Forward-pass dataflow analysis: every condition reference (`step.marker`)
///    must name a step key that has been "produced" before that point.
/// 2. Sub-workflow required-input satisfaction: every `required` input declared
///    by a called sub-workflow must be supplied at the call site.
/// 3. Sub-workflow existence: if the loader returns an error the missing workflow
///    is reported as a validation error.
///
/// The `loader` callback receives a workflow name and returns its parsed
/// `WorkflowDef`, allowing this function to be tested without touching the
/// filesystem.
pub fn validate_workflow_semantics<F>(def: &WorkflowDef, loader: &F) -> ValidationReport
where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    let mut errors = Vec::new();
    let mut produced: HashSet<String> = HashSet::new();

    validate_nodes(&def.body, &mut produced, &mut errors, loader);

    // The `always` block sees every step key produced anywhere in the main body.
    let mut always_produced = produced.clone();
    validate_nodes(&def.always, &mut always_produced, &mut errors, loader);

    // Validate target values
    const VALID_TARGETS: &[&str] = &["worktree", "ticket", "repo", "pr", "workflow_run"];
    for target in &def.targets {
        if !VALID_TARGETS.contains(&target.as_str()) {
            errors.push(ValidationError {
                message: format!(
                    "Unknown target '{}' in workflow '{}'. Valid targets: {}",
                    target,
                    def.name,
                    VALID_TARGETS.join(", ")
                ),
                hint: Some(format!(
                    "Change '{}' to one of: {}",
                    target,
                    VALID_TARGETS.join(", ")
                )),
            });
        }
    }

    ValidationReport { errors }
}

fn validate_nodes<F>(
    nodes: &[WorkflowNode],
    produced: &mut HashSet<String>,
    errors: &mut Vec<ValidationError>,
    loader: &F,
) where
    F: Fn(&str) -> std::result::Result<WorkflowDef, String>,
{
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                produced.insert(n.agent.step_key());
            }
            WorkflowNode::CallWorkflow(n) => {
                // Check that required inputs are satisfied.
                match loader(&n.workflow) {
                    Ok(sub_def) => {
                        for input_decl in &sub_def.inputs {
                            if input_decl.required && !n.inputs.contains_key(&input_decl.name) {
                                errors.push(ValidationError {
                                    message: format!(
                                        "Sub-workflow '{}' requires input '{}' but it was not provided at the call site",
                                        n.workflow, input_decl.name
                                    ),
                                    hint: None,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        errors.push(ValidationError {
                            message: format!(
                                "Sub-workflow '{}' could not be loaded: {}",
                                n.workflow, e
                            ),
                            hint: None,
                        });
                    }
                }
                produced.insert(n.workflow.clone());
            }
            WorkflowNode::Parallel(n) => {
                // Validate `if` condition references before inserting produced keys,
                // since conditions must reference steps produced *before* this block.
                for (step_name, _marker) in n.call_if.values() {
                    check_condition_reachable(step_name, produced, errors);
                }
                for call in &n.calls {
                    produced.insert(call.step_key());
                }
            }
            WorkflowNode::If(n) => {
                check_condition_reachable(&n.step, produced, errors);
                let mut branch_produced = produced.clone();
                validate_nodes(&n.body, &mut branch_produced, errors, loader);
                // Conservative union: optimistically assume branch steps are available downstream.
                produced.extend(branch_produced);
            }
            WorkflowNode::Unless(n) => {
                check_condition_reachable(&n.step, produced, errors);
                let mut branch_produced = produced.clone();
                validate_nodes(&n.body, &mut branch_produced, errors, loader);
                produced.extend(branch_produced);
            }
            WorkflowNode::While(n) => {
                // Condition is checked before the first iteration.
                check_condition_reachable(&n.step, produced, errors);
                let mut body_produced = produced.clone();
                validate_nodes(&n.body, &mut body_produced, errors, loader);
                produced.extend(body_produced);
            }
            WorkflowNode::DoWhile(n) => {
                // Body always executes at least once before the condition is checked.
                validate_nodes(&n.body, produced, errors, loader);
                check_condition_reachable(&n.step, produced, errors);
            }
            WorkflowNode::Do(n) => {
                validate_nodes(&n.body, produced, errors, loader);
            }
            WorkflowNode::Gate(_) => {}
            WorkflowNode::Script(n) => {
                produced.insert(n.name.clone());
            }
            WorkflowNode::Always(n) => {
                // An Always node nested inside a body block sees the current produced set.
                validate_nodes(&n.body, produced, errors, loader);
            }
        }
    }
}

/// Emit a validation error if `step` has not yet been produced.
fn check_condition_reachable(
    step: &str,
    produced: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    if !produced.contains(step) {
        errors.push(ValidationError {
            message: format!(
                "Condition references step '{}' which has not been produced at this point in the workflow",
                step
            ),
            hint: Some(
                "Note: inner steps of called sub-workflows are not available in this context. \
                 Use the sub-workflow's own name (the key produced by `call workflow`) as the condition step."
                    .to_string(),
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_WORKFLOW: &str = r#"
workflow ticket-to-pr {
  meta {
    description = "Full development cycle"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  inputs {
    ticket_id  required
    skip_tests default = "false"
  }

  call plan

  call implement {
    retries = 2
    on_fail = diagnose
  }

  call push_and_pr
  call review

  while review.has_review_issues {
    max_iterations = 10
    stuck_after    = 3
    on_max_iter    = fail

    call address_reviews
    call push
    call review
  }

  parallel {
    fail_fast   = false
    min_success = 1
    call reviewer_security
    call reviewer_tests
    call reviewer_style
  }

  gate human_review {
    prompt     = "Review agent findings before merging. Add notes if needed."
    timeout    = "48h"
    on_timeout = fail
  }

  gate pr_checks {
    timeout    = "2h"
    on_timeout = fail
  }

  if review.has_critical_issues {
    call escalate
  }

  unless review.has_critical_issues {
    call fast-path
  }

  always {
    call notify_result
  }
}
"#;

    #[test]
    fn test_parse_full_workflow() {
        let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
        assert_eq!(def.name, "ticket-to-pr");
        assert_eq!(def.description, "Full development cycle");
        assert_eq!(def.trigger, WorkflowTrigger::Manual);

        // Inputs
        assert_eq!(def.inputs.len(), 2);
        assert_eq!(def.inputs[0].name, "ticket_id");
        assert!(def.inputs[0].required);
        assert_eq!(def.inputs[1].name, "skip_tests");
        assert!(!def.inputs[1].required);
        assert_eq!(def.inputs[1].default.as_deref(), Some("false"));

        // Body nodes: call plan, call implement, call push_and_pr, call review,
        //             while, parallel, gate human_review, gate pr_checks, if, unless
        assert_eq!(def.body.len(), 10);

        // call plan
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("plan".to_string()));
                assert_eq!(c.retries, 0);
                assert!(c.on_fail.is_none());
            }
            _ => panic!("Expected Call node"),
        }

        // call implement with retries
        match &def.body[1] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("implement".to_string()));
                assert_eq!(c.retries, 2);
                assert_eq!(c.on_fail, Some(AgentRef::Name("diagnose".to_string())));
            }
            _ => panic!("Expected Call node"),
        }

        // while loop
        match &def.body[4] {
            WorkflowNode::While(w) => {
                assert_eq!(w.step, "review");
                assert_eq!(w.marker, "has_review_issues");
                assert_eq!(w.max_iterations, 10);
                assert_eq!(w.stuck_after, Some(3));
                assert_eq!(w.on_max_iter, OnMaxIter::Fail);
                assert_eq!(w.body.len(), 3);
            }
            _ => panic!("Expected While node"),
        }

        // parallel
        match &def.body[5] {
            WorkflowNode::Parallel(p) => {
                assert!(!p.fail_fast);
                assert_eq!(p.min_success, Some(1));
                assert_eq!(
                    p.calls,
                    vec![
                        AgentRef::Name("reviewer_security".to_string()),
                        AgentRef::Name("reviewer_tests".to_string()),
                        AgentRef::Name("reviewer_style".to_string()),
                    ]
                );
            }
            _ => panic!("Expected Parallel node"),
        }

        // gate human_review
        match &def.body[6] {
            WorkflowNode::Gate(g) => {
                assert_eq!(g.gate_type, GateType::HumanReview);
                assert!(g.prompt.as_ref().unwrap().contains("Review agent findings"));
                assert_eq!(g.timeout_secs, 48 * 3600);
                assert_eq!(g.on_timeout, OnTimeout::Fail);
            }
            _ => panic!("Expected Gate node"),
        }

        // gate pr_checks
        match &def.body[7] {
            WorkflowNode::Gate(g) => {
                assert_eq!(g.gate_type, GateType::PrChecks);
                assert_eq!(g.timeout_secs, 2 * 3600);
            }
            _ => panic!("Expected Gate node"),
        }

        // if block
        match &def.body[8] {
            WorkflowNode::If(i) => {
                assert_eq!(i.step, "review");
                assert_eq!(i.marker, "has_critical_issues");
                assert_eq!(i.body.len(), 1);
            }
            _ => panic!("Expected If node"),
        }

        // unless block
        match &def.body[9] {
            WorkflowNode::Unless(u) => {
                assert_eq!(u.step, "review");
                assert_eq!(u.marker, "has_critical_issues");
                assert_eq!(u.body.len(), 1);
            }
            _ => panic!("Expected Unless node"),
        }

        // always
        assert_eq!(def.always.len(), 1);
        match &def.always[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("notify_result".to_string()))
            }
            _ => panic!("Expected Call node in always"),
        }
    }

    #[test]
    fn test_parse_minimal_workflow() {
        let input = "workflow simple { meta { targets = [\"worktree\"] } call build }";
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.name, "simple");
        assert_eq!(def.body.len(), 1);
        assert!(def.always.is_empty());
        assert!(def.inputs.is_empty());
    }

    #[test]
    fn test_while_requires_max_iterations() {
        let input = r#"
            workflow test {
                while step.marker {
                    call something
                }
            }
        "#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("max_iterations"));
    }

    #[test]
    fn test_unknown_gate_type() {
        let input = r#"
            workflow test {
                gate unknown_type {
                    timeout = "1h"
                }
            }
        "#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("Unknown gate type"));
    }

    #[test]
    fn test_parallel_requires_calls() {
        let input = r#"
            workflow test {
                parallel {
                    fail_fast = true
                }
            }
        "#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("at least one call"));
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_str("2h").unwrap(), 7200);
        assert_eq!(parse_duration_str("48h").unwrap(), 172800);
        assert_eq!(parse_duration_str("30m").unwrap(), 1800);
        assert_eq!(parse_duration_str("60s").unwrap(), 60);
        assert_eq!(parse_duration_str("3600").unwrap(), 3600);
    }

    #[test]
    fn test_comments_ignored() {
        let input = r#"
            // This is a comment
            workflow test {
                meta { targets = ["worktree"] }
                // Another comment
                call build // inline comment
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.name, "test");
        assert_eq!(def.body.len(), 1);
    }

    #[test]
    fn test_collect_agent_names() {
        let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
        let mut refs = collect_agent_names(&def.body);
        refs.extend(collect_agent_names(&def.always));
        assert!(refs.contains(&AgentRef::Name("plan".to_string())));
        assert!(refs.contains(&AgentRef::Name("implement".to_string())));
        assert!(refs.contains(&AgentRef::Name("diagnose".to_string()))); // on_fail
        assert!(refs.contains(&AgentRef::Name("reviewer_security".to_string())));
        assert!(refs.contains(&AgentRef::Name("notify_result".to_string())));
    }

    #[test]
    fn test_count_nodes() {
        let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
        let body_count = count_nodes(&def.body);
        // 10 top-level + 3 in while + 3 in parallel + 1 in if + 1 in unless = 18
        assert_eq!(body_count, 18);
        // total_nodes covers body + always
        assert_eq!(def.total_nodes(), body_count + count_nodes(&def.always));
    }

    #[test]
    fn test_load_from_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("simple.wf"),
            "workflow simple { meta { targets = [\"worktree\"] } call build }",
        )
        .unwrap();

        let (defs, warnings) =
            load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "simple");
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_load_partial_failure_returns_successes_and_warnings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wf_dir).unwrap();
        // Valid workflow
        fs::write(
            wf_dir.join("good.wf"),
            "workflow good { meta { targets = [\"worktree\"] } call build }",
        )
        .unwrap();
        // Invalid workflow (syntax error)
        fs::write(
            wf_dir.join("bad.wf"),
            "this is not valid workflow syntax !!!",
        )
        .unwrap();

        let (defs, warnings) =
            load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
        // The good workflow is returned despite the bad one failing
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "good");
        // One warning for the bad file
        assert_eq!(warnings.len(), 1);
        // Warning carries the filename in the structured `file` field
        assert_eq!(warnings[0].file, "bad.wf");
        assert!(!warnings[0].message.is_empty());
    }

    #[test]
    fn test_serialization_roundtrip() {
        let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
        let json = serde_json::to_string(&def).unwrap();
        let restored: WorkflowDef = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.name, def.name);
        assert_eq!(restored.body.len(), def.body.len());
    }

    #[test]
    fn test_parse_ticket_to_pr_wf() {
        let input = r#"
workflow ticket-to-pr {
  meta {
    description = "Full development cycle — plan from ticket, implement, push PR, run review swarm, iterate until clean"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  inputs {
    ticket_id required
  }

  call plan { output = "task-plan" }

  call implement {
    retries = 2
  }

  call push-and-pr

  parallel {
    output    = "review-findings"
    with      = ["review-diff-scope"]
    fail_fast = false
    call review-architecture
    call review-security
    call review-performance
    call review-dry-abstraction
    call review-error-handling
    call review-test-coverage
    call review-db-migrations
  }

  call review-aggregator { output = "review-aggregator" }

  while review-aggregator.has_review_issues {
    max_iterations = 3
    stuck_after    = 2
    on_max_iter    = fail

    call address-reviews

    parallel {
      output    = "review-findings"
      with      = ["review-diff-scope"]
      fail_fast = false
      call review-architecture
      call review-security
      call review-performance
      call review-dry-abstraction
      call review-error-handling
      call review-test-coverage
      call review-db-migrations
    }

    call review-aggregator { output = "review-aggregator" }
  }
}
"#;
        let def = parse_workflow_str(input, "ticket-to-pr.wf").unwrap();
        assert_eq!(def.name, "ticket-to-pr");
        assert_eq!(def.trigger, WorkflowTrigger::Manual);
        assert_eq!(def.inputs.len(), 1);
        assert!(def.inputs[0].required);
        // call plan, call implement, call push-and-pr, parallel, call review-aggregator, while
        assert_eq!(def.body.len(), 6);

        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("plan".to_string()));
                assert_eq!(c.output.as_deref(), Some("task-plan"));
            }
            _ => panic!("Expected Call node for plan"),
        }

        match &def.body[1] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("implement".to_string()));
                assert_eq!(c.retries, 2);
            }
            _ => panic!("Expected Call node"),
        }

        match &def.body[3] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.calls.len(), 7);
                assert!(!p.fail_fast);
                assert_eq!(p.with, vec!["review-diff-scope".to_string()]);
                assert_eq!(p.output.as_deref(), Some("review-findings"));
            }
            _ => panic!("Expected Parallel node"),
        }

        match &def.body[4] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("review-aggregator".to_string()));
                assert_eq!(c.output.as_deref(), Some("review-aggregator"));
            }
            _ => panic!("Expected Call node for review-aggregator"),
        }

        match &def.body[5] {
            WorkflowNode::While(w) => {
                assert_eq!(w.step, "review-aggregator");
                assert_eq!(w.marker, "has_review_issues");
                assert_eq!(w.max_iterations, 3);
                assert_eq!(w.stuck_after, Some(2));
                // address-reviews, parallel, review-aggregator
                assert_eq!(w.body.len(), 3);
            }
            _ => panic!("Expected While node"),
        }
    }

    #[test]
    fn test_parse_test_coverage_wf() {
        let input = r#"
workflow test-coverage {
  meta {
    description = "Validate PR has sufficient tests; write and commit missing ones"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  call analyze-coverage

  if analyze-coverage.has_missing_tests {
    call write-tests
  }
}
"#;
        let def = parse_workflow_str(input, "test-coverage.wf").unwrap();
        assert_eq!(def.name, "test-coverage");
        assert_eq!(def.body.len(), 2);

        match &def.body[1] {
            WorkflowNode::If(i) => {
                assert_eq!(i.step, "analyze-coverage");
                assert_eq!(i.marker, "has_missing_tests");
                assert_eq!(i.body.len(), 1);
            }
            _ => panic!("Expected If node"),
        }
    }

    #[test]
    fn test_parse_lint_fix_wf() {
        let input = r#"
workflow lint-fix {
  meta {
    description = "Analyze lint errors and apply fixes"
    trigger     = "manual"
    targets     = ["worktree"]
  }

  call analyze-lint

  if analyze-lint.has_lint_errors {
    call lint-fix-impl
  }
}
"#;
        let def = parse_workflow_str(input, "lint-fix.wf").unwrap();
        assert_eq!(def.name, "lint-fix");
        assert_eq!(def.body.len(), 2);

        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("analyze-lint".to_string()))
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_validate_workflow_name_valid() {
        assert!(validate_workflow_name("ticket-to-pr").is_ok());
        assert!(validate_workflow_name("test_coverage").is_ok());
        assert!(validate_workflow_name("simple").is_ok());
        assert!(validate_workflow_name("A-Z_0-9").is_ok());
    }

    #[test]
    fn test_validate_workflow_name_empty() {
        assert!(validate_workflow_name("").is_err());
    }

    #[test]
    fn test_validate_workflow_name_path_traversal() {
        assert!(validate_workflow_name("..").is_err());
        assert!(validate_workflow_name("../etc/passwd").is_err());
        assert!(validate_workflow_name("foo/bar").is_err());
        assert!(validate_workflow_name("foo\\bar").is_err());
    }

    #[test]
    fn test_validate_workflow_name_special_chars() {
        assert!(validate_workflow_name("name with spaces").is_err());
        assert!(validate_workflow_name("name.wf").is_err());
        assert!(validate_workflow_name("name;rm -rf").is_err());
        assert!(validate_workflow_name("name\0null").is_err());
    }

    #[test]
    fn test_load_workflow_by_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("deploy.wf"),
            "workflow deploy { meta { targets = [\"worktree\"] } call build }",
        )
        .unwrap();

        let def =
            load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "deploy").unwrap();
        assert_eq!(def.name, "deploy");
    }

    #[test]
    fn test_load_workflow_by_name_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("deploy.wf"),
            "workflow deploy { meta { targets = [\"worktree\"] } call build }",
        )
        .unwrap();

        let result =
            load_workflow_by_name(tmp.path().to_str().unwrap(), "/nonexistent", "nonexistent");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not found"));
    }

    #[test]
    fn test_load_workflow_by_name_rejects_invalid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = load_workflow_by_name(
            tmp.path().to_str().unwrap(),
            "/nonexistent",
            "../etc/passwd",
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid workflow name"));
    }

    #[test]
    fn test_load_workflow_by_name_falls_back_to_repo_path() {
        let repo = tempfile::TempDir::new().unwrap();
        let wf_dir = repo.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("deploy.wf"),
            "workflow deploy { meta { targets = [\"worktree\"] } call build }",
        )
        .unwrap();

        // worktree has no .conductor/workflows/, should fall back to repo_path
        let worktree = tempfile::TempDir::new().unwrap();
        let def = load_workflow_by_name(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "deploy",
        )
        .unwrap();
        assert_eq!(def.name, "deploy");
    }

    #[test]
    fn test_load_workflow_by_name_no_workflows_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = load_workflow_by_name(
            tmp.path().to_str().unwrap(),
            tmp.path().to_str().unwrap(),
            "deploy",
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not found"));
    }

    #[test]
    fn test_parse_call_explicit_path() {
        let input =
            r#"workflow test { meta { targets = ["worktree"] } call ".claude/agents/review.md" }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(
                    c.agent,
                    AgentRef::Path(".claude/agents/review.md".to_string())
                );
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_parse_call_mixed_name_and_path() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan
    call ".claude/agents/code-review.md"
    call implement { retries = 1  on_fail = ".claude/agents/diagnose.md" }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 3);
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("plan".to_string()));
            }
            _ => panic!("Expected Call node"),
        }
        match &def.body[1] {
            WorkflowNode::Call(c) => {
                assert_eq!(
                    c.agent,
                    AgentRef::Path(".claude/agents/code-review.md".to_string())
                );
            }
            _ => panic!("Expected Call node"),
        }
        match &def.body[2] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("implement".to_string()));
                assert_eq!(c.retries, 1);
                assert_eq!(
                    c.on_fail,
                    Some(AgentRef::Path(".claude/agents/diagnose.md".to_string()))
                );
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_parse_parallel_explicit_paths() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    parallel {
        call reviewer-security
        call ".claude/agents/code-review.md"
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(
                    p.calls,
                    vec![
                        AgentRef::Name("reviewer-security".to_string()),
                        AgentRef::Path(".claude/agents/code-review.md".to_string()),
                    ]
                );
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_agent_ref_label() {
        assert_eq!(AgentRef::Name("plan".to_string()).label(), "plan");
        assert_eq!(
            AgentRef::Path(".claude/agents/plan.md".to_string()).label(),
            ".claude/agents/plan.md"
        );
    }

    #[test]
    fn test_agent_ref_step_key() {
        // Name variants: step_key == label
        assert_eq!(AgentRef::Name("plan".to_string()).step_key(), "plan");

        // Path variants: step_key is the file stem (no extension)
        assert_eq!(
            AgentRef::Path(".claude/agents/plan.md".to_string()).step_key(),
            "plan"
        );
        assert_eq!(
            AgentRef::Path(".claude/agents/code-review.md".to_string()).step_key(),
            "code-review"
        );
        // Nested subdir — still just the stem
        assert_eq!(
            AgentRef::Path("custom/dir/my-agent.md".to_string()).step_key(),
            "my-agent"
        );
    }

    /// A quoted bare name (no `/`) in `on_fail` should produce `AgentRef::Name`,
    /// not `AgentRef::Path` — quoting alone does not make a value a path.
    #[test]
    fn test_on_fail_quoted_bare_name_is_name() {
        let input = r#"workflow test { meta { targets = ["worktree"] } call agent { on_fail = "diagnose" } }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(
                    c.on_fail,
                    Some(AgentRef::Name("diagnose".to_string())),
                    "quoted on_fail value without a slash should be AgentRef::Name"
                );
            }
            _ => panic!("Expected Call node"),
        }
    }

    /// A bare (unquoted) name in `on_fail` should produce `AgentRef::Name`.
    #[test]
    fn test_on_fail_bare_name_is_name() {
        let input = r#"workflow test { meta { targets = ["worktree"] } call agent { on_fail = diagnose } }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.on_fail, Some(AgentRef::Name("diagnose".to_string())));
            }
            _ => panic!("Expected Call node"),
        }
    }

    // -----------------------------------------------------------------------
    // call workflow tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_call_workflow_simple() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow lint-fix
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::CallWorkflow(n) => {
                assert_eq!(n.workflow, "lint-fix");
                assert!(n.inputs.is_empty());
                assert_eq!(n.retries, 0);
                assert!(n.on_fail.is_none());
            }
            _ => panic!("Expected CallWorkflow node"),
        }
    }

    #[test]
    fn test_parse_call_workflow_with_inputs() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow test-coverage {
        inputs {
            pr_url = "{{pr_url}}"
            branch = "main"
        }
        retries = 1
        on_fail = notify-lint-failure
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::CallWorkflow(n) => {
                assert_eq!(n.workflow, "test-coverage");
                assert_eq!(n.inputs.get("pr_url").unwrap(), "{{pr_url}}");
                assert_eq!(n.inputs.get("branch").unwrap(), "main");
                assert_eq!(n.retries, 1);
                assert_eq!(
                    n.on_fail,
                    Some(AgentRef::Name("notify-lint-failure".to_string()))
                );
            }
            _ => panic!("Expected CallWorkflow node"),
        }
    }

    #[test]
    fn test_parse_call_workflow_as_before_inputs() {
        // Regression: `as =` before `inputs { }` used to silently drop the workflow
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow ticket-to-pr {
        as = "developer"
        inputs {
            ticket_id = "{{ticket_id}}"
        }
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::CallWorkflow(n) => {
                assert_eq!(n.workflow, "ticket-to-pr");
                assert_eq!(n.inputs.get("ticket_id").unwrap(), "{{ticket_id}}");
                assert_eq!(n.bot_name.as_deref(), Some("developer"));
            }
            _ => panic!("Expected CallWorkflow node"),
        }
    }

    #[test]
    fn test_parse_call_workflow_no_block() {
        let input = "workflow parent { meta { targets = [\"worktree\"] } call workflow child }";
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::CallWorkflow(n) => {
                assert_eq!(n.workflow, "child");
                assert!(n.inputs.is_empty());
            }
            _ => panic!("Expected CallWorkflow node"),
        }
    }

    #[test]
    fn test_parse_mixed_call_and_call_workflow() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call plan
    call workflow lint-fix
    call implement
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 3);
        assert!(matches!(&def.body[0], WorkflowNode::Call(_)));
        assert!(matches!(&def.body[1], WorkflowNode::CallWorkflow(_)));
        assert!(matches!(&def.body[2], WorkflowNode::Call(_)));
    }

    #[test]
    fn test_parse_call_workflow_in_if_block() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    if analyze.needs_lint {
        call workflow lint-fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 2);
        match &def.body[1] {
            WorkflowNode::If(i) => {
                assert_eq!(i.body.len(), 1);
                assert!(matches!(&i.body[0], WorkflowNode::CallWorkflow(_)));
            }
            _ => panic!("Expected If node"),
        }
    }

    #[test]
    fn test_parse_unless() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call analyze
    unless analyze.has_errors {
        call deploy
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 2);
        match &def.body[1] {
            WorkflowNode::Unless(u) => {
                assert_eq!(u.step, "analyze");
                assert_eq!(u.marker, "has_errors");
                assert_eq!(u.body.len(), 1);
                assert!(matches!(&u.body[0], WorkflowNode::Call(_)));
            }
            _ => panic!("Expected Unless node"),
        }
    }

    #[test]
    fn test_parse_call_workflow_in_unless_block() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    unless analyze.needs_lint {
        call workflow lint-fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 2);
        match &def.body[1] {
            WorkflowNode::Unless(u) => {
                assert_eq!(u.body.len(), 1);
                assert!(matches!(&u.body[0], WorkflowNode::CallWorkflow(_)));
            }
            _ => panic!("Expected Unless node"),
        }
    }

    #[test]
    fn test_collect_workflow_refs() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call plan
    call workflow lint-fix
    if plan.needs_tests {
        call workflow test-coverage
    }
    always {
        call workflow notify
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let mut refs = collect_workflow_refs(&def.body);
        refs.extend(collect_workflow_refs(&def.always));
        refs.sort();
        assert_eq!(refs, vec!["lint-fix", "notify", "test-coverage"]);
    }

    #[test]
    fn test_detect_workflow_cycles_no_cycle() {
        let result = detect_workflow_cycles("a", &|name| match name {
            "a" => parse_workflow_str(
                "workflow a { meta { targets = [\"worktree\"] } call workflow b }",
                "a.wf",
            )
            .map_err(|e| e.to_string()),
            "b" => parse_workflow_str(
                "workflow b { meta { targets = [\"worktree\"] } call agent }",
                "b.wf",
            )
            .map_err(|e| e.to_string()),
            other => Err(format!("Unknown workflow: {other}")),
        });
        assert!(result.is_ok());
    }

    #[test]
    fn test_detect_workflow_cycles_direct_cycle() {
        let result = detect_workflow_cycles("a", &|name| match name {
            "a" => parse_workflow_str(
                "workflow a { meta { targets = [\"worktree\"] } call workflow b }",
                "a.wf",
            )
            .map_err(|e| e.to_string()),
            "b" => parse_workflow_str(
                "workflow b { meta { targets = [\"worktree\"] } call workflow a }",
                "b.wf",
            )
            .map_err(|e| e.to_string()),
            other => Err(format!("Unknown workflow: {other}")),
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Circular workflow reference"));
        assert!(err.contains("a -> b -> a"));
    }

    #[test]
    fn test_detect_workflow_cycles_self_reference() {
        let result = detect_workflow_cycles("a", &|name| match name {
            "a" => parse_workflow_str(
                "workflow a { meta { targets = [\"worktree\"] } call workflow a }",
                "a.wf",
            )
            .map_err(|e| e.to_string()),
            other => Err(format!("Unknown workflow: {other}")),
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Circular workflow reference"));
        assert!(err.contains("a -> a"));
    }

    #[test]
    fn test_detect_workflow_cycles_transitive() {
        let result = detect_workflow_cycles("a", &|name| match name {
            "a" => parse_workflow_str(
                "workflow a { meta { targets = [\"worktree\"] } call workflow b }",
                "a.wf",
            )
            .map_err(|e| e.to_string()),
            "b" => parse_workflow_str(
                "workflow b { meta { targets = [\"worktree\"] } call workflow c }",
                "b.wf",
            )
            .map_err(|e| e.to_string()),
            "c" => parse_workflow_str(
                "workflow c { meta { targets = [\"worktree\"] } call workflow a }",
                "c.wf",
            )
            .map_err(|e| e.to_string()),
            other => Err(format!("Unknown workflow: {other}")),
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("a -> b -> c -> a"));
    }

    #[test]
    fn test_detect_workflow_cycles_depth_limit() {
        // Build a chain of 6 workflows (exceeds MAX_WORKFLOW_DEPTH of 5)
        let result = detect_workflow_cycles("w0", &|name| {
            let idx: usize = name[1..].parse().unwrap();
            if idx < 6 {
                let next = format!("w{}", idx + 1);
                let src = format!("workflow {name} {{ meta {{ targets = [\"worktree\"] }} call workflow {next} }}");
                parse_workflow_str(&src, &format!("{name}.wf")).map_err(|e| e.to_string())
            } else {
                let src =
                    format!("workflow {name} {{ meta {{ targets = [\"worktree\"] }} call agent }}");
                parse_workflow_str(&src, &format!("{name}.wf")).map_err(|e| e.to_string())
            }
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("nesting depth exceeds"));
    }

    #[test]
    fn test_call_workflow_serialization_roundtrip() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow test-coverage {
        inputs { pr_url = "https://example.com" }
        retries = 2
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let json = serde_json::to_string(&def).unwrap();
        let restored: WorkflowDef = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.body.len(), 1);
        match &restored.body[0] {
            WorkflowNode::CallWorkflow(n) => {
                assert_eq!(n.workflow, "test-coverage");
                assert_eq!(n.inputs.get("pr_url").unwrap(), "https://example.com");
                assert_eq!(n.retries, 2);
            }
            _ => panic!("Expected CallWorkflow node after deserialization"),
        }
    }

    #[test]
    fn test_parse_call_workflow_in_while_block() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    while analyze.needs_fixes {
        max_iterations = 3
        call workflow lint-fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 2);
        match &def.body[1] {
            WorkflowNode::While(w) => {
                assert_eq!(w.body.len(), 1);
                match &w.body[0] {
                    WorkflowNode::CallWorkflow(n) => assert_eq!(n.workflow, "lint-fix"),
                    _ => panic!("Expected CallWorkflow node inside while"),
                }
            }
            _ => panic!("Expected While node"),
        }
    }

    #[test]
    fn test_collect_workflow_refs_in_while() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    while analyze.needs_fixes {
        max_iterations = 3
        call workflow lint-fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_workflow_refs(&def.body);
        assert_eq!(refs, vec!["lint-fix"]);
    }

    #[test]
    fn test_collect_agent_names_call_workflow_on_fail() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow lint-fix {
        on_fail = recovery-agent
    }
    call workflow test-coverage
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_agent_names(&def.body);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0], AgentRef::Name("recovery-agent".to_string()));
    }

    #[test]
    fn test_parse_call_workflow_in_always_block() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call build
    always {
        call workflow notify
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.always.len(), 1);
        match &def.always[0] {
            WorkflowNode::CallWorkflow(n) => assert_eq!(n.workflow, "notify"),
            _ => panic!("Expected CallWorkflow node inside always"),
        }
    }

    #[test]
    fn test_collect_workflow_refs_in_always() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call build
    always {
        call workflow notify
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let body_refs = collect_workflow_refs(&def.body);
        let always_refs = collect_workflow_refs(&def.always);
        assert!(body_refs.is_empty());
        assert_eq!(always_refs, vec!["notify"]);
    }

    /// A quoted string without a `/` in `call` position should produce
    /// `AgentRef::Path`, not `AgentRef::Name`.  In `call` position, quoting is
    /// always a deliberate signal that the value is an explicit path, so the
    /// slash-heuristic used by `KvValue::into_agent_ref` does not apply.
    #[test]
    fn test_call_quoted_bare_name_is_path() {
        let input = r#"workflow test { meta { targets = ["worktree"] } call "diagnose" }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(
                    c.agent,
                    AgentRef::Path("diagnose".to_string()),
                    "quoted agent in call position should always be AgentRef::Path"
                );
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_call_with_output_option() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call review-security { output = "review-findings" }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, AgentRef::Name("review-security".to_string()));
                assert_eq!(c.output.as_deref(), Some("review-findings"));
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_call_with_output_and_retries() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call review { output = "review-findings" retries = 2 }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.output.as_deref(), Some("review-findings"));
                assert_eq!(c.retries, 2);
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_call_without_output() {
        let input = r#"workflow test { meta { targets = ["worktree"] } call plan }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert!(c.output.is_none());
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_parallel_with_block_level_output() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            parallel {
                output = "review-findings"
                fail_fast = false
                call review-security
                call review-style
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.output.as_deref(), Some("review-findings"));
                assert_eq!(p.calls.len(), 2);
                assert!(!p.fail_fast);
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_parallel_with_per_call_output_override() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            parallel {
                output = "review-findings"
                call review-security
                call lint-check { output = "lint-results" }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.output.as_deref(), Some("review-findings"));
                assert_eq!(p.calls.len(), 2);
                assert!(p.call_outputs.is_empty() || !p.call_outputs.contains_key("0"));
                assert_eq!(
                    p.call_outputs.get("1").map(|s| s.as_str()),
                    Some("lint-results")
                );
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_call_with_single_snippet() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call plan { with = "ticket-context" }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.with, vec!["ticket-context".to_string()]);
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_call_with_array_snippets() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call plan { with = ["ticket-context", "rust-conventions"] }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(
                    c.with,
                    vec!["ticket-context".to_string(), "rust-conventions".to_string()]
                );
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_parallel_with_block_level_snippets() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            parallel {
                with      = ["review-diff-scope", "rust-conventions"]
                fail_fast = false
                call review-security
                call review-style
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(
                    p.with,
                    vec![
                        "review-diff-scope".to_string(),
                        "rust-conventions".to_string()
                    ]
                );
                assert!(p.call_with.is_empty());
                assert_eq!(p.calls.len(), 2);
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_parallel_with_per_call_snippets() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            parallel {
                with = ["review-diff-scope"]
                call ".conductor/agents/review-architecture.md"
                call ".conductor/agents/review-db-migrations.md" { with = ["migration-rules"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.with, vec!["review-diff-scope".to_string()]);
                assert!(!p.call_with.contains_key("0"));
                assert_eq!(
                    p.call_with.get("1").unwrap(),
                    &vec!["migration-rules".to_string()]
                );
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_parallel_if_parsed() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call detect-db-migrations
            parallel {
                fail_fast = false
                call review-security    { retries = 1 }
                call review-db-migrations { retries = 1 if = "detect-db-migrations.has_db_migrations" }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[1] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.calls.len(), 2);
                assert!(!p.call_if.contains_key("0"));
                assert_eq!(
                    p.call_if.get("1"),
                    Some(&(
                        "detect-db-migrations".to_string(),
                        "has_db_migrations".to_string()
                    ))
                );
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_parallel_call_if_snapshot_roundtrip() {
        // Regression test: HashMap<String, (String, String)> must survive serde_json
        // serialize → deserialize. Previously the key type was HashMap<usize, ...> which
        // caused "invalid type: string "6", expected usize" on resume because JSON object
        // keys are always strings and serde_json's MapKeyDeserializer does not coerce
        // string keys to integer types.
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call detect-db-migrations
            call detect-file-types
            parallel {
                fail_fast = false
                call review-architecture    { retries = 1 }
                call review-dry-abstraction { retries = 1 }
                call review-security        { retries = 1 if = "detect-file-types.has_code_changes" }
                call review-performance     { retries = 1 if = "detect-file-types.has_code_changes" }
                call review-error-handling  { retries = 1 if = "detect-file-types.has_code_changes" }
                call review-test-coverage   { retries = 1 if = "detect-file-types.has_code_changes" }
                call review-db-migrations   { retries = 1 if = "detect-db-migrations.has_db_migrations" }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        // Serialize to JSON (as stored in the DB snapshot) and deserialize back.
        let json = serde_json::to_string(&def).expect("serialize failed");
        let def2: WorkflowDef = serde_json::from_str(&json).expect(
            "deserialize failed — HashMap<String, (String, String)> must round-trip through JSON",
        );
        match &def2.body[2] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.calls.len(), 7);
                // call_if should survive the round-trip with correct string keys
                assert_eq!(
                    p.call_if.get("6"),
                    Some(&(
                        "detect-db-migrations".to_string(),
                        "has_db_migrations".to_string()
                    ))
                );
                assert_eq!(
                    p.call_if.get("2"),
                    Some(&(
                        "detect-file-types".to_string(),
                        "has_code_changes".to_string()
                    ))
                );
            }
            _ => panic!("Expected Parallel node at index 2"),
        }
    }

    #[test]
    fn test_parallel_if_malformed_no_dot() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            parallel {
                call review-db-migrations { if = "no-dot-here" }
            }
        }"#;
        let err = parse_workflow_str(input, "test.wf").unwrap_err();
        assert!(
            err.to_string().contains("step.marker"),
            "Expected error about step.marker format, got: {err}"
        );
    }

    #[test]
    fn test_parallel_if_with_output_and_with() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call detect-check
            parallel {
                output = "findings"
                with   = ["scope"]
                fail_fast = false
                call agent-a { retries = 1 }
                call agent-b { output = "b-out" with = ["extra"] if = "detect-check.flag" }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[1] {
            WorkflowNode::Parallel(p) => {
                assert_eq!(p.output.as_deref(), Some("findings"));
                assert_eq!(p.with, vec!["scope".to_string()]);
                assert!(!p.call_if.contains_key("0"));
                assert_eq!(
                    p.call_if.get("1"),
                    Some(&("detect-check".to_string(), "flag".to_string()))
                );
                assert_eq!(p.call_outputs.get("1").map(|s| s.as_str()), Some("b-out"));
                assert_eq!(p.call_with.get("1"), Some(&vec!["extra".to_string()]));
            }
            _ => panic!("Expected Parallel node"),
        }
    }

    #[test]
    fn test_parallel_if_validation_step_not_produced() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            parallel {
                call review-db-migrations { if = "detect-db-migrations.has_db_migrations" }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let no_loader = |_: &str| Err("not found".to_string());
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            !report.is_ok(),
            "Expected validation error for unreachable step"
        );
        assert!(
            report.errors[0].message.contains("detect-db-migrations"),
            "Expected error mentioning detect-db-migrations, got: {}",
            report.errors[0].message
        );
    }

    #[test]
    fn test_parallel_if_validation_step_produced() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call detect-db-migrations
            parallel {
                call review-db-migrations { if = "detect-db-migrations.has_db_migrations" }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let no_loader = |_: &str| Err("not found".to_string());
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            report.is_ok(),
            "Expected no validation errors, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn test_collect_snippet_refs() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call plan { with = ["context-a"] }
            parallel {
                with = ["scope-b"]
                call agent-1
                call agent-2 { with = ["extra-c"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let mut refs = collect_snippet_refs(&def.body);
        refs.sort();
        refs.dedup();
        assert_eq!(
            refs,
            vec![
                "context-a".to_string(),
                "extra-c".to_string(),
                "scope-b".to_string(),
            ]
        );
    }

    #[test]
    fn test_call_with_no_snippets() {
        let input = r#"workflow test { meta { targets = ["worktree"] } call plan }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert!(c.with.is_empty());
            }
            _ => panic!("Expected Call node"),
        }
    }

    #[test]
    fn test_collect_snippet_refs_inside_if() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call plan
            if plan.approved {
                call implement { with = ["if-context"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_snippet_refs(&def.body);
        assert_eq!(refs, vec!["if-context".to_string()]);
    }

    #[test]
    fn test_collect_snippet_refs_inside_unless() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call review
            unless review.approved {
                call fix { with = ["unless-context"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_snippet_refs(&def.body);
        assert_eq!(refs, vec!["unless-context".to_string()]);
    }

    #[test]
    fn test_collect_snippet_refs_inside_while() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call review
            while review.has_issues {
                max_iterations = 3
                call fix { with = ["while-context"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_snippet_refs(&def.body);
        assert_eq!(refs, vec!["while-context".to_string()]);
    }

    #[test]
    fn test_collect_snippet_refs_inside_always() {
        // Top-level `always { }` block is parsed into `def.always`, not `def.body`.
        // collect_all_snippet_refs() covers both; here we test the always slice directly.
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call plan
            always {
                call cleanup { with = ["always-context"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_snippet_refs(&def.always);
        assert_eq!(refs, vec!["always-context".to_string()]);
    }

    #[test]
    fn test_parse_do_while() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call analyze
    do {
        max_iterations = 3
        stuck_after    = 2
        on_max_iter    = continue
        call diagnose
        call fix
    } while analyze.needs_retry
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 2);
        match &def.body[1] {
            WorkflowNode::DoWhile(n) => {
                assert_eq!(n.step, "analyze");
                assert_eq!(n.marker, "needs_retry");
                assert_eq!(n.max_iterations, 3);
                assert_eq!(n.stuck_after, Some(2));
                assert_eq!(n.on_max_iter, OnMaxIter::Continue);
                assert_eq!(n.body.len(), 2);
            }
            _ => panic!("Expected DoWhile node"),
        }
    }

    #[test]
    fn test_parse_do_while_requires_max_iterations() {
        // New syntax: missing max_iterations after `while` clause
        let input = r#"workflow test { do { call baz } while foo.bar }"#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_iterations"));
    }

    #[test]
    fn test_parse_do_while_old_syntax_gives_hint() {
        // Old syntax (do x.y { ... }) must produce a clear error with a hint.
        let input = r#"workflow test { do foo.bar { call baz } }"#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("expected `{` after `do`"), "msg={msg}");
        assert!(msg.contains("do { ... } while x.y"), "msg={msg}");
    }

    #[test]
    fn test_parse_do_while_invalid_on_max_iter() {
        let input = r#"workflow test { do { max_iterations = 3  on_max_iter = explode  call baz } while foo.bar }"#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid on_max_iter"));
    }

    #[test]
    fn test_parse_do_while_serde_roundtrip() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call check
    do {
        max_iterations = 2
        call fix
    } while check.has_issues
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("\"type\":\"do_while\""));
        // Deserialize and verify round-trip
        let def2: WorkflowDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def2.body.len(), def.body.len());
        match &def2.body[1] {
            WorkflowNode::DoWhile(n) => {
                assert_eq!(n.step, "check");
                assert_eq!(n.marker, "has_issues");
                assert_eq!(n.max_iterations, 2);
            }
            _ => panic!("Expected DoWhile node after roundtrip"),
        }
    }

    #[test]
    fn test_collect_snippet_refs_inside_do_while() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            do {
                max_iterations = 2
                call fix { with = ["do-while-context"] }
            } while check.has_issues
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_snippet_refs(&def.body);
        assert_eq!(refs, vec!["do-while-context".to_string()]);
    }

    #[test]
    fn test_collect_agent_names_inside_do_while() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            do {
                max_iterations = 2
                call fix
                call verify
            } while check.has_issues
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_agent_names(&def.body);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0], AgentRef::Name("fix".to_string()));
        assert_eq!(refs[1], AgentRef::Name("verify".to_string()));
    }

    #[test]
    fn test_collect_workflow_refs_in_do_while() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call analyze
    do {
        max_iterations = 3
        call workflow lint-fix
    } while analyze.needs_fixes
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_workflow_refs(&def.body);
        assert_eq!(refs, vec!["lint-fix"]);
    }

    #[test]
    fn test_parse_plain_do_block() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    do {
        output = "review-result"
        with   = ["shared-context", "extra"]
        call reviewer_a
        call reviewer_b
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::Do(n) => {
                assert_eq!(n.output.as_deref(), Some("review-result"));
                assert_eq!(n.with, vec!["shared-context", "extra"]);
                assert_eq!(n.body.len(), 2);
                // Verify body contains the expected calls
                match &n.body[0] {
                    WorkflowNode::Call(c) => {
                        assert_eq!(c.agent, AgentRef::Name("reviewer_a".to_string()))
                    }
                    _ => panic!("Expected Call node"),
                }
            }
            _ => panic!("Expected Do node"),
        }
    }

    #[test]
    fn test_parse_plain_do_block_minimal() {
        // Plain do block with no options — just grouping
        let input = r#"workflow test { meta { targets = ["worktree"] } do { call build } }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::Do(n) => {
                assert!(n.output.is_none());
                assert!(n.with.is_empty());
                assert_eq!(n.body.len(), 1);
            }
            _ => panic!("Expected Do node"),
        }
    }

    #[test]
    fn test_parse_plain_do_block_rejects_unknown_keys() {
        let input = r#"workflow test { do { max_iterations = 5 call build } }"#;
        let err_msg = parse_workflow_str(input, "test.wf")
            .unwrap_err()
            .to_string();
        assert!(
            err_msg.contains("unknown option"),
            "expected unknown option error, got: {err_msg}"
        );
    }

    #[test]
    fn test_collect_snippet_refs_inside_do_block() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            do {
                with = ["block-snippet"]
                call fix { with = ["call-snippet"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = collect_snippet_refs(&def.body);
        // Should include both the do-block's `with` and the inner call's `with`
        assert!(refs.contains(&"block-snippet".to_string()));
        assert!(refs.contains(&"call-snippet".to_string()));
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_collect_all_snippet_refs_deduplicates_across_body_and_always() {
        let input = r#"workflow test {
            meta { targets = ["worktree"] }
            call plan { with = ["shared-context", "body-only"] }
            always {
                call cleanup { with = ["shared-context", "always-only"] }
            }
        }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let refs = def.collect_all_snippet_refs();
        // Should be sorted and deduplicated: "shared-context" appears in both blocks
        assert_eq!(
            refs,
            vec![
                "always-only".to_string(),
                "body-only".to_string(),
                "shared-context".to_string(),
            ]
        );
    }

    // -----------------------------------------------------------------------
    // Semantic validation tests
    // -----------------------------------------------------------------------

    fn no_loader(name: &str) -> std::result::Result<WorkflowDef, String> {
        Err(format!("no loader: {name}"))
    }

    #[test]
    fn test_semantics_valid_simple() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan
    call implement
    while plan.has_issues {
        max_iterations = 3
        call fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            report.is_ok(),
            "Expected no errors, got: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_semantics_condition_unreachable() {
        // `review-aggregator` was never produced — only `review-pr` was
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call workflow review-pr
    if review-aggregator.has_review_issues {
        call fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &|name| {
            if name == "review-pr" {
                parse_workflow_str(
                    "workflow review-pr { meta { description = \"r\" trigger = \"manual\" targets = [\"worktree\"] } call review-aggregator }",
                    "review-pr.wf",
                )
                .map_err(|e| e.to_string())
            } else {
                Err(format!("unknown: {name}"))
            }
        });
        assert!(!report.is_ok());
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].message.contains("review-aggregator"));
        assert!(report.errors[0].hint.is_some());
    }

    #[test]
    fn test_semantics_condition_ok_from_do_while() {
        // check is produced inside do-while body; condition references it after body runs
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    do {
        max_iterations = 3
        call check
        call fix
    } while check.needs_retry
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            report.is_ok(),
            "Expected no errors, got: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_semantics_condition_inner_step_hint() {
        // The step referenced in the condition is an inner step of a sub-workflow —
        // the error must mention the step name and include a hint.
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow review-pr
    while review-aggregator.has_review_issues {
        max_iterations = 3
        call fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &|name| {
            if name == "review-pr" {
                parse_workflow_str(
                    "workflow review-pr { meta { description = \"r\" trigger = \"manual\" targets = [\"worktree\"] } call review-aggregator }",
                    "review-pr.wf",
                )
                .map_err(|e| e.to_string())
            } else {
                Err(format!("unknown: {name}"))
            }
        });
        assert!(!report.is_ok());
        let err = &report.errors[0];
        assert!(err.message.contains("review-aggregator"));
        assert!(err.hint.is_some());
        let hint = err.hint.as_ref().unwrap();
        assert!(hint.contains("sub-workflow"));
    }

    #[test]
    fn test_semantics_missing_required_input() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow child
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &|name| {
            if name == "child" {
                parse_workflow_str(
                    r#"workflow child {
                        meta { description = "c" trigger = "manual" targets = ["worktree"] }
                        inputs { ticket_id required }
                        call do-work
                    }"#,
                    "child.wf",
                )
                .map_err(|e| e.to_string())
            } else {
                Err(format!("unknown: {name}"))
            }
        });
        assert!(!report.is_ok());
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].message.contains("ticket_id"));
        assert!(report.errors[0].message.contains("child"));
    }

    #[test]
    fn test_semantics_provided_required_input_ok() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow child {
        inputs { ticket_id = "{{ticket_id}}" }
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &|name| {
            if name == "child" {
                parse_workflow_str(
                    r#"workflow child {
                        meta { description = "c" trigger = "manual" targets = ["worktree"] }
                        inputs { ticket_id required }
                        call do-work
                    }"#,
                    "child.wf",
                )
                .map_err(|e| e.to_string())
            } else {
                Err(format!("unknown: {name}"))
            }
        });
        assert!(
            report.is_ok(),
            "Expected no errors, got: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_semantics_sub_workflow_not_found() {
        let input = r#"
workflow parent {
    meta { targets = ["worktree"] }
    call workflow missing-workflow
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(!report.is_ok());
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].message.contains("missing-workflow"));
    }

    #[test]
    fn test_semantics_always_block_sees_full_produced() {
        // `plan` and `implement` are produced in the body; `always` can reference `plan`
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan
    call implement
    always {
        if plan.has_issues {
            call notify
        }
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            report.is_ok(),
            "Expected no errors, got: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_semantics_parallel_produces_step_keys() {
        let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    parallel {
        call reviewer-security
        call reviewer-style
    }
    if reviewer-security.has_issues {
        call fix
    }
}
"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            report.is_ok(),
            "Expected no errors, got: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_validate_known_targets_accepted() {
        for target in &["worktree", "ticket", "repo", "pr", "workflow_run"] {
            let input =
                format!("workflow test {{ meta {{ targets = [\"{target}\"] }} call step }}",);
            let def = parse_workflow_str(&input, "test.wf").unwrap();
            let report = validate_workflow_semantics(&def, &no_loader);
            assert!(
                report.is_ok(),
                "target '{target}' should be valid, got errors: {:?}",
                report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_validate_unknown_target_rejected() {
        let input = r#"workflow test { meta { targets = ["foobar"] } call step }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            !report.is_ok(),
            "unknown target 'foobar' should produce a validation error"
        );
        let msg = &report.errors[0].message;
        assert!(
            msg.contains("foobar"),
            "error should mention the bad target, got: {msg}"
        );
    }

    #[test]
    fn test_validate_multiple_targets_with_one_unknown() {
        let input = r#"workflow test { meta { targets = ["worktree", "badtarget"] } call step }"#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let report = validate_workflow_semantics(&def, &no_loader);
        assert!(
            !report.is_ok(),
            "mixed valid/invalid targets should produce a validation error"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("badtarget")),
            "error should name the unknown target"
        );
    }

    #[test]
    fn test_parse_gate_review_decision_mode() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                gate pr_approval {
                    mode = "review_decision"
                    timeout = "1h"
                }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::Gate(g) => {
                assert_eq!(g.approval_mode, ApprovalMode::ReviewDecision);
                assert_eq!(g.gate_type, GateType::PrApproval);
            }
            other => panic!("Expected Gate node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_gate_min_approvals_mode_explicit() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                gate pr_approval {
                    mode = "min_approvals"
                    min_approvals = 2
                    timeout = "1h"
                }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::Gate(g) => {
                assert_eq!(g.approval_mode, ApprovalMode::MinApprovals);
                assert_eq!(g.min_approvals, 2);
            }
            other => panic!("Expected Gate node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_gate_invalid_mode_rejected() {
        let input = r#"
            workflow test {
                gate pr_approval {
                    mode = "banana"
                    timeout = "1h"
                }
            }
        "#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("Invalid mode"), "got: {err}");
    }

    #[test]
    fn test_parse_gate_review_decision_with_min_approvals_rejected() {
        let input = r#"
            workflow test {
                gate pr_approval {
                    mode = "review_decision"
                    min_approvals = 2
                    timeout = "1h"
                }
            }
        "#;
        let result = parse_workflow_str(input, "test.wf");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("Cannot specify both"),
            "expected conflict error, got: {err}"
        );
    }

    #[test]
    fn test_parse_call_with_bot_name() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                call my_agent { as = "developer" }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::Call(c) => {
                assert_eq!(c.bot_name.as_deref(), Some("developer"));
            }
            other => panic!("Expected Call node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_call_without_bot_name() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                call my_agent {}
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::Call(c) => {
                assert!(c.bot_name.is_none(), "bot_name should be None when omitted");
            }
            other => panic!("Expected Call node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_call_workflow_with_bot_name() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                call workflow sub-workflow { as = "reviewer" }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::CallWorkflow(cw) => {
                assert_eq!(cw.bot_name.as_deref(), Some("reviewer"));
            }
            other => panic!("Expected CallWorkflow node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_call_workflow_without_bot_name() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                call workflow sub-workflow {}
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::CallWorkflow(cw) => {
                assert!(
                    cw.bot_name.is_none(),
                    "bot_name should be None when omitted"
                );
            }
            other => panic!("Expected CallWorkflow node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_gate_with_bot_name() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                gate pr_approval {
                    mode = "review_decision"
                    timeout = "1h"
                    as = "reviewer"
                }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::Gate(g) => {
                assert_eq!(g.bot_name.as_deref(), Some("reviewer"));
            }
            other => panic!("Expected Gate node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_gate_without_bot_name() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                gate pr_checks {
                    timeout = "30m"
                }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let node = def.body.first().unwrap();
        match node {
            WorkflowNode::Gate(g) => {
                assert!(g.bot_name.is_none(), "bot_name should be None when omitted");
            }
            other => panic!("Expected Gate node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_call_bot_name_serde_roundtrip() {
        let input = r#"
            workflow test {
                meta { targets = ["worktree"] }
                call my_agent { as = "developer" }
                call workflow sub { as = "reviewer" }
            }
        "#;
        let def = parse_workflow_str(input, "test.wf").unwrap();
        let json = serde_json::to_string(&def).unwrap();
        let restored: WorkflowDef = serde_json::from_str(&json).unwrap();
        match restored.body.first().unwrap() {
            WorkflowNode::Call(c) => assert_eq!(c.bot_name.as_deref(), Some("developer")),
            other => panic!("Expected Call, got {other:?}"),
        }
        match restored.body.get(1).unwrap() {
            WorkflowNode::CallWorkflow(cw) => {
                assert_eq!(cw.bot_name.as_deref(), Some("reviewer"))
            }
            other => panic!("Expected CallWorkflow, got {other:?}"),
        }
    }

    #[test]
    fn test_input_with_description_remains_required() {
        // Regression: a description modifier must not silently change required→optional.
        let src = r#"
workflow w {
    meta { trigger = "manual" targets = ["worktree"] }
    inputs {
        bare_required
        explicit_required required
        with_description description = "some help text"
        with_desc_and_required required description = "help"
        with_default default = "x"
    }
    call agent
}
"#;
        let def = parse_workflow_str(src, "test.wf").unwrap();
        assert_eq!(def.inputs.len(), 5);

        // bare identifier → required
        assert_eq!(def.inputs[0].name, "bare_required");
        assert!(def.inputs[0].required, "bare input should be required");
        assert!(def.inputs[0].default.is_none());
        assert!(def.inputs[0].description.is_none());

        // explicit `required` keyword
        assert_eq!(def.inputs[1].name, "explicit_required");
        assert!(def.inputs[1].required);

        // description alone must NOT make the input optional
        assert_eq!(def.inputs[2].name, "with_description");
        assert!(
            def.inputs[2].required,
            "input with only a description must still be required"
        );
        assert_eq!(def.inputs[2].description.as_deref(), Some("some help text"));
        assert!(def.inputs[2].default.is_none());

        // explicit required + description
        assert_eq!(def.inputs[3].name, "with_desc_and_required");
        assert!(def.inputs[3].required);
        assert_eq!(def.inputs[3].description.as_deref(), Some("help"));

        // default makes it optional
        assert_eq!(def.inputs[4].name, "with_default");
        assert!(!def.inputs[4].required);
        assert_eq!(def.inputs[4].default.as_deref(), Some("x"));
    }

    // ---------------------------------------------------------------------------
    // collect_schema_refs / collect_bot_names
    // ---------------------------------------------------------------------------

    #[test]
    fn test_collect_schema_refs_empty() {
        assert!(collect_schema_refs(&[]).is_empty());
    }

    /// Minimal workflow header that satisfies the parser's required fields.
    const WF_HEADER: &str = r#"meta { targets = ["worktree"] }"#;

    fn make_wf(body: &str) -> String {
        format!("workflow w {{\n  {WF_HEADER}\n{body}\n}}")
    }

    #[test]
    fn test_collect_schema_refs_call_node() {
        let src = make_wf(
            r#"  call plan { output = "review-findings" }
  call build"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let refs = collect_schema_refs(&def.body);
        assert_eq!(refs, vec!["review-findings"]);
    }

    #[test]
    fn test_collect_schema_refs_nested_if() {
        let src = make_wf(
            r#"  call plan { output = "plan-output" }
  if plan.ready {
    call implement { output = "impl-result" }
  }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let refs = collect_schema_refs(&def.body);
        assert!(refs.contains(&"plan-output".to_string()));
        assert!(refs.contains(&"impl-result".to_string()));
    }

    #[test]
    fn test_collect_schema_refs_parallel_node() {
        let src = make_wf(
            r#"  parallel {
    output = "shared-schema"
    call reviewer_a
    call reviewer_b
  }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let refs = collect_schema_refs(&def.body);
        assert!(refs.contains(&"shared-schema".to_string()));
    }

    #[test]
    fn test_collect_all_schema_refs_includes_always_block() {
        let src = make_wf(
            r#"  call plan { output = "plan-schema" }
  always {
    call notify { output = "notify-schema" }
  }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let refs = def.collect_all_schema_refs();
        assert!(
            refs.contains(&"plan-schema".to_string()),
            "body schema missing"
        );
        assert!(
            refs.contains(&"notify-schema".to_string()),
            "always schema missing"
        );
    }

    #[test]
    fn test_collect_all_schema_refs_deduplicates() {
        let src = make_wf(
            r#"  call step_a { output = "shared" }
  call step_b { output = "shared" }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let refs = def.collect_all_schema_refs();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0], "shared");
    }

    #[test]
    fn test_collect_bot_names_empty() {
        assert!(collect_bot_names(&[]).is_empty());
    }

    #[test]
    fn test_collect_bot_names_call_node() {
        let src = make_wf(
            r#"  call plan { as = "conductor-ai" }
  call build"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let names = collect_bot_names(&def.body);
        assert_eq!(names, vec!["conductor-ai"]);
    }

    #[test]
    fn test_collect_bot_names_nested_blocks() {
        let src = make_wf(
            r#"  if step.marker {
    call act { as = "my-bot" }
  }
  while step.marker {
    max_iterations = 3
    on_max_iter = fail
    call retry { as = "my-bot" }
  }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let names = collect_bot_names(&def.body);
        // both calls have the same bot name — raw list has two entries
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| n == "my-bot"));
    }

    #[test]
    fn test_collect_all_bot_names_includes_always_block() {
        let src = make_wf(
            r#"  call plan { as = "main-bot" }
  always {
    call cleanup { as = "always-bot" }
  }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let names = def.collect_all_bot_names();
        assert!(names.contains(&"main-bot".to_string()), "body bot missing");
        assert!(
            names.contains(&"always-bot".to_string()),
            "always bot missing"
        );
    }

    #[test]
    fn test_collect_all_bot_names_deduplicates() {
        let src = make_wf(
            r#"  call step_a { as = "shared-bot" }
  call step_b { as = "shared-bot" }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        let names = def.collect_all_bot_names();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "shared-bot");
    }

    // -----------------------------------------------------------------------
    // parse_script tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_script_happy_path() {
        let src = make_wf(r#"  script my-step { run = "scripts/build.sh" }"#);
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        assert_eq!(def.body.len(), 1);
        match &def.body[0] {
            WorkflowNode::Script(s) => {
                assert_eq!(s.name, "my-step");
                assert_eq!(s.run, "scripts/build.sh");
                assert!(s.env.is_empty());
                assert!(s.timeout.is_none());
                assert_eq!(s.retries, 0);
                assert!(s.on_fail.is_none());
            }
            other => panic!("expected Script node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_script_with_all_fields() {
        let src = make_wf(
            r#"  script build {
    run = "ci/build.sh"
    timeout = "120"
    retries = "2"
    env = { CI = "true" BRANCH = "main" }
  }"#,
        );
        let def = parse_workflow_str(&src, "w.wf").unwrap();
        match &def.body[0] {
            WorkflowNode::Script(s) => {
                assert_eq!(s.run, "ci/build.sh");
                assert_eq!(s.timeout, Some(120));
                assert_eq!(s.retries, 2);
                assert_eq!(s.env.get("CI").map(|s| s.as_str()), Some("true"));
                assert_eq!(s.env.get("BRANCH").map(|s| s.as_str()), Some("main"));
            }
            other => panic!("expected Script node, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_script_missing_run_field() {
        let src = make_wf(r#"  script my-step { timeout = "30" }"#);
        let err = parse_workflow_str(&src, "w.wf").unwrap_err();
        assert!(
            err.to_string().contains("missing required `run` field"),
            "expected 'missing required `run` field', got: {err}"
        );
    }

    #[test]
    fn test_parse_script_invalid_timeout() {
        let src = make_wf(r#"  script my-step { run = "x.sh" timeout = "not-a-number" }"#);
        let err = parse_workflow_str(&src, "w.wf").unwrap_err();
        assert!(
            err.to_string().contains("invalid timeout"),
            "expected 'invalid timeout', got: {err}"
        );
    }

    #[test]
    fn test_parse_script_invalid_retries() {
        let src = make_wf(r#"  script my-step { run = "x.sh" retries = "bad" }"#);
        let err = parse_workflow_str(&src, "w.wf").unwrap_err();
        assert!(
            err.to_string().contains("invalid retries"),
            "expected 'invalid retries', got: {err}"
        );
    }
}
