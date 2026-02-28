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

export interface Session {
  id: string;
  started_at: string;
  ended_at: string | null;
  notes: string | null;
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

export interface EndSessionRequest {
  notes?: string;
}

export interface SyncResult {
  synced: number;
  closed: number;
}
