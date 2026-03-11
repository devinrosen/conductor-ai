import type {
  Repo,
  Worktree,
  Ticket,
  TicketLabel,
  TicketAgentTotals,
  TicketDetail,
  CreateRepoRequest,
  CreateWorktreeRequest,
  SyncResult,
  AgentRun,
  AgentEvent,
  AgentPromptInfo,
  RunTreeTotals,
  AgentCreatedIssue,
  WorkTarget,
  CreateWorkTargetRequest,
  PushResult,
  CreatePrResult,
  IssueSource,
  CreateIssueSourceRequest,
  DiscoverableRepo,
  GlobalConfig,
  KnownModel,
  WorkflowDefSummary,
  WorkflowRun,
  WorkflowRunStep,
  RunWorkflowRequest,
} from "./types";

const BASE = "/api";

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(body.error || `Request failed: ${res.status}`);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export const api = {
  // Repos
  listRepos: () => request<Repo[]>("/repos"),
  createRepo: (data: CreateRepoRequest) =>
    request<Repo>("/repos", { method: "POST", body: JSON.stringify(data) }),
  deleteRepo: (id: string) =>
    request<void>(`/repos/${id}`, { method: "DELETE" }),
  setRepoModel: (id: string, model: string | null) =>
    request<Repo>(`/repos/${id}/model`, {
      method: "PATCH",
      body: JSON.stringify({ model }),
    }),

  // Worktrees
  listWorktrees: (repoId: string, showCompleted = false) =>
    request<Worktree[]>(
      showCompleted
        ? `/repos/${repoId}/worktrees?show_completed=true`
        : `/repos/${repoId}/worktrees`,
    ),
  createWorktree: (repoId: string, data: CreateWorktreeRequest) =>
    request<Worktree>(`/repos/${repoId}/worktrees`, {
      method: "POST",
      body: JSON.stringify(data),
    }),
  deleteWorktree: (id: string) =>
    request<Worktree>(`/worktrees/${id}`, { method: "DELETE" }),
  pushWorktree: (id: string) =>
    request<PushResult>(`/worktrees/${id}/push`, { method: "POST" }),
  createPr: (id: string, draft = false) =>
    request<CreatePrResult>(`/worktrees/${id}/pr`, {
      method: "POST",
      body: JSON.stringify({ draft }),
    }),
  linkTicket: (id: string, ticketId: string) =>
    request<Worktree>(`/worktrees/${id}/link-ticket`, {
      method: "POST",
      body: JSON.stringify({ ticket_id: ticketId }),
    }),
  setWorktreeModel: (id: string, model: string | null) =>
    request<Worktree>(`/worktrees/${id}/model`, {
      method: "PATCH",
      body: JSON.stringify({ model }),
    }),

  // Tickets
  ticketLabels: () => request<TicketLabel[]>("/ticket-labels"),
  listAllTickets: (showClosed = false) =>
    request<Ticket[]>(showClosed ? "/tickets?show_closed=true" : "/tickets"),
  listTickets: (repoId: string, showClosed = false) =>
    request<Ticket[]>(
      showClosed
        ? `/repos/${repoId}/tickets?show_closed=true`
        : `/repos/${repoId}/tickets`,
    ),
  syncTickets: (repoId: string) =>
    request<SyncResult>(`/repos/${repoId}/tickets/sync`, { method: "POST" }),
  getTicketDetail: (ticketId: string) =>
    request<TicketDetail>(`/tickets/${ticketId}/detail`),

  // Agent stats (aggregates)
  latestRunsByWorktree: () =>
    request<Record<string, AgentRun>>("/agent/latest-runs"),
  ticketAgentTotals: () =>
    request<Record<string, TicketAgentTotals>>("/agent/ticket-totals"),

  // Agent orchestration
  listAgentRuns: (worktreeId: string) =>
    request<AgentRun[]>(`/worktrees/${worktreeId}/agent/runs`),
  latestAgentRun: (worktreeId: string) =>
    request<AgentRun | null>(`/worktrees/${worktreeId}/agent/latest`),
  startAgent: (
    worktreeId: string,
    prompt: string,
    resumeSessionId?: string,
    parentRunId?: string,
  ) =>
    request<AgentRun>(`/worktrees/${worktreeId}/agent/start`, {
      method: "POST",
      body: JSON.stringify({
        prompt,
        resume_session_id: resumeSessionId ?? null,
        parent_run_id: parentRunId ?? null,
      }),
    }),
  stopAgent: (worktreeId: string) =>
    request<AgentRun>(`/worktrees/${worktreeId}/agent/stop`, {
      method: "POST",
    }),
  getAgentEvents: (worktreeId: string) =>
    request<AgentEvent[]>(`/worktrees/${worktreeId}/agent/events`),
  getAgentPrompt: (worktreeId: string) =>
    request<AgentPromptInfo>(`/worktrees/${worktreeId}/agent/prompt`),
  listChildRuns: (worktreeId: string, runId: string) =>
    request<AgentRun[]>(
      `/worktrees/${worktreeId}/agent/runs/${runId}/children`,
    ),
  getRunTree: (worktreeId: string, runId: string) =>
    request<AgentRun[]>(`/worktrees/${worktreeId}/agent/runs/${runId}/tree`),
  getRunTreeTotals: (worktreeId: string, runId: string) =>
    request<RunTreeTotals>(
      `/worktrees/${worktreeId}/agent/runs/${runId}/tree-totals`,
    ),
  orchestrateAgent: (
    worktreeId: string,
    prompt: string,
    failFast?: boolean,
    childTimeoutSecs?: number,
  ) =>
    request<AgentRun>(`/worktrees/${worktreeId}/agent/orchestrate`, {
      method: "POST",
      body: JSON.stringify({
        prompt,
        fail_fast: failFast ?? false,
        child_timeout_secs: childTimeoutSecs ?? 1800,
      }),
    }),
  getCreatedIssues: (worktreeId: string) =>
    request<AgentCreatedIssue[]>(`/worktrees/${worktreeId}/agent/created-issues`),
  updateRepoSettings: (repoId: string, settings: { allow_agent_issue_creation?: boolean }) =>
    request<Repo>(`/repos/${repoId}/settings`, {
      method: "PATCH",
      body: JSON.stringify(settings),
    }),

  // Global config
  getGlobalModel: () => request<GlobalConfig>("/config/model"),
  setGlobalModel: (model: string | null) =>
    request<GlobalConfig>("/config/model", {
      method: "PATCH",
      body: JSON.stringify({ model }),
    }),
  listKnownModels: () => request<KnownModel[]>("/config/known-models"),
  suggestModel: (prompt: string) =>
    request<{ suggested: string }>("/config/suggest-model", {
      method: "POST",
      body: JSON.stringify({ prompt }),
    }),

  // Work Targets
  listWorkTargets: () => request<WorkTarget[]>("/config/work-targets"),
  createWorkTarget: (data: CreateWorkTargetRequest) =>
    request<WorkTarget[]>("/config/work-targets", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  deleteWorkTarget: (index: number) =>
    request<WorkTarget[]>(`/config/work-targets/${index}`, {
      method: "DELETE",
    }),
  replaceWorkTargets: (targets: CreateWorkTargetRequest[]) =>
    request<WorkTarget[]>("/config/work-targets", {
      method: "PUT",
      body: JSON.stringify(targets),
    }),

  // Issue Sources
  listIssueSources: (repoId: string) =>
    request<IssueSource[]>(`/repos/${repoId}/sources`),
  createIssueSource: (repoId: string, data: CreateIssueSourceRequest) =>
    request<IssueSource>(`/repos/${repoId}/sources`, {
      method: "POST",
      body: JSON.stringify(data),
    }),
  deleteIssueSource: (repoId: string, sourceId: string) =>
    request<void>(`/repos/${repoId}/sources/${sourceId}`, {
      method: "DELETE",
    }),

  // GitHub repo discovery
  listGithubOrgs: () => request<string[]>("/github/orgs"),
  discoverGithubRepos: (owner?: string) =>
    request<DiscoverableRepo[]>(
      owner ? `/github/repos?owner=${encodeURIComponent(owner)}` : "/github/repos",
    ),

  // Workflows
  listWorkflowDefs: (worktreeId: string) =>
    request<WorkflowDefSummary[]>(`/worktrees/${worktreeId}/workflows/defs`),
  runWorkflow: (worktreeId: string, data: RunWorkflowRequest) =>
    request<{ status: string; worktree_id: string }>(`/worktrees/${worktreeId}/workflows/run`, {
      method: "POST",
      body: JSON.stringify(data),
    }),
  listWorkflowRuns: (worktreeId: string) =>
    request<WorkflowRun[]>(`/worktrees/${worktreeId}/workflows/runs`),
  getWorkflowRun: (runId: string) =>
    request<WorkflowRun | null>(`/workflows/runs/${runId}`),
  getWorkflowSteps: (runId: string) =>
    request<WorkflowRunStep[]>(`/workflows/runs/${runId}/steps`),
  cancelWorkflow: (runId: string) =>
    request<void>(`/workflows/runs/${runId}/cancel`, { method: "POST" }),
  approveGate: (runId: string, feedback?: string) =>
    request<void>(`/workflows/runs/${runId}/gate/approve`, {
      method: "POST",
      body: JSON.stringify({ feedback: feedback ?? null }),
    }),
  rejectGate: (runId: string) =>
    request<void>(`/workflows/runs/${runId}/gate/reject`, { method: "POST" }),
};
