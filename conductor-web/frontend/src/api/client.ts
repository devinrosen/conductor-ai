import type {
  Repo,
  Worktree,
  Ticket,
  Session,
  CreateRepoRequest,
  CreateWorktreeRequest,
  EndSessionRequest,
  SyncResult,
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

  // Tickets
  listTickets: (repoId: string) =>
    request<Ticket[]>(`/repos/${repoId}/tickets`),
  syncTickets: (repoId: string) =>
    request<SyncResult>(`/repos/${repoId}/tickets/sync`, { method: "POST" }),

  // Sessions
  listSessions: () => request<Session[]>("/sessions"),
  startSession: () =>
    request<Session>("/sessions", { method: "POST" }),
  endSession: (id: string, data: EndSessionRequest) =>
    request<void>(`/sessions/${id}/end`, {
      method: "POST",
      body: JSON.stringify(data),
    }),
};
