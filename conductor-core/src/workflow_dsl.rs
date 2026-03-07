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
//! node          := call | if_node | while_node | parallel | gate | always
//! call          := "call" IDENT ("{" kv* "}")?
//! if_node       := "if" condition "{" kv* node* "}"
//! while_node    := "while" condition "{" kv* node* "}"
//! parallel      := "parallel" "{" kv* call* "}"
//! gate          := "gate" IDENT "{" kv* "}"
//! always        := "always" "{" node* "}"
//! condition     := IDENT "." IDENT
//! kv            := IDENT "=" (STRING | NUMBER | IDENT)
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ConductorError, Result};

// ---------------------------------------------------------------------------
// AST types
// ---------------------------------------------------------------------------

/// A complete workflow definition parsed from a `.wf` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    pub description: String,
    pub trigger: WorkflowTrigger,
    pub inputs: Vec<InputDecl>,
    pub body: Vec<WorkflowNode>,
    pub always: Vec<WorkflowNode>,
    pub source_path: String,
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
}

/// A node in the workflow execution graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowNode {
    Call(CallNode),
    If(IfNode),
    While(WhileNode),
    Parallel(ParallelNode),
    Gate(GateNode),
    Always(AlwaysNode),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallNode {
    pub agent: String,
    #[serde(default)]
    pub retries: u32,
    pub on_fail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfNode {
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
    pub calls: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateNode {
    pub name: String,
    pub gate_type: GateType,
    pub prompt: Option<String>,
    #[serde(default = "default_one")]
    pub min_approvals: u32,
    pub timeout_secs: u64,
    pub on_timeout: OnTimeout,
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
    While,
    Parallel,
    Gate,
    Always,
    Required,
    Default,
    // Punctuation
    LBrace,
    RBrace,
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
            "while" => Token::While,
            "parallel" => Token::Parallel,
            "gate" => Token::Gate,
            "always" => Token::Always,
            "required" => Token::Required,
            "default" => Token::Default,
            _ => Token::Ident(s),
        })
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
            other => Err(format!("Expected identifier, got {other:?}")),
        }
    }

    fn expect_value(&mut self) -> std::result::Result<String, String> {
        match self.advance() {
            Token::StringLit(s) => Ok(s),
            Token::Int(n) => Ok(n.to_string()),
            Token::Ident(s) => Ok(s),
            // Allow keyword tokens as values
            Token::Required => Ok("required".to_string()),
            Token::Default => Ok("default".to_string()),
            Token::Call => Ok("call".to_string()),
            Token::If => Ok("if".to_string()),
            Token::While => Ok("while".to_string()),
            Token::Parallel => Ok("parallel".to_string()),
            Token::Gate => Ok("gate".to_string()),
            Token::Always => Ok("always".to_string()),
            other => Err(format!(
                "Expected value (string, int, or ident), got {other:?}"
            )),
        }
    }

    // Parse key-value pairs until we hit a non-kv token.
    // KV pairs look like: IDENT = VALUE
    fn parse_kvs(&mut self) -> std::result::Result<HashMap<String, String>, String> {
        let mut kvs = HashMap::new();
        loop {
            // Peek ahead: if it's an ident/keyword followed by '=', it's a kv.
            if self.pos + 1 < self.tokens.len() {
                let is_kv_key = matches!(
                    self.peek(),
                    Token::Ident(_) | Token::Required | Token::Default
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
                        description = desc.clone();
                    }
                    if let Some(trig) = kvs.get("trigger") {
                        trigger = trig
                            .parse::<WorkflowTrigger>()
                            .map_err(|e| format!("In meta block: {e}"))?;
                        if trigger != WorkflowTrigger::Manual {
                            self.warnings.push(format!(
                                "trigger '{}' is not implemented in v1; only 'manual' is active",
                                trig
                            ));
                        }
                    }
                }
                Token::Inputs => {
                    self.advance();
                    self.expect(&Token::LBrace)?;
                    while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                        let input_name = self.expect_ident()?;
                        match self.peek() {
                            Token::Required => {
                                self.advance();
                                inputs.push(InputDecl {
                                    name: input_name,
                                    required: true,
                                    default: None,
                                });
                            }
                            Token::Default => {
                                self.advance();
                                self.expect(&Token::Equals)?;
                                let value = self.expect_value()?;
                                inputs.push(InputDecl {
                                    name: input_name,
                                    required: false,
                                    default: Some(value),
                                });
                            }
                            _ => {
                                // Bare identifier — treat as required
                                inputs.push(InputDecl {
                                    name: input_name,
                                    required: true,
                                    default: None,
                                });
                            }
                        }
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

        Ok(WorkflowDef {
            name,
            description,
            trigger,
            inputs,
            body,
            always,
            source_path: String::new(),
        })
    }

    fn parse_node(&mut self) -> std::result::Result<WorkflowNode, String> {
        match self.peek() {
            Token::Call => self.parse_call().map(WorkflowNode::Call),
            Token::If => self.parse_if().map(WorkflowNode::If),
            Token::While => self.parse_while().map(WorkflowNode::While),
            Token::Parallel => self.parse_parallel().map(WorkflowNode::Parallel),
            Token::Gate => self.parse_gate().map(WorkflowNode::Gate),
            Token::Always => self.parse_always().map(WorkflowNode::Always),
            other => Err(format!(
                "Expected a workflow node (call, if, while, parallel, gate, always), got {other:?}"
            )),
        }
    }

    fn parse_call(&mut self) -> std::result::Result<CallNode, String> {
        self.expect(&Token::Call)?;
        let agent = self.expect_ident()?;

        let mut retries = 0u32;
        let mut on_fail = None;

        if self.peek() == &Token::LBrace {
            self.advance();
            let kvs = self.parse_kvs()?;
            self.expect(&Token::RBrace)?;

            if let Some(r) = kvs.get("retries") {
                retries = r.parse().map_err(|e| format!("Invalid retries: {e}"))?;
            }
            if let Some(f) = kvs.get("on_fail") {
                on_fail = Some(f.clone());
            }
        }

        Ok(CallNode {
            agent,
            retries,
            on_fail,
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

    fn parse_while(&mut self) -> std::result::Result<WhileNode, String> {
        self.expect(&Token::While)?;
        let (step, marker) = self.parse_condition()?;
        self.expect(&Token::LBrace)?;

        let kvs = self.parse_kvs()?;

        let max_iterations = kvs
            .get("max_iterations")
            .ok_or("while loop requires max_iterations")?
            .parse::<u32>()
            .map_err(|e| format!("Invalid max_iterations: {e}"))?;

        let stuck_after = kvs
            .get("stuck_after")
            .map(|v| v.parse::<u32>())
            .transpose()
            .map_err(|e| format!("Invalid stuck_after: {e}"))?;

        let on_max_iter = match kvs.get("on_max_iter").map(|s| s.as_str()) {
            Some("continue") => OnMaxIter::Continue,
            Some("fail") | None => OnMaxIter::Fail,
            Some(other) => return Err(format!("Invalid on_max_iter: {other}")),
        };

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

    fn parse_parallel(&mut self) -> std::result::Result<ParallelNode, String> {
        self.expect(&Token::Parallel)?;
        self.expect(&Token::LBrace)?;

        let kvs = self.parse_kvs()?;

        let fail_fast = kvs.get("fail_fast").map(|v| v == "true").unwrap_or(true);

        let min_success = kvs
            .get("min_success")
            .map(|v| v.parse::<u32>())
            .transpose()
            .map_err(|e| format!("Invalid min_success: {e}"))?;

        let mut calls = Vec::new();
        while self.peek() == &Token::Call {
            self.advance(); // consume "call"
            let agent = self.expect_ident()?;
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

        let prompt = kvs.get("prompt").cloned();
        let min_approvals = kvs
            .get("min_approvals")
            .map(|v| v.parse::<u32>())
            .transpose()
            .map_err(|e| format!("Invalid min_approvals: {e}"))?
            .unwrap_or(1);

        let timeout_secs = kvs
            .get("timeout")
            .map(|v| parse_duration_str(v))
            .transpose()?
            .unwrap_or(24 * 3600); // default 24h

        let on_timeout = match kvs.get("on_timeout").map(|s| s.as_str()) {
            Some("continue") => OnTimeout::Continue,
            Some("fail") | None => OnTimeout::Fail,
            Some(other) => return Err(format!("Invalid on_timeout: {other}")),
        };

        Ok(GateNode {
            name,
            gate_type,
            prompt,
            min_approvals,
            timeout_secs,
            on_timeout,
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
}

// ---------------------------------------------------------------------------
// Duration parser
// ---------------------------------------------------------------------------

/// Parse a human-readable duration string like "2h", "48h", "30m" into seconds.
fn parse_duration_str(s: &str) -> std::result::Result<u64, String> {
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
        eprintln!("[workflow] Warning in {source_path}: {warning}");
    }

    Ok(def)
}

/// Load all workflow definitions from `.conductor/workflows/*.wf`.
pub fn load_workflow_defs(worktree_path: &str, repo_path: &str) -> Result<Vec<WorkflowDef>> {
    let worktree_dir = PathBuf::from(worktree_path)
        .join(".conductor")
        .join("workflows");
    let workflows_dir = if worktree_dir.is_dir() {
        worktree_dir
    } else {
        let repo_dir = PathBuf::from(repo_path)
            .join(".conductor")
            .join("workflows");
        if !repo_dir.is_dir() {
            return Ok(Vec::new());
        }
        repo_dir
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
    for entry in entries {
        defs.push(parse_workflow_file(&entry.path())?);
    }
    Ok(defs)
}

/// Load a single workflow definition by name.
pub fn load_workflow_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
) -> Result<WorkflowDef> {
    let defs = load_workflow_defs(worktree_path, repo_path)?;
    defs.into_iter().find(|d| d.name == name).ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Workflow '{name}' not found in .conductor/workflows/"
        ))
    })
}

/// Count the total number of nodes in a workflow (for display).
pub fn count_nodes(nodes: &[WorkflowNode]) -> usize {
    let mut count = 0;
    for node in nodes {
        count += 1;
        match node {
            WorkflowNode::Call(_) => {}
            WorkflowNode::If(n) => count += count_nodes(&n.body),
            WorkflowNode::While(n) => count += count_nodes(&n.body),
            WorkflowNode::Parallel(n) => count += n.calls.len(),
            WorkflowNode::Gate(_) => {}
            WorkflowNode::Always(n) => count += count_nodes(&n.body),
        }
    }
    count
}

/// Collect all agent names referenced in a node tree (for validation).
pub fn collect_agent_names(nodes: &[WorkflowNode]) -> Vec<String> {
    let mut names = Vec::new();
    for node in nodes {
        match node {
            WorkflowNode::Call(n) => {
                names.push(n.agent.clone());
                if let Some(ref f) = n.on_fail {
                    names.push(f.clone());
                }
            }
            WorkflowNode::If(n) => names.extend(collect_agent_names(&n.body)),
            WorkflowNode::While(n) => names.extend(collect_agent_names(&n.body)),
            WorkflowNode::Parallel(n) => names.extend(n.calls.iter().cloned()),
            WorkflowNode::Gate(_) => {}
            WorkflowNode::Always(n) => names.extend(collect_agent_names(&n.body)),
        }
    }
    names
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
        //             while, parallel, gate human_review, gate pr_checks, if
        assert_eq!(def.body.len(), 9);

        // call plan
        match &def.body[0] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, "plan");
                assert_eq!(c.retries, 0);
                assert!(c.on_fail.is_none());
            }
            _ => panic!("Expected Call node"),
        }

        // call implement with retries
        match &def.body[1] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, "implement");
                assert_eq!(c.retries, 2);
                assert_eq!(c.on_fail.as_deref(), Some("diagnose"));
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
                    vec!["reviewer_security", "reviewer_tests", "reviewer_style"]
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

        // always
        assert_eq!(def.always.len(), 1);
        match &def.always[0] {
            WorkflowNode::Call(c) => assert_eq!(c.agent, "notify_result"),
            _ => panic!("Expected Call node in always"),
        }
    }

    #[test]
    fn test_parse_minimal_workflow() {
        let input = "workflow simple { call build }";
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
        let mut names = collect_agent_names(&def.body);
        names.extend(collect_agent_names(&def.always));
        assert!(names.contains(&"plan".to_string()));
        assert!(names.contains(&"implement".to_string()));
        assert!(names.contains(&"diagnose".to_string())); // on_fail
        assert!(names.contains(&"reviewer_security".to_string()));
        assert!(names.contains(&"notify_result".to_string()));
    }

    #[test]
    fn test_count_nodes() {
        let def = parse_workflow_str(FULL_WORKFLOW, "test.wf").unwrap();
        let body_count = count_nodes(&def.body);
        // 9 top-level + 3 in while + 3 in parallel + 1 in if = 16
        assert_eq!(body_count, 16);
    }

    #[test]
    fn test_load_from_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(wf_dir.join("simple.wf"), "workflow simple { call build }").unwrap();

        let defs = load_workflow_defs(tmp.path().to_str().unwrap(), "/nonexistent").unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "simple");
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
    description = "Full development cycle — plan from ticket, implement, push PR, then review and iterate until clean"
    trigger     = "manual"
  }

  inputs {
    ticket_id required
  }

  call plan

  call implement {
    retries = 2
  }

  call push-and-pr

  call review

  while review.has_review_issues {
    max_iterations = 5
    stuck_after    = 3
    on_max_iter    = fail

    call address-reviews
    call review
  }
}
"#;
        let def = parse_workflow_str(input, "ticket-to-pr.wf").unwrap();
        assert_eq!(def.name, "ticket-to-pr");
        assert_eq!(def.trigger, WorkflowTrigger::Manual);
        assert_eq!(def.inputs.len(), 1);
        assert!(def.inputs[0].required);
        // call plan, call implement, call push-and-pr, call review, while
        assert_eq!(def.body.len(), 5);

        match &def.body[1] {
            WorkflowNode::Call(c) => {
                assert_eq!(c.agent, "implement");
                assert_eq!(c.retries, 2);
            }
            _ => panic!("Expected Call node"),
        }

        match &def.body[4] {
            WorkflowNode::While(w) => {
                assert_eq!(w.step, "review");
                assert_eq!(w.marker, "has_review_issues");
                assert_eq!(w.max_iterations, 5);
                assert_eq!(w.stuck_after, Some(3));
                assert_eq!(w.body.len(), 2);
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
            WorkflowNode::Call(c) => assert_eq!(c.agent, "analyze-lint"),
            _ => panic!("Expected Call node"),
        }
    }
}
