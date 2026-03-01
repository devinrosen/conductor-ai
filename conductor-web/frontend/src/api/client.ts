import type {
  Repo,
  Worktree,
  Ticket,
  TicketAgentTotals,
  TicketDetail,
  CreateRepoRequest,
  CreateWorktreeRequest,
  SyncResult,
  AgentRun,
  AgentEvent,
  AgentPromptInfo,
  WorkTarget,
  CreateWorkTargetRequest,
  PushResult,
  CreatePrResult,
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

  // Worktrees
  listWorktrees: (repoId: string) =>
    request<Worktree[]>(`/repos/${repoId}/worktrees`),
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

  // Tickets
  listAllTickets: () => request<Ticket[]>("/tickets"),
  listTickets: (repoId: string) =>
    request<Ticket[]>(`/repos/${repoId}/tickets`),
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
  startAgent: (worktreeId: string, prompt: string, resumeSessionId?: string) =>
    request<AgentRun>(`/worktrees/${worktreeId}/agent/start`, {
      method: "POST",
      body: JSON.stringify({
        prompt,
        resume_session_id: resumeSessionId ?? null,
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
};
