export interface Repo {
  id: string;
  slug: string;
  local_path: string;
  remote_url: string;
  default_branch: string;
  workspace_dir: string;
  created_at: string;
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

export interface TicketAgentTotals {
  ticket_id: string;
  total_runs: number;
  total_cost: number;
  total_turns: number;
  total_duration_ms: number;
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

export interface AgentRun {
  id: string;
  worktree_id: string;
  claude_session_id: string | null;
  prompt: string;
  status: "running" | "completed" | "failed" | "cancelled";
  result_text: string | null;
  cost_usd: number | null;
  num_turns: number | null;
  duration_ms: number | null;
  started_at: string;
  ended_at: string | null;
  tmux_window: string | null;
  log_file: string | null;
}

export interface AgentEvent {
  kind: "text" | "tool" | "result" | "system" | "error";
  summary: string;
}

export interface AgentPromptInfo {
  prompt: string;
  resume_session_id: string | null;
}

export interface WorkTarget {
  name: string;
  command: string;
  type: string;
}

export interface CreateWorkTargetRequest {
  name: string;
  command: string;
  type: string;
}
