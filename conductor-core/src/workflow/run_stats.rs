//! `workflow run-stats`: time, review cycles, tokens and cost for a workflow run tree.
//!
//! Usage is summed from `agent_runs`, not from the `workflow_run_steps` usage columns,
//! so runs from before those columns were filled report correctly, and retry attempts
//! whose agent run no step row links any more are still counted (they are attributed
//! by their `Workflow step: <name>` prompt, or fall into the `(unattributed)` bucket).
//!
//! Review cycles are found from the *shape* of step outputs, not from workflow or step
//! names: an aggregator output has `blocking_findings`, triage has `triaged`, verify has
//! `passed` plus `checks`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;

use crate::error::{ConductorError, Result};

const UNATTRIBUTED: &str = "(unattributed)";
const STEP_PROMPT_PREFIX: &str = "Workflow step: ";

/// Usage summed over agent runs.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cost_usd: f64,
    pub num_turns: i64,
    /// Number of agent runs summed.
    pub agent_runs: usize,
}

impl Usage {
    fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_input_tokens += o.cache_read_input_tokens;
        self.cache_creation_input_tokens += o.cache_creation_input_tokens;
        self.cost_usd += o.cost_usd;
        self.num_turns += o.num_turns;
        self.agent_runs += o.agent_runs;
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VerifyResult {
    pub passed: bool,
    pub failed_checks: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TriageCounts {
    pub address: usize,
    pub pushback: usize,
    pub clarify: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepStats {
    /// `workflow > child-workflow > step`, which keeps same-named steps in different
    /// workflows apart.
    pub path: String,
    pub step_name: String,
    pub workflow_name: String,
    /// Step rows aggregated into this entry (iterations, child runs).
    pub executions: usize,
    /// Executions plus retries.
    pub attempts: usize,
    /// Sum of each row's own start..end.
    pub wall_ms: i64,
    pub usage: Usage,
    #[serde(skip)]
    first_start: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CycleStats {
    pub index: usize,
    /// First start to last end over the cycle's steps, so parallel reviewers are not
    /// counted twice.
    pub wall_ms: i64,
    pub usage: Usage,
    pub approved: Option<bool>,
    pub blocking_by_reviewer: BTreeMap<String, usize>,
    pub blocking_total: usize,
    pub triage: TriageCounts,
    pub verify: Vec<VerifyResult>,
    /// Aggregator outputs in the cycle (more than one when a pushback re-reviews).
    pub reviews: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunStats {
    pub run_ids: Vec<String>,
    pub workflow_names: Vec<String>,
    pub status: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub wall_ms: i64,
    pub usage: Usage,
    pub agent_runs: usize,
    pub steps: Vec<StepStats>,
    pub cycles: Vec<CycleStats>,
    pub cycles_to_approval: Option<usize>,
    pub pre_cycle_verify: Vec<VerifyResult>,
}

struct RunRow {
    id: String,
    workflow_name: String,
    parent_run_id: String,
    parent_workflow_run_id: Option<String>,
    status: String,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    started_raw: String,
    ended_raw: Option<String>,
}

struct StepRow {
    run_id: String,
    step_name: String,
    child_run_id: Option<String>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    iteration: i64,
    retry_count: i64,
    position: i64,
    output: Option<Value>,
}

struct AgentRow {
    id: String,
    prompt: String,
    started_at: Option<DateTime<Utc>>,
    usage: Usage,
}

fn parse_ts(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn ms_between(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<i64> {
    Some((b? - a?).num_milliseconds().max(0))
}

fn parse_output(raw: Option<String>) -> Option<Value> {
    let v: Value = serde_json::from_str(raw?.trim()).ok()?;
    match v {
        // Some writers store the output as a JSON string holding JSON.
        Value::String(s) => serde_json::from_str(&s).ok(),
        other => Some(other),
    }
}

fn load_run(conn: &Connection, id: &str) -> Result<Option<RunRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_name, parent_run_id, parent_workflow_run_id, status, started_at, \
         ended_at FROM workflow_runs WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_run)?;
    Ok(rows.next().transpose()?)
}

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    let started_raw: String = r.get(5)?;
    let ended_raw: Option<String> = r.get(6)?;
    Ok(RunRow {
        id: r.get(0)?,
        workflow_name: r.get(1)?,
        parent_run_id: r.get(2)?,
        parent_workflow_run_id: r.get(3)?,
        status: r.get(4)?,
        started_at: parse_ts(Some(&started_raw)),
        ended_at: parse_ts(ended_raw.as_deref()),
        started_raw,
        ended_raw,
    })
}

fn load_children(conn: &Connection, parent: &str) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_name, parent_run_id, parent_workflow_run_id, status, started_at, \
         ended_at FROM workflow_runs WHERE parent_workflow_run_id = ?1 ORDER BY started_at",
    )?;
    let rows = stmt
        .query_map(params![parent], row_to_run)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn load_steps(conn: &Connection, run_id: &str) -> Result<Vec<StepRow>> {
    let mut stmt = conn.prepare(
        "SELECT step_name, child_run_id, started_at, ended_at, iteration, retry_count, position, \
         structured_output FROM workflow_run_steps WHERE workflow_run_id = ?1 \
         ORDER BY position, started_at",
    )?;
    let rows = stmt
        .query_map(params![run_id], |r| {
            let started: Option<String> = r.get(2)?;
            let ended: Option<String> = r.get(3)?;
            Ok(StepRow {
                run_id: run_id.to_string(),
                step_name: r.get(0)?,
                child_run_id: r.get(1)?,
                started_at: parse_ts(started.as_deref()),
                ended_at: parse_ts(ended.as_deref()),
                iteration: r.get(4)?,
                retry_count: r.get(5)?,
                position: r.get(6)?,
                output: parse_output(r.get(7)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

const AGENT_COLS: &str = "id, prompt, started_at, cost_usd, num_turns, input_tokens, \
     output_tokens, cache_read_input_tokens, cache_creation_input_tokens";

fn row_to_agent(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRow> {
    let started: Option<String> = r.get(2)?;
    Ok(AgentRow {
        id: r.get(0)?,
        prompt: r.get(1)?,
        started_at: parse_ts(started.as_deref()),
        usage: Usage {
            cost_usd: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            num_turns: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            input_tokens: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            output_tokens: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
            cache_read_input_tokens: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
            cache_creation_input_tokens: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
            agent_runs: 1,
        },
    })
}

fn load_agents_by_parent(conn: &Connection, parent: &str) -> Result<Vec<AgentRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {AGENT_COLS} FROM agent_runs WHERE parent_run_id = ?1 ORDER BY started_at"
    ))?;
    let rows = stmt
        .query_map(params![parent], row_to_agent)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn load_agent_by_id(conn: &Connection, id: &str) -> Result<Option<AgentRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {AGENT_COLS} FROM agent_runs WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id], row_to_agent)?;
    Ok(rows.next().transpose()?)
}

/// Everything collected for one set of root runs, before aggregation.
struct Tree {
    runs: Vec<RunRow>,
    steps: Vec<StepRow>,
    /// Attributed usage per step index; `None` index = unattributed.
    step_usage: Vec<Usage>,
    unattributed: Usage,
}

fn load_tree(conn: &Connection, root_ids: &[String]) -> Result<Tree> {
    let mut runs: Vec<RunRow> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for root in root_ids {
        let run = load_run(conn, root)?
            .ok_or_else(|| ConductorError::WorkflowRunNotFound { id: root.clone() })?;
        let mut queue = vec![run];
        while let Some(r) = queue.pop() {
            if !seen.insert(r.id.clone()) {
                continue;
            }
            let mut kids = load_children(conn, &r.id)?;
            kids.reverse();
            runs.push(r);
            queue.extend(kids);
        }
    }

    let mut steps: Vec<StepRow> = Vec::new();
    for r in &runs {
        steps.extend(load_steps(conn, &r.id)?);
    }

    // Agent runs: every run the workflow runs created (they share the run's synthetic
    // parent agent run), plus any a step links that lives elsewhere.
    let mut agents: Vec<AgentRow> = Vec::new();
    let mut agent_ids: HashSet<String> = HashSet::new();
    let mut parent_to_run: HashMap<&str, &str> = HashMap::new();
    for r in &runs {
        parent_to_run.entry(&r.parent_run_id).or_insert(&r.id);
    }
    let mut agent_run_of: HashMap<String, String> = HashMap::new();
    for (parent, run_id) in &parent_to_run {
        for a in load_agents_by_parent(conn, parent)? {
            if agent_ids.insert(a.id.clone()) {
                agent_run_of.insert(a.id.clone(), run_id.to_string());
                agents.push(a);
            }
        }
    }
    let run_ids: HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    for s in &steps {
        if let Some(c) = &s.child_run_id {
            if !agent_ids.contains(c) && !run_ids.contains(c.as_str()) {
                if let Some(a) = load_agent_by_id(conn, c)? {
                    agent_ids.insert(a.id.clone());
                    agent_run_of.insert(a.id.clone(), s.run_id.clone());
                    agents.push(a);
                }
            }
        }
    }

    // Attribute.
    let mut by_child: HashMap<&str, usize> = HashMap::new();
    for (i, s) in steps.iter().enumerate() {
        if let Some(c) = &s.child_run_id {
            by_child.insert(c, i);
        }
    }
    let mut step_usage = vec![Usage::default(); steps.len()];
    let mut unattributed = Usage::default();
    for a in &agents {
        let idx = by_child.get(a.id.as_str()).copied().or_else(|| {
            let name = a.prompt.strip_prefix(STEP_PROMPT_PREFIX)?;
            let run_id = agent_run_of.get(&a.id)?;
            let cands: Vec<usize> = steps
                .iter()
                .enumerate()
                .filter(|(_, s)| &s.run_id == run_id && s.step_name == name)
                .map(|(i, _)| i)
                .collect();
            // The attempt belongs to the row that ended first at or after it started.
            let best = cands
                .iter()
                .copied()
                .filter(|&i| match (steps[i].ended_at, a.started_at) {
                    (Some(e), Some(st)) => e >= st,
                    _ => false,
                })
                .min_by_key(|&i| steps[i].ended_at);
            best.or_else(|| cands.last().copied())
        });
        match idx {
            Some(i) => step_usage[i].add(&a.usage),
            None => unattributed.add(&a.usage),
        }
    }

    Ok(Tree {
        runs,
        steps,
        step_usage,
        unattributed,
    })
}

fn is_workflow_step(name: &str) -> bool {
    name.starts_with("workflow:")
}

/// Workflow-name chain from the root to `run_id`.
fn workflow_chain(runs: &HashMap<&str, &RunRow>, run_id: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut cur = runs.get(run_id).copied();
    let mut guard = 0;
    while let Some(r) = cur {
        chain.push(r.workflow_name.clone());
        cur = r
            .parent_workflow_run_id
            .as_deref()
            .and_then(|p| runs.get(p).copied());
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    chain.reverse();
    chain
}

fn aggregator_findings(v: &Value) -> Option<&Vec<Value>> {
    v.get("blocking_findings")?.as_array()
}

fn triage_items(v: &Value) -> Option<&Vec<Value>> {
    v.get("triaged")?.as_array()
}

fn verify_result(v: &Value) -> Option<VerifyResult> {
    let passed = v.get("passed")?.as_bool()?;
    let checks = v.get("checks")?.as_array()?;
    let failed_checks = checks
        .iter()
        .filter(|c| {
            matches!(
                c.get("status").and_then(Value::as_str),
                Some("fail") | Some("failed")
            )
        })
        .filter_map(|c| c.get("name").and_then(Value::as_str).map(String::from))
        .collect();
    Some(VerifyResult {
        passed,
        failed_checks,
    })
}

/// Collect stats for one or more root runs combined into one logical unit.
pub fn collect_run_stats(conn: &Connection, root_run_ids: &[String]) -> Result<RunStats> {
    if root_run_ids.is_empty() {
        return Err(ConductorError::InvalidInput(
            "at least one workflow run id is required".into(),
        ));
    }
    let tree = load_tree(conn, root_run_ids)?;
    let now = Utc::now();
    let run_by_id: HashMap<&str, &RunRow> = tree.runs.iter().map(|r| (r.id.as_str(), r)).collect();

    // ---- steps ----
    let mut by_path: HashMap<String, StepStats> = HashMap::new();
    for (i, s) in tree.steps.iter().enumerate() {
        if is_workflow_step(&s.step_name) {
            continue;
        }
        let chain = workflow_chain(&run_by_id, &s.run_id);
        let wf = chain.last().cloned().unwrap_or_default();
        let path = format!("{} > {}", chain.join(" > "), s.step_name);
        let e = by_path.entry(path.clone()).or_insert_with(|| StepStats {
            path,
            step_name: s.step_name.clone(),
            workflow_name: wf,
            executions: 0,
            attempts: 0,
            wall_ms: 0,
            usage: Usage::default(),
            first_start: None,
        });
        e.executions += 1;
        e.attempts += 1 + s.retry_count.max(0) as usize;
        e.wall_ms += ms_between(s.started_at, s.ended_at.or(s.started_at.map(|_| now)))
            .unwrap_or(0);
        e.usage.add(&tree.step_usage[i]);
        if let Some(st) = s.started_at {
            e.first_start = Some(e.first_start.map_or(st, |f| f.min(st)));
        }
    }
    let mut steps: Vec<StepStats> = by_path.into_values().collect();
    steps.sort_by(|a, b| {
        a.first_start
            .cmp(&b.first_start)
            .then_with(|| a.path.cmp(&b.path))
    });
    if tree.unattributed.agent_runs > 0 {
        steps.push(StepStats {
            path: UNATTRIBUTED.into(),
            step_name: UNATTRIBUTED.into(),
            workflow_name: String::new(),
            executions: 0,
            attempts: 0,
            wall_ms: 0,
            usage: tree.unattributed.clone(),
            first_start: None,
        });
    }

    // ---- cycles ----
    // Steps by run, and the child run each `workflow:` step spawned.
    let mut steps_of_run: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in tree.steps.iter().enumerate() {
        steps_of_run.entry(&s.run_id).or_default().push(i);
    }
    let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in &tree.runs {
        if let Some(p) = &r.parent_workflow_run_id {
            children_of.entry(p).or_default().push(&r.id);
        }
    }
    // (owner run, owner iteration) -> aggregator step indices
    let mut cycle_keys: Vec<(String, i64)> = Vec::new();
    let mut aggregators: HashMap<(String, i64), Vec<usize>> = HashMap::new();
    for (i, s) in tree.steps.iter().enumerate() {
        let Some(out) = &s.output else { continue };
        if aggregator_findings(out).is_none() {
            continue;
        }
        let key = match run_by_id
            .get(s.run_id.as_str())
            .and_then(|r| r.parent_workflow_run_id.as_deref())
        {
            Some(p) => {
                let iter = tree
                    .steps
                    .iter()
                    .find(|ps| ps.run_id == p && ps.child_run_id.as_deref() == Some(s.run_id.as_str()))
                    .map(|ps| ps.iteration)
                    .unwrap_or(0);
                (p.to_string(), iter)
            }
            None => (s.run_id.clone(), s.iteration),
        };
        if !aggregators.contains_key(&key) {
            cycle_keys.push(key.clone());
        }
        aggregators.entry(key).or_default().push(i);
    }

    struct Cycle {
        start: Option<DateTime<Utc>>,
        stats: CycleStats,
    }
    let mut in_cycle: HashSet<usize> = HashSet::new();
    let mut cycles: Vec<Cycle> = Vec::new();
    for key in &cycle_keys {
        // Steps: owner-run steps of that iteration plus all descendant runs' steps.
        let mut members: Vec<usize> = Vec::new();
        for &i in steps_of_run.get(key.0.as_str()).into_iter().flatten() {
            if tree.steps[i].iteration != key.1 {
                continue;
            }
            members.push(i);
            if let Some(child) = tree.steps[i].child_run_id.as_deref() {
                let mut stack = vec![child];
                let mut visited: HashSet<&str> = HashSet::new();
                while let Some(r) = stack.pop() {
                    if !visited.insert(r) || !run_by_id.contains_key(r) {
                        continue;
                    }
                    members.extend(steps_of_run.get(r).into_iter().flatten().copied());
                    stack.extend(children_of.get(r).into_iter().flatten().copied());
                }
            }
        }
        members.sort_unstable();
        members.dedup();

        let start = members.iter().filter_map(|&i| tree.steps[i].started_at).min();
        let end = members
            .iter()
            .filter_map(|&i| tree.steps[i].ended_at.or(tree.steps[i].started_at.map(|_| now)))
            .max();
        let mut usage = Usage::default();
        for &i in &members {
            usage.add(&tree.step_usage[i]);
        }

        let aggs = &aggregators[key];
        let last = aggs
            .iter()
            .copied()
            .max_by_key(|&i| (tree.steps[i].ended_at, tree.steps[i].position))
            .expect("cycle has an aggregator");
        let last_out = tree.steps[last].output.as_ref().expect("aggregator output");
        let findings = aggregator_findings(last_out).cloned().unwrap_or_default();
        let mut blocking_by_reviewer: BTreeMap<String, usize> = BTreeMap::new();
        for f in &findings {
            let who = f
                .get("reviewer")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            *blocking_by_reviewer.entry(who).or_default() += 1;
        }

        let mut triage = TriageCounts::default();
        let mut verify = Vec::new();
        for &i in &members {
            let Some(out) = &tree.steps[i].output else {
                continue;
            };
            if let Some(items) = triage_items(out) {
                for t in items {
                    match t.get("outcome").and_then(Value::as_str) {
                        Some("address") => triage.address += 1,
                        Some("pushback") => triage.pushback += 1,
                        Some("clarify") => triage.clarify += 1,
                        _ => {}
                    }
                }
            }
            if let Some(v) = verify_result(out) {
                verify.push(v);
            }
        }
        in_cycle.extend(members.iter().copied());
        cycles.push(Cycle {
            start,
            stats: CycleStats {
                index: 0,
                wall_ms: ms_between(start, end).unwrap_or(0),
                usage,
                approved: last_out.get("overall_approved").and_then(Value::as_bool),
                blocking_total: findings.len(),
                blocking_by_reviewer,
                triage,
                verify,
                reviews: aggs.len(),
            },
        });
    }
    cycles.sort_by_key(|c| c.start);
    let cycles: Vec<CycleStats> = cycles
        .into_iter()
        .enumerate()
        .map(|(n, mut c)| {
            c.stats.index = n + 1;
            c.stats
        })
        .collect();
    let cycles_to_approval = cycles
        .iter()
        .find(|c| c.approved == Some(true))
        .map(|c| c.index);

    let pre_cycle_verify: Vec<VerifyResult> = tree
        .steps
        .iter()
        .enumerate()
        .filter(|(i, _)| !in_cycle.contains(i))
        .filter_map(|(_, s)| s.output.as_ref().and_then(verify_result))
        .collect();

    // ---- totals ----
    let mut usage = Usage::default();
    for u in &tree.step_usage {
        usage.add(u);
    }
    usage.add(&tree.unattributed);

    let mut wall_ms = 0;
    let mut started: Option<DateTime<Utc>> = None;
    let mut ended: Option<DateTime<Utc>> = None;
    let mut started_raw: Option<String> = None;
    let mut ended_raw: Option<String> = None;
    let mut seen_roots = HashSet::new();
    for root in root_run_ids {
        if !seen_roots.insert(root.as_str()) {
            continue;
        }
        let Some(r) = run_by_id.get(root.as_str()) else {
            continue;
        };
        let end = r.ended_at.unwrap_or(now);
        wall_ms += ms_between(r.started_at, Some(end)).unwrap_or(0);
        if started.is_none() || r.started_at < started {
            started = r.started_at;
            started_raw = Some(r.started_raw.clone());
        }
        if ended.is_none() || r.ended_at > ended {
            ended = r.ended_at;
            ended_raw = r.ended_raw.clone();
        }
    }

    let mut workflow_names: Vec<String> = Vec::new();
    for r in &tree.runs {
        if !workflow_names.contains(&r.workflow_name) {
            workflow_names.push(r.workflow_name.clone());
        }
    }
    let status = root_run_ids
        .last()
        .and_then(|id| run_by_id.get(id.as_str()))
        .map(|r| r.status.clone())
        .unwrap_or_default();

    Ok(RunStats {
        run_ids: root_run_ids.to_vec(),
        workflow_names,
        status,
        started_at: started_raw,
        ended_at: ended_raw,
        wall_ms,
        agent_runs: usage.agent_runs,
        usage,
        steps,
        cycles,
        cycles_to_approval,
        pre_cycle_verify,
    })
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn fmt_cost(c: f64) -> String {
    format!("${c:.2}")
}

fn fmt_tokens(n: i64) -> String {
    let a = n.abs() as f64;
    if a >= 1_000_000.0 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if a >= 10_000.0 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else if a >= 1_000.0 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_blocking(c: &CycleStats) -> String {
    if c.blocking_by_reviewer.is_empty() {
        return "-".into();
    }
    c.blocking_by_reviewer
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn fmt_verify(v: &[VerifyResult]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    v.iter()
        .map(|r| {
            if r.passed {
                "✓".to_string()
            } else if r.failed_checks.is_empty() {
                "✗".to_string()
            } else {
                format!("✗ ({})", r.failed_checks.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn fmt_approved(a: Option<bool>) -> &'static str {
    match a {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    }
}

/// Render rows as a left-aligned first column and right-aligned others, with a header.
fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let cols = header.len();
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            let pad = widths[i].saturating_sub(c.chars().count());
            if i == 0 {
                let _ = write!(out, "{c}{}", " ".repeat(pad));
            } else {
                let _ = write!(out, "  {}{c}", " ".repeat(pad));
            }
        }
        out.trim_end().to_string()
    };
    let mut s = line(header.to_vec());
    s.push('\n');
    for r in rows {
        s.push_str(&line(r.iter().map(String::as_str).collect()));
        s.push('\n');
    }
    s
}

fn header_block(s: &RunStats) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Run{}: {} ({})  status: {}",
        if s.run_ids.len() > 1 { "s" } else { "" },
        s.run_ids.join(" + "),
        s.workflow_names.join(", "),
        s.status
    );
    let u = &s.usage;
    let _ = writeln!(
        out,
        "Wall: {}   Agent runs: {}   Cost: {}   Turns: {}",
        fmt_dur(s.wall_ms),
        s.agent_runs,
        fmt_cost(u.cost_usd),
        u.num_turns
    );
    let _ = writeln!(
        out,
        "Tokens: in {} / out {}   Cache: read {} / write {}",
        fmt_tokens(u.input_tokens),
        fmt_tokens(u.output_tokens),
        fmt_tokens(u.cache_read_input_tokens),
        fmt_tokens(u.cache_creation_input_tokens)
    );
    let _ = writeln!(
        out,
        "Review cycles: {}   Cycles to approval: {}",
        s.cycles.len(),
        s.cycles_to_approval
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out
}

pub fn format_run_stats(s: &RunStats) -> String {
    let mut out = header_block(s);
    if !s.steps.is_empty() {
        out.push_str("\nSteps\n");
        let rows: Vec<Vec<String>> = s
            .steps
            .iter()
            .map(|st| {
                vec![
                    st.path.clone(),
                    format!("{}x{}", st.executions, st.attempts),
                    fmt_dur(st.wall_ms),
                    fmt_cost(st.usage.cost_usd),
                    fmt_tokens(st.usage.output_tokens),
                    format!(
                        "{}/{}",
                        fmt_tokens(st.usage.cache_read_input_tokens),
                        fmt_tokens(st.usage.cache_creation_input_tokens)
                    ),
                    st.usage.num_turns.to_string(),
                ]
            })
            .collect();
        out.push_str(&table(
            &["step", "runs", "wall", "cost", "out", "cache r/w", "turns"],
            &rows,
        ));
    }
    if !s.cycles.is_empty() {
        out.push_str("\nReview cycles\n");
        let rows: Vec<Vec<String>> = s
            .cycles
            .iter()
            .map(|c| {
                vec![
                    c.index.to_string(),
                    fmt_dur(c.wall_ms),
                    fmt_cost(c.usage.cost_usd),
                    fmt_approved(c.approved).into(),
                    format!("{} ({})", c.blocking_total, fmt_blocking(c)),
                    format!("{}/{}/{}", c.triage.address, c.triage.pushback, c.triage.clarify),
                    fmt_verify(&c.verify),
                ]
            })
            .collect();
        out.push_str(&table(
            &[
                "cycle",
                "wall",
                "cost",
                "approved",
                "blocking",
                "triage A/P/C",
                "verify",
            ],
            &rows,
        ));
    }
    if !s.pre_cycle_verify.is_empty() {
        let _ = writeln!(
            out,
            "\nVerify outside review cycles: {}",
            fmt_verify(&s.pre_cycle_verify)
        );
    }
    out
}

fn delta_i(a: i64, b: i64) -> String {
    format!("{:+}", b - a)
}

pub fn format_run_stats_compare(a: &RunStats, b: &RunStats) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "A: {} ({})", a.run_ids.join(" + "), a.workflow_names.join(", "));
    let _ = writeln!(out, "B: {} ({})", b.run_ids.join(" + "), b.workflow_names.join(", "));
    out.push_str("\nTotals\n");
    let (ua, ub) = (&a.usage, &b.usage);
    let blocking = |s: &RunStats| s.cycles.iter().map(|c| c.blocking_total as i64).sum::<i64>();
    let rows = vec![
        vec![
            "wall".to_string(),
            fmt_dur(a.wall_ms),
            fmt_dur(b.wall_ms),
            format!("{}{}", if b.wall_ms >= a.wall_ms { "+" } else { "-" }, fmt_dur((b.wall_ms - a.wall_ms).abs())),
        ],
        vec![
            "cost".into(),
            fmt_cost(ua.cost_usd),
            fmt_cost(ub.cost_usd),
            format!("{:+.2}", ub.cost_usd - ua.cost_usd),
        ],
        vec![
            "agent runs".into(),
            a.agent_runs.to_string(),
            b.agent_runs.to_string(),
            delta_i(a.agent_runs as i64, b.agent_runs as i64),
        ],
        vec![
            "turns".into(),
            ua.num_turns.to_string(),
            ub.num_turns.to_string(),
            delta_i(ua.num_turns, ub.num_turns),
        ],
        vec![
            "tokens in".into(),
            fmt_tokens(ua.input_tokens),
            fmt_tokens(ub.input_tokens),
            delta_i(ua.input_tokens, ub.input_tokens),
        ],
        vec![
            "tokens out".into(),
            fmt_tokens(ua.output_tokens),
            fmt_tokens(ub.output_tokens),
            delta_i(ua.output_tokens, ub.output_tokens),
        ],
        vec![
            "cache read".into(),
            fmt_tokens(ua.cache_read_input_tokens),
            fmt_tokens(ub.cache_read_input_tokens),
            delta_i(ua.cache_read_input_tokens, ub.cache_read_input_tokens),
        ],
        vec![
            "cache write".into(),
            fmt_tokens(ua.cache_creation_input_tokens),
            fmt_tokens(ub.cache_creation_input_tokens),
            delta_i(ua.cache_creation_input_tokens, ub.cache_creation_input_tokens),
        ],
        vec![
            "review cycles".into(),
            a.cycles.len().to_string(),
            b.cycles.len().to_string(),
            delta_i(a.cycles.len() as i64, b.cycles.len() as i64),
        ],
        vec![
            "cycles to approval".into(),
            a.cycles_to_approval.map_or("-".into(), |n| n.to_string()),
            b.cycles_to_approval.map_or("-".into(), |n| n.to_string()),
            String::new(),
        ],
        vec![
            "blocking findings".into(),
            blocking(a).to_string(),
            blocking(b).to_string(),
            delta_i(blocking(a), blocking(b)),
        ],
    ];
    out.push_str(&table(&["", "A", "B", "delta"], &rows));

    // Per-step, joined on path.
    let mut paths: Vec<&str> = a.steps.iter().map(|s| s.path.as_str()).collect();
    for s in &b.steps {
        if !paths.contains(&s.path.as_str()) {
            paths.push(&s.path);
        }
    }
    if !paths.is_empty() {
        out.push_str("\nSteps\n");
        let find = |s: &RunStats, p: &str| s.steps.iter().find(|x| x.path == p).cloned();
        let rows: Vec<Vec<String>> = paths
            .iter()
            .map(|p| {
                let (sa, sb) = (find(a, p), find(b, p));
                let cell = |s: &Option<StepStats>, f: &dyn Fn(&StepStats) -> String| {
                    s.as_ref().map_or("—".to_string(), f)
                };
                vec![
                    p.to_string(),
                    cell(&sa, &|s| fmt_dur(s.wall_ms)),
                    cell(&sb, &|s| fmt_dur(s.wall_ms)),
                    cell(&sa, &|s| fmt_cost(s.usage.cost_usd)),
                    cell(&sb, &|s| fmt_cost(s.usage.cost_usd)),
                    cell(&sa, &|s| fmt_tokens(s.usage.output_tokens)),
                    cell(&sb, &|s| fmt_tokens(s.usage.output_tokens)),
                ]
            })
            .collect();
        out.push_str(&table(
            &["step", "wall A", "wall B", "cost A", "cost B", "out A", "out B"],
            &rows,
        ));
    }

    out.push_str("\nReview cycles\n");
    for (label, s) in [("A", a), ("B", b)] {
        let _ = writeln!(
            out,
            "{label}: {} cycle(s), approved in {}, {} blocking finding(s)",
            s.cycles.len(),
            s.cycles_to_approval.map_or("-".into(), |n| n.to_string()),
            blocking(s)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;

    fn t(min: i64) -> String {
        format!("2026-01-01T00:{:02}:00Z", min)
    }

    fn wf_run(conn: &Connection, id: &str, name: &str, parent_agent: &str, parent_wf: Option<&str>) {
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, parent_run_id, status, dry_run, \
             trigger, started_at, ended_at, iteration, parent_workflow_run_id) \
             VALUES (?1, ?2, ?3, 'completed', 0, 'manual', ?4, ?5, 0, ?6)",
            params![id, name, parent_agent, t(0), t(59), parent_wf],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        conn: &Connection,
        id: &str,
        run: &str,
        name: &str,
        iter: i64,
        start: i64,
        end: i64,
        child: Option<&str>,
        out: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, status, \
             position, started_at, ended_at, iteration, child_run_id, structured_output) \
             VALUES (?1, ?2, ?3, 'actor', 'completed', ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, run, name, start, t(start), t(end), iter, child, out],
        )
        .unwrap();
    }

    fn agent(conn: &Connection, id: &str, parent: &str, step: &str, start: i64, cost: f64, out: i64) {
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, status, started_at, ended_at, parent_run_id, \
             cost_usd, num_turns, input_tokens, output_tokens, cache_read_input_tokens, \
             cache_creation_input_tokens) \
             VALUES (?1, ?2, 'completed', ?3, ?3, ?4, ?5, 2, 10, ?6, 100, 50)",
            params![id, format!("Workflow step: {step}"), t(start), parent, cost, out],
        )
        .unwrap();
    }

    fn db() -> Connection {
        let conn = test_helpers::setup_db();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn
    }

    const AGG_BLOCK: &str = r#"{"overall_approved":false,"blocking_findings":[{"reviewer":"security"},{"reviewer":"security"},{"reviewer":"arch"}]}"#;
    const AGG_OK: &str = r#"{"overall_approved":true,"blocking_findings":[]}"#;
    const TRIAGE: &str = r#"{"triaged":[{"reviewer":"security","outcome":"address"},{"reviewer":"arch","outcome":"pushback"}]}"#;
    const VERIFY_FAIL: &str = r#"{"passed":false,"checks":[{"name":"clippy","status":"fail"},{"name":"test","status":"pass"}]}"#;
    const VERIFY_OK: &str = r#"{"passed":true,"checks":[{"name":"clippy","status":"pass"}]}"#;

    /// iterate-pr with two cycles; each cycle calls a review-pr child run.
    fn two_cycle_fixture(conn: &Connection) {
        wf_run(conn, "R", "iterate-pr", "PA", None);
        wf_run(conn, "C1", "review-pr", "PC1", Some("R"));
        wf_run(conn, "C2", "review-pr", "PC2", Some("R"));
        // Iteration -1: outside the review loop.
        step(conn, "s0", "R", "implement", -1, 1, 2, Some("a0"), None);
        agent(conn, "a0", "PA", "implement", 1, 1.0, 100);
        // Cycle 1 (iteration 0)
        step(conn, "w1", "R", "workflow:review-pr", 0, 3, 10, Some("C1"), None);
        step(conn, "r1a", "C1", "review-security", 0, 3, 8, Some("ra1"), None);
        step(conn, "r1b", "C1", "review-arch", 0, 4, 9, Some("rb1"), None);
        step(conn, "g1", "C1", "aggregate", 0, 9, 10, None, Some(AGG_BLOCK));
        agent(conn, "ra1", "PC1", "review-security", 3, 0.5, 10);
        agent(conn, "rb1", "PC1", "review-arch", 4, 0.5, 10);
        step(conn, "t1", "R", "triage", 0, 11, 12, None, Some(TRIAGE));
        step(conn, "v1", "R", "verify", 0, 13, 14, None, Some(VERIFY_FAIL));
        // Cycle 2 (iteration 1)
        step(conn, "w2", "R", "workflow:review-pr", 1, 20, 25, Some("C2"), None);
        step(conn, "r2a", "C2", "review-security", 0, 20, 24, Some("ra2"), None);
        step(conn, "g2", "C2", "aggregate", 0, 24, 25, None, Some(AGG_OK));
        agent(conn, "ra2", "PC2", "review-security", 20, 0.25, 10);
        step(conn, "v2", "R", "verify", 1, 26, 27, None, Some(VERIFY_OK));
    }

    #[test]
    fn missing_root_is_an_error() {
        let conn = db();
        let err = collect_run_stats(&conn, &["nope".to_string()]).unwrap_err();
        assert!(matches!(err, ConductorError::WorkflowRunNotFound { .. }));
    }

    #[test]
    fn cycles_findings_triage_and_verify_land_in_the_right_cycle() {
        let conn = db();
        two_cycle_fixture(&conn);
        let s = collect_run_stats(&conn, &["R".to_string()]).unwrap();
        assert_eq!(s.cycles.len(), 2);
        assert_eq!(s.cycles_to_approval, Some(2));

        let c1 = &s.cycles[0];
        assert_eq!(c1.approved, Some(false));
        assert_eq!(c1.blocking_total, 3);
        assert_eq!(c1.blocking_by_reviewer["security"], 2);
        assert_eq!(c1.blocking_by_reviewer["arch"], 1);
        assert_eq!((c1.triage.address, c1.triage.pushback), (1, 1));
        assert_eq!(c1.verify.len(), 1);
        assert_eq!(c1.verify[0].failed_checks, vec!["clippy".to_string()]);

        let c2 = &s.cycles[1];
        assert_eq!(c2.approved, Some(true));
        assert_eq!(c2.blocking_total, 0);
        assert_eq!(c2.triage, TriageCounts::default());
        assert!(c2.verify[0].passed);
        assert!(s.pre_cycle_verify.is_empty());
    }

    #[test]
    fn cycle_wall_is_the_span_not_the_sum() {
        let conn = db();
        two_cycle_fixture(&conn);
        let s = collect_run_stats(&conn, &["R".to_string()]).unwrap();
        // Cycle 1: first start minute 3 .. last end minute 14.
        assert_eq!(s.cycles[0].wall_ms, 11 * 60_000);
        // Cycle 2: minute 20 .. 27.
        assert_eq!(s.cycles[1].wall_ms, 7 * 60_000);
        // Cycle usage covers only that cycle's agent runs.
        assert!((s.cycles[0].usage.cost_usd - 1.0).abs() < 1e-9);
        assert!((s.cycles[1].usage.cost_usd - 0.25).abs() < 1e-9);
    }

    #[test]
    fn totals_sum_agent_runs() {
        let conn = db();
        two_cycle_fixture(&conn);
        let s = collect_run_stats(&conn, &["R".to_string()]).unwrap();
        assert_eq!(s.agent_runs, 4);
        assert!((s.usage.cost_usd - 2.25).abs() < 1e-9);
        assert_eq!(s.usage.output_tokens, 130);
        let sec = s
            .steps
            .iter()
            .find(|x| x.path == "iterate-pr > review-pr > review-security")
            .unwrap();
        assert_eq!(sec.executions, 2);
        assert!((sec.usage.cost_usd - 0.75).abs() < 1e-9);
        assert!(s.steps.iter().all(|x| !x.step_name.starts_with("workflow:")));
    }

    #[test]
    fn orphaned_retry_attempt_is_attributed_by_prompt() {
        let conn = db();
        wf_run(&conn, "R", "wf", "PA", None);
        // The step row points at the second attempt; the first is orphaned.
        step(&conn, "s1", "R", "implement", 0, 1, 10, Some("a2"), None);
        agent(&conn, "a1", "PA", "implement", 2, 1.0, 5);
        agent(&conn, "a2", "PA", "implement", 6, 2.0, 5);
        agent(&conn, "x", "PA", "ghost", 3, 4.0, 5);
        let s = collect_run_stats(&conn, &["R".to_string()]).unwrap();
        let st = s.steps.iter().find(|x| x.step_name == "implement").unwrap();
        assert_eq!(st.usage.agent_runs, 2);
        assert!((st.usage.cost_usd - 3.0).abs() < 1e-9);
        let un = s.steps.iter().find(|x| x.path == UNATTRIBUTED).unwrap();
        assert!((un.usage.cost_usd - 4.0).abs() < 1e-9);
        assert!((s.usage.cost_usd - 7.0).abs() < 1e-9);
    }

    #[test]
    fn multiple_roots_are_combined() {
        let conn = db();
        wf_run(&conn, "R1", "ticket-to-pr", "P1", None);
        wf_run(&conn, "R2", "iterate-pr", "P2", None);
        step(&conn, "s1", "R1", "implement", 0, 1, 2, Some("a1"), None);
        step(&conn, "s2", "R2", "address", 0, 1, 2, Some("a2"), None);
        agent(&conn, "a1", "P1", "implement", 1, 1.0, 1);
        agent(&conn, "a2", "P2", "address", 1, 2.0, 1);
        let s = collect_run_stats(&conn, &["R1".to_string(), "R2".to_string()]).unwrap();
        assert_eq!(s.agent_runs, 2);
        assert!((s.usage.cost_usd - 3.0).abs() < 1e-9);
        assert_eq!(s.workflow_names, vec!["ticket-to-pr", "iterate-pr"]);
        assert_eq!(s.wall_ms, 2 * 59 * 60_000);
    }

    #[test]
    fn verify_outside_a_cycle_is_reported_separately() {
        let conn = db();
        wf_run(&conn, "R", "wf", "PA", None);
        step(&conn, "v", "R", "verify", 0, 1, 2, None, Some(VERIFY_OK));
        let s = collect_run_stats(&conn, &["R".to_string()]).unwrap();
        assert!(s.cycles.is_empty());
        assert_eq!(s.pre_cycle_verify.len(), 1);
    }

    #[test]
    fn renderers_contain_key_lines() {
        let conn = db();
        two_cycle_fixture(&conn);
        let s = collect_run_stats(&conn, &["R".to_string()]).unwrap();
        let text = format_run_stats(&s);
        assert!(text.contains("Cycles to approval: 2"), "{text}");
        assert!(text.contains("Cost: $2.25"), "{text}");
        assert!(text.contains("security 2, arch 1") || text.contains("arch 1, security 2"));
        assert!(text.contains("iterate-pr > review-pr > review-security"));
        assert!(text.contains("✗ (clippy)"));
        let cmp = format_run_stats_compare(&s, &s);
        assert!(cmp.contains("Totals"), "{cmp}");
        assert!(cmp.contains("cycles to approval"));
        assert!(cmp.contains("+0.00"));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"cycles_to_approval\":2"));
    }

    #[test]
    fn formatters() {
        assert_eq!(fmt_dur(750_000), "12m 30s");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
        assert_eq!(fmt_cost(4.756), "$4.76");
    }
}
