import type {
  Repo,
  Worktree,
  WorktreeWithStatus,
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
  IssueSource,
  CreateIssueSourceRequest,
  DiscoverableRepo,
  GlobalConfig,
  KnownModel,
  WorkflowDef,
  WorkflowDefSummary,
  WorkflowRun,
  WorkflowRunStep,
  RunWorkflowRequest,
  FeedbackRequest,
  Notification,
  ThemeUnlockStats,
  PushSubscribeRequest,
  VapidPublicKeyResponse,
  PushSubscribeResponse,
  WorkflowTokenAggregate,
  WorkflowTokenTrendRow,
  StepTokenHeatmapRow,
  WorkflowRunMetricsRow,
  WorkflowFailureRateTrendRow,
  StepFailureHeatmapRow,
  StepRetryAnalyticsRow,
  WorkflowPercentiles,
  WorkflowRegressionSignal,
  GateAnalyticsRow,
  PendingGateAnalyticsRow,
} from "./types";
import { getApiBaseUrl } from "./transport";

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const base = await getApiBaseUrl();
  const res = await fetch(`${base}${path}`, {
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
  registerRepo: (data: CreateRepoRequest) =>
    request<Repo>("/repos", { method: "POST", body: JSON.stringify(data) }),
  unregisterRepo: (id: string) =>
    request<void>(`/repos/${id}`, { method: "DELETE" }),
  setRepoModel: (id: string, model: string | null) =>
    request<Repo>(`/repos/${id}/model`, {
      method: "PATCH",
      body: JSON.stringify({ model }),
    }),

  // Worktrees
  listAllWorktrees: (showCompleted = false) =>
    request<WorktreeWithStatus[]>(
      showCompleted ? `/worktrees?show_completed=true` : `/worktrees`,
    ),
  listWorktrees: (repoId: string, showCompleted = false) =>
    request<WorktreeWithStatus[]>(
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
    request<void>(`/worktrees/${id}`, { method: "DELETE" }),
  linkTicket: (id: string, ticketId: string) =>
    request<Worktree>(`/worktrees/${id}/ticket`, {
      method: "PUT",
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
    request<TicketDetail>(`/tickets/${ticketId}`),

  // Agent stats (aggregates)
  latestRunsByWorktree: () =>
    request<Record<string, AgentRun>>("/agent/latest-runs"),
  ticketAgentTotals: () =>
    request<Record<string, TicketAgentTotals>>("/agent/ticket-totals"),
  latestRunsByWorktreeForRepo: (repoId: string) =>
    request<Record<string, AgentRun>>(`/repos/${repoId}/agent/latest-runs`),
  ticketAgentTotalsForRepo: (repoId: string) =>
    request<Record<string, TicketAgentTotals>>(`/repos/${repoId}/agent/ticket-totals`),

  // Repo-scoped agents (read-only)
  startRepoAgent: (repoId: string, prompt: string, newSession?: boolean) =>
    request<AgentRun>(`/repos/${repoId}/agent/start`, {
      method: "POST",
      body: JSON.stringify({ prompt, new_session: newSession ?? false }),
    }),
  listRepoAgentRuns: (repoId: string) =>
    request<AgentRun[]>(`/repos/${repoId}/agent/runs`),
  stopRepoAgent: (repoId: string, runId: string) =>
    request<AgentRun>(`/repos/${repoId}/agent/${runId}/stop`, {
      method: "POST",
    }),
  getRepoAgentEvents: (repoId: string, runId: string) =>
    request<AgentEvent[]>(`/repos/${repoId}/agent/${runId}/events`),

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
  getRunEvents: (worktreeId: string, runId: string) =>
    request<AgentEvent[]>(`/worktrees/${worktreeId}/agent/runs/${runId}/events`),
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

  // Agent Feedback
  getPendingFeedback: (worktreeId: string) =>
    request<FeedbackRequest | null>(`/worktrees/${worktreeId}/agent/feedback`),
  submitFeedback: (worktreeId: string, feedbackId: string, response: string) =>
    request<FeedbackRequest>(`/worktrees/${worktreeId}/agent/feedback/${feedbackId}/respond`, {
      method: "POST",
      body: JSON.stringify({ response }),
    }),
  dismissFeedback: (worktreeId: string, feedbackId: string) =>
    request<void>(`/worktrees/${worktreeId}/agent/feedback/${feedbackId}/dismiss`, {
      method: "POST",
    }),

  // Workflows
  listWorkflowDefs: (worktreeId: string) =>
    request<WorkflowDefSummary[]>(`/worktrees/${worktreeId}/workflows/defs`),
  getWorkflowDef: (worktreeId: string, name: string) =>
    request<WorkflowDef>(`/worktrees/${worktreeId}/workflows/defs/${encodeURIComponent(name)}`),
  runWorkflow: (worktreeId: string, data: RunWorkflowRequest) =>
    request<{ status: string; worktree_id: string }>(`/worktrees/${worktreeId}/workflows/run`, {
      method: "POST",
      body: JSON.stringify(data),
    }),
  listAllWorkflowRuns: (statuses?: string[]) => {
    const params = statuses && statuses.length > 0 ? `?status=${statuses.join(",")}` : "";
    return request<WorkflowRun[]>(`/workflows/runs${params}`);
  },
  listWorkflowRuns: (worktreeId: string) =>
    request<WorkflowRun[]>(`/worktrees/${worktreeId}/workflows/runs`),
  getWorkflowRun: (runId: string) =>
    request<WorkflowRun | null>(`/workflows/runs/${runId}`),
  getWorkflowSteps: (runId: string) =>
    request<WorkflowRunStep[]>(`/workflows/runs/${runId}/steps`),
  getChildWorkflowRuns: (runId: string) =>
    request<WorkflowRun[]>(`/workflows/runs/${runId}/children`),
  cancelWorkflow: (runId: string) =>
    request<void>(`/workflows/runs/${runId}/cancel`, { method: "POST" }),
  approveGate: (runId: string, feedback?: string, selections?: string[]) =>
    request<void>(`/workflows/runs/${runId}/gate/approve`, {
      method: "POST",
      body: JSON.stringify({
        feedback: feedback ?? null,
        selections: selections ?? null,
      }),
    }),
  rejectGate: (runId: string) =>
    request<void>(`/workflows/runs/${runId}/gate/reject`, { method: "POST" }),

  // Workflow token analytics
  getWorkflowTokenAggregates: (repoId?: string) =>
    request<WorkflowTokenAggregate[]>(
      repoId ? `/workflows/analytics/aggregates?repo_id=${encodeURIComponent(repoId)}` : `/workflows/analytics/aggregates`,
    ),
  getWorkflowTokenTrend: (workflowName: string, granularity: "daily" | "weekly" = "daily") =>
    request<WorkflowTokenTrendRow[]>(
      `/workflows/analytics/trend?workflow_name=${encodeURIComponent(workflowName)}&granularity=${granularity}`,
    ),
  getStepTokenHeatmap: (workflowName: string, runs = 20) =>
    request<StepTokenHeatmapRow[]>(
      `/workflows/analytics/heatmap?workflow_name=${encodeURIComponent(workflowName)}&runs=${runs}`,
    ),
  getRunMetrics: (workflowName: string, days = 30) =>
    request<WorkflowRunMetricsRow[]>(
      `/workflows/analytics/runs?workflow_name=${encodeURIComponent(workflowName)}&days=${days}`,
    ),
  getWorkflowFailureRateTrend: (workflowName: string, granularity: "daily" | "weekly" = "daily") =>
    request<WorkflowFailureRateTrendRow[]>(
      `/workflows/analytics/failure-trend?workflow_name=${encodeURIComponent(workflowName)}&granularity=${granularity}`,
    ),
  getStepFailureHeatmap: (workflowName: string, runs = 20) =>
    request<StepFailureHeatmapRow[]>(
      `/workflows/analytics/failure-heatmap?workflow_name=${encodeURIComponent(workflowName)}&runs=${runs}`,
    ),
  getStepRetryAnalytics: (workflowName: string, runs = 20) =>
    request<StepRetryAnalyticsRow[]>(
      `/workflows/analytics/step-retries?workflow_name=${encodeURIComponent(workflowName)}&runs=${runs}`,
    ),
  getWorkflowPercentiles: (workflowName: string, days = 30) =>
    request<WorkflowPercentiles | null>(
      `/workflows/analytics/percentiles?workflow_name=${encodeURIComponent(workflowName)}&days=${days}`,
    ),
  getWorkflowRegressions: () =>
    request<WorkflowRegressionSignal[]>("/workflows/analytics/regressions"),
  getGateAnalytics: (workflowName: string, days = 30) =>
    request<GateAnalyticsRow[]>(
      `/workflows/analytics/gates?workflow_name=${encodeURIComponent(workflowName)}&days=${days}`,
    ),
  getPendingGates: () =>
    request<PendingGateAnalyticsRow[]>("/workflows/analytics/gates/pending"),

  // Notifications
  listNotifications: (unreadOnly = false, limit = 50, offset = 0) =>
    request<Notification[]>(
      `/notifications?unread_only=${unreadOnly}&limit=${limit}&offset=${offset}`,
    ),
  unreadNotificationCount: () =>
    request<{ count: number }>("/notifications/unread-count"),
  markNotificationRead: (id: string) =>
    request<void>(`/notifications/${id}/read`, { method: "POST" }),
  markAllNotificationsRead: () =>
    request<void>("/notifications/read", { method: "POST" }),

  // Stats
  getThemeUnlockStats: () =>
    request<ThemeUnlockStats>("/stats/theme-unlocks"),

  // Push Notifications
  getPushVapidKey: () =>
    request<VapidPublicKeyResponse>("/push/vapid-public-key"),
  subscribePush: (data: PushSubscribeRequest) =>
    request<PushSubscribeResponse>("/push/subscribe", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  unsubscribePush: (data: PushSubscribeRequest) =>
    request<void>("/push/subscribe", {
      method: "DELETE",
      body: JSON.stringify(data),
    }),
};

// Export as apiClient for consistency with hook usage
export const apiClient = api;