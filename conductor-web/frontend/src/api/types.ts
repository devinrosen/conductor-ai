export interface Repo {
  id: string;
  slug: string;
  local_path: string;
  remote_url: string;
  default_branch: string;
  workspace_dir: string;
  created_at: string;
  model: string | null;
  allow_agent_issue_creation: boolean;
}

export interface Worktree {
  id: string;
  repo_id: string;
  slug: string;
  branch: string;
  path: string;
  ticket_id: string | null;
  status: string;
  created_at: string;
  completed_at: string | null;
  model: string | null;
}

export interface WorktreeWithStatus extends Worktree {
  agent_status: AgentRun["status"] | null;
  ticket_title: string | null;
  ticket_number: string | null;
  ticket_url: string | null;
}

export interface Ticket {
  id: string;
  repo_id: string;
  source_type: string;
  source_id: string;
  title: string;
  body: string;
  state: string;
  labels: string;
  assignee: string | null;
  priority: string | null;
  url: string;
  synced_at: string;
  raw_json: string;
}

export interface TicketLabel {
  ticket_id: string;
  label: string;
  color: string | null;
}

export interface TicketAgentTotals {
  ticket_id: string;
  total_runs: number;
  total_cost: number;
  total_turns: number;
  total_duration_ms: number;
  total_input_tokens: number;
  total_output_tokens: number;
}

export interface CreateRepoRequest {
  remote_url: string;
  slug?: string;
  local_path?: string;
  workspace_dir?: string;
}

export interface CreateWorktreeRequest {
  name: string;
  from_branch?: string;
  ticket_id?: string;
}

export interface SyncResult {
  synced: number;
  closed: number;
}

export interface PlanStep {
  id?: string;
  description: string;
  done: boolean;
  status: "pending" | "in_progress" | "completed" | "failed";
  position?: number;
  started_at?: string | null;
  completed_at?: string | null;
}

export interface AgentRun {
  id: string;
  worktree_id: string | null;
  repo_id?: string | null;
  claude_session_id: string | null;
  prompt: string;
  status: "running" | "completed" | "failed" | "cancelled" | "waiting_for_feedback";
  result_text: string | null;
  cost_usd: number | null;
  num_turns: number | null;
  duration_ms: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_input_tokens: number | null;
  cache_creation_input_tokens: number | null;
  started_at: string;
  ended_at: string | null;
  tmux_window: string | null;
  log_file: string | null;
  model: string | null;
  plan: PlanStep[] | null;
  parent_run_id: string | null;
}

export interface RunTreeTotals {
  total_runs: number;
  total_cost: number;
  total_turns: number;
  total_duration_ms: number;
  total_input_tokens: number;
  total_output_tokens: number;
}

export interface AgentEvent {
  id: string;
  run_id: string;
  kind: "text" | "tool" | "result" | "system" | "error" | "tool_error" | "prompt";
  summary: string;
  started_at: string;
  ended_at: string | null;
  duration_ms: number | null;
  metadata: string | null;
}

export interface AgentPromptInfo {
  prompt: string;
  resume_session_id: string | null;
}

export interface AgentCreatedIssue {
  id: string;
  agent_run_id: string;
  repo_id: string;
  source_type: string;
  source_id: string;
  title: string;
  url: string;
  created_at: string;
}

export interface TicketDependencies {
  blocked_by: Ticket[];
  blocks: Ticket[];
  parent: Ticket | null;
  children: Ticket[];
}

export interface TicketDetail {
  agent_totals: TicketAgentTotals | null;
  worktrees: Worktree[];
  dependencies: TicketDependencies;
}

export interface IssueSource {
  id: string;
  repo_id: string;
  source_type: string;
  config_json: string;
}

export interface CreateIssueSourceRequest {
  source_type: string;
  config_json?: string;
}

export interface GlobalConfig {
  model: string | null;
}

export interface KnownModel {
  id: string;
  alias: string;
  tier: number;
  tier_label: string;
  description: string;
}

export interface DiscoverableRepo {
  name: string;
  /** "owner/repo" format */
  full_name: string;
  description: string;
  clone_url: string;
  ssh_url: string;
  default_branch: string;
  private: boolean;
  already_registered: boolean;
  registered_id: string | null;
}

export interface WorkflowDefSummary {
  name: string;
  description: string;
  trigger: string;
  inputs: { name: string; required: boolean; type: string; defaultValue: string | null; description: string | null }[];
  node_count: number;
  group: string | null;
  targets: string[];
}

export interface WorkflowRun {
  id: string;
  workflow_name: string;
  worktree_id: string | null;
  parent_run_id: string;
  status: "pending" | "running" | "completed" | "failed" | "cancelled" | "waiting";
  dry_run: boolean;
  trigger: string;
  started_at: string;
  ended_at: string | null;
  result_summary: string | null;
  repo_id: string | null;
  parent_workflow_run_id: string | null;
  target_label: string | null;
  active_steps?: WorkflowRunStep[];
  repo_slug: string | null;
  worktree_slug: string | null;
}

export interface WorkflowRunStep {
  id: string;
  workflow_run_id: string;
  step_name: string;
  role: string;
  can_commit: boolean;
  status: "pending" | "running" | "completed" | "failed" | "skipped" | "waiting";
  child_run_id: string | null;
  position: number;
  iteration: number;
  started_at: string | null;
  ended_at: string | null;
  result_text: string | null;
  markers_out: string | null;
  retry_count: number;
  gate_type: string | null;
  gate_prompt: string | null;
  gate_approved_by: string | null;
  gate_feedback: string | null;
  context_out: string | null;
  gate_options: string | null;
  gate_selections: string | null;
  input_tokens?: number | null;
  output_tokens?: number | null;
  cache_read_input_tokens?: number | null;
  cache_creation_input_tokens?: number | null;
}

export interface WorkflowTokenAggregate {
  workflow_name: string;
  avg_input: number;
  avg_output: number;
  avg_cache_read: number;
  avg_cache_creation: number;
  run_count: number;
}

export interface WorkflowTokenTrendRow {
  period: string;
  total_input: number;
  total_output: number;
  total_cache_read: number;
  total_cache_creation: number;
}

export interface StepTokenHeatmapRow {
  step_name: string;
  avg_input: number;
  avg_output: number;
  avg_cache_read: number;
  run_count: number;
}

// Workflow Definition AST types (matches Rust WorkflowDef serialization)

export interface WorkflowInputDecl {
  name: string;
  required: boolean;
  default: string | null;
  description: string | null;
  input_type: "string" | "boolean";
}

export interface WorkflowDef {
  name: string;
  description: string;
  trigger: "manual" | "pr" | "scheduled";
  targets: string[];
  inputs: WorkflowInputDecl[];
  body: WorkflowNode[];
  always: WorkflowNode[];
  source_path: string;
}

export interface AgentRef {
  kind: "name" | "path";
  value: string;
}

export interface Condition {
  kind: "step_marker" | "bool_input";
  step?: string;
  marker?: string;
  input?: string;
}

export type WorkflowNode =
  | { type: "call"; agent: AgentRef; retries: number; on_fail: AgentRef | null; output: string | null; with: string[]; bot_name: string | null }
  | { type: "call_workflow"; workflow: string; inputs: Record<string, string>; retries: number; on_fail: AgentRef | null; bot_name: string | null }
  | { type: "if"; condition: Condition; body: WorkflowNode[] }
  | { type: "unless"; condition: Condition; body: WorkflowNode[] }
  | { type: "while"; step: string; marker: string; max_iterations: number; stuck_after: number | null; on_max_iter: "fail" | "continue"; body: WorkflowNode[] }
  | { type: "do_while"; step: string; marker: string; max_iterations: number; stuck_after: number | null; on_max_iter: "fail" | "continue"; body: WorkflowNode[] }
  | { type: "do"; output: string | null; with: string[]; body: WorkflowNode[] }
  | { type: "parallel"; fail_fast: boolean; min_success: number | null; calls: AgentRef[]; output: string | null }
  | { type: "gate"; name: string; gate_type: string; prompt: string | null; min_approvals: number; timeout_secs: number; on_timeout: "fail" | "continue" }
  | { type: "always"; body: WorkflowNode[] }
  | { type: "script"; name: string; run: string; timeout: number | null; retries: number };

export interface RunWorkflowRequest {
  name: string;
  model?: string;
  dry_run?: boolean;
  inputs?: Record<string, string>;
}

export type FeedbackType = "text" | "confirm" | "single_select" | "multi_select";

export interface FeedbackOption {
  value: string;
  label: string;
}

export interface FeedbackRequest {
  id: string;
  run_id: string;
  prompt: string;
  response: string | null;
  status: "pending" | "responded" | "dismissed";
  created_at: string;
  feedback_type: FeedbackType;
  options?: FeedbackOption[];
  timeout_secs?: number;
}

export interface Notification {
  id: string;
  kind: string;
  title: string;
  body: string;
  severity: "info" | "warning" | "action_required";
  entity_id: string | null;
  entity_type: string | null;
  read: boolean;
  created_at: string;
  read_at: string | null;
}

export interface ThemeUnlockStats {
  repos_registered: number;
  prs_merged: number;
  workflow_streak: number;
  max_workflow_steps: number;
  max_parallel_agents: number;
  usage_days: number;
}

// Push Notifications
export interface PushSubscriptionKeys {
  p256dh: string;
  auth: string;
}

export interface PushSubscribeRequest {
  endpoint: string;
  keys: PushSubscriptionKeys;
}

export interface VapidPublicKeyResponse {
  public_key: string;
}

export interface PushSubscribeResponse {
  success: boolean;
  message: string;
}
