use utoipa::OpenApi;

#[allow(unused_imports)]
use conductor_core::agent::{
    AgentCreatedIssue, AgentRun, AgentRunEvent, AgentRunStatus, FeedbackOption, FeedbackRequest,
    FeedbackStatus, FeedbackType, PlanStep, RunTreeTotals, StepStatus, TicketAgentTotals,
};
#[allow(unused_imports)]
use conductor_core::conversation::{Conversation, ConversationScope, ConversationWithRuns};
#[allow(unused_imports)]
use conductor_core::feature::{FeatureRow, FeatureStatus};
#[allow(unused_imports)]
use conductor_core::github::{DiscoveredRepo, GithubPr};
#[allow(unused_imports)]
use conductor_core::issue_source::IssueSource;
#[allow(unused_imports)]
use conductor_core::notification_manager::{Notification, NotificationSeverity};
#[allow(unused_imports)]
use conductor_core::repo::Repo;
#[allow(unused_imports)]
use conductor_core::tickets::{Ticket, TicketLabel};
#[allow(unused_imports)]
use conductor_core::workflow::{
    BlockedOn, GateAnalyticsRow, GateType, PendingGateAnalyticsRow, StepFailureHeatmapRow,
    StepRetryAnalyticsRow, StepTokenHeatmapRow, WorkflowFailureRateTrendRow, WorkflowPercentiles,
    WorkflowRegressionSignal, WorkflowRun, WorkflowRunMetricsRow, WorkflowRunStatus,
    WorkflowRunStep, WorkflowStepStatus, WorkflowTokenAggregate, WorkflowTokenTrendRow,
};
#[allow(unused_imports)]
use conductor_core::worktree::{Worktree, WorktreeStatus, WorktreeWithStatus};

#[allow(unused_imports)]
use crate::routes::conversations::{
    CreateConversationRequest, ListConversationsQuery, RespondToFeedbackByIdRequest,
    RespondToFeedbackRequest, SendMessageRequest,
};
#[allow(unused_imports)]
use crate::routes::features::FeaturesResponse;
#[allow(unused_imports)]
use crate::routes::hooks::{HookSummary, TestHookRequest};
#[allow(unused_imports)]
use crate::routes::issue_sources::CreateIssueSourceRequest;
#[allow(unused_imports)]
use crate::routes::model_config::{
    GlobalModelResponse, KnownModelResponse, SetGlobalModelRequest, SuggestModelRequest,
    SuggestModelResponse,
};
#[allow(unused_imports)]
use crate::routes::notifications::{ListNotificationsQuery, UnreadCountResponse};
#[allow(unused_imports)]
use crate::routes::push::{PushSubscribeRequest, VapidPublicKeyResponse};
#[allow(unused_imports)]
use crate::routes::repos::{
    DiscoverReposQuery, DiscoverableRepo, RegisterRepoRequest,
    SetModelRequest as RepoSetModelRequest, UpdateRepoSettingsRequest,
};
#[allow(unused_imports)]
use crate::routes::stats::ThemeUnlockStats;
#[allow(unused_imports)]
use crate::routes::tickets::{SyncResult, TicketDetail, TicketListQuery, TicketListResponse};
#[allow(unused_imports)]
use crate::routes::workflows::{
    InputDeclSummary, InstantiateTemplateRequest, PostWorkflowRunRequest, RunWorkflowRequest,
    WorkflowDefSummary, WorkflowRunResponse,
};
#[allow(unused_imports)]
use crate::routes::worktrees::{
    CreateWorktreeRequest, CreateWorktreeResponse, LinkTicketRequest,
    SetModelRequest as WorktreeSetModelRequest, WorktreeListQuery,
};

/// OpenAPI documentation for the Conductor REST API.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Conductor API",
        description = "REST API for the Conductor multi-repo orchestration tool",
        version = "1.0.0"
    ),
    paths(
        // Health
        crate::routes::health::health,
        // Repos
        crate::routes::repos::list_repos,
        crate::routes::repos::register_repo,
        crate::routes::repos::unregister_repo,
        crate::routes::repos::patch_repo_model,
        crate::routes::repos::update_repo_settings,
        crate::routes::repos::list_github_orgs_handler,
        crate::routes::repos::discover_github_repos_handler,
        crate::routes::repos::list_prs,
        // Worktrees
        crate::routes::worktrees::list_all_worktrees,
        crate::routes::worktrees::list_worktrees,
        crate::routes::worktrees::create_worktree,
        crate::routes::worktrees::get_worktree,
        crate::routes::worktrees::delete_worktree,
        crate::routes::worktrees::get_worktree_for_repo,
        crate::routes::worktrees::delete_worktree_for_repo,
        crate::routes::worktrees::patch_worktree_model,
        crate::routes::worktrees::link_ticket,
        // Tickets
        crate::routes::tickets::list_ticket_labels,
        crate::routes::tickets::list_all_tickets,
        crate::routes::tickets::list_tickets,
        crate::routes::tickets::sync_tickets,
        crate::routes::tickets::ticket_detail,
        // Features
        crate::routes::features::list_features,
        // Agents
        crate::routes::agents::list_agent_runs,
        crate::routes::agents::list_all_agent_runs,
        crate::routes::agents::get_agent_run_by_id,
        crate::routes::agents::get_agent_run_feedback_by_run_id,
        crate::routes::agents::get_agent_run_events_by_id,
        crate::routes::agents::latest_runs_by_worktree,
        crate::routes::agents::ticket_totals,
        crate::routes::agents::latest_runs_by_worktree_for_repo,
        crate::routes::agents::ticket_totals_for_repo,
        crate::routes::agents::start_repo_agent,
        crate::routes::agents::list_repo_agent_runs,
        crate::routes::agents::stop_repo_agent,
        crate::routes::agents::repo_agent_events,
        crate::routes::agents::list_runs,
        crate::routes::agents::latest_run,
        crate::routes::agents::start_agent,
        crate::routes::agents::stop_agent,
        crate::routes::agents::get_events,
        crate::routes::agents::restart_agent,
        crate::routes::agents::get_run_events,
        crate::routes::agents::list_child_runs,
        crate::routes::agents::get_run_tree,
        crate::routes::agents::get_run_tree_totals,
        crate::routes::agents::get_prompt,
        crate::routes::agents::orchestrate_agent,
        crate::routes::agents::list_created_issues,
        crate::routes::agents::get_pending_feedback,
        crate::routes::agents::request_feedback,
        crate::routes::agents::submit_feedback,
        crate::routes::agents::dismiss_feedback,
        crate::routes::agents::list_run_feedback,
        // Conversations
        crate::routes::conversations::list_conversations,
        crate::routes::conversations::create_conversation,
        crate::routes::conversations::get_conversation,
        crate::routes::conversations::delete_conversation,
        crate::routes::conversations::send_message,
        crate::routes::conversations::respond_to_run_feedback,
        crate::routes::conversations::respond_to_feedback,
        // Workflows
        crate::routes::workflows::list_repo_workflow_defs,
        crate::routes::workflows::list_workflow_defs,
        crate::routes::workflows::get_workflow_def,
        crate::routes::workflows::run_workflow,
        crate::routes::workflows::list_workflow_runs,
        crate::routes::workflows::list_all_workflow_runs_handler,
        crate::routes::workflows::post_workflow_run,
        crate::routes::workflows::get_workflow_run,
        crate::routes::workflows::get_workflow_steps,
        crate::routes::workflows::get_workflow_step_log,
        crate::routes::workflows::get_child_workflow_runs,
        crate::routes::workflows::cancel_workflow,
        crate::routes::workflows::resume_workflow_endpoint,
        crate::routes::workflows::approve_gate,
        crate::routes::workflows::reject_gate,
        crate::routes::workflows::get_token_aggregates,
        crate::routes::workflows::get_token_trend,
        crate::routes::workflows::get_step_heatmap,
        crate::routes::workflows::get_run_metrics,
        crate::routes::workflows::get_failure_trend,
        crate::routes::workflows::get_failure_heatmap,
        crate::routes::workflows::get_step_retry_analytics,
        crate::routes::workflows::get_workflow_percentiles,
        crate::routes::workflows::get_workflow_regressions,
        crate::routes::workflows::get_gate_analytics,
        crate::routes::workflows::get_pending_gates,
        crate::routes::workflows::list_templates,
        crate::routes::workflows::instantiate_template,
        // Issue Sources
        crate::routes::issue_sources::list_issue_sources,
        crate::routes::issue_sources::create_issue_source,
        crate::routes::issue_sources::delete_issue_source,
        // Notifications
        crate::routes::notifications::list_notifications,
        crate::routes::notifications::unread_count,
        crate::routes::notifications::mark_all_read,
        crate::routes::notifications::mark_read,
        // Stats
        crate::routes::stats::theme_unlock_stats,
        // Push Notifications
        crate::routes::push::get_vapid_public_key,
        crate::routes::push::subscribe_push,
        crate::routes::push::unsubscribe_push,
        // Slack
        crate::routes::slack::handle_slash_command,
        // Model Config
        crate::routes::model_config::get_global_model,
        crate::routes::model_config::patch_global_model,
        crate::routes::model_config::list_known_models,
        crate::routes::model_config::suggest_model,
        // Hooks
        crate::routes::hooks::list_hooks,
        crate::routes::hooks::test_hook,
        // SSE events
        crate::routes::events::event_stream,
    ),
    components(
        schemas(
            // Core agent types
            AgentRun,
            PlanStep,
            AgentRunStatus,
            AgentRunEvent,
            FeedbackRequest,
            FeedbackOption,
            FeedbackStatus,
            FeedbackType,
            StepStatus,
            AgentCreatedIssue,
            TicketAgentTotals,
            RunTreeTotals,
            // Conversation types
            Conversation,
            ConversationScope,
            ConversationWithRuns,
            // Workflow types
            WorkflowRun,
            WorkflowRunStatus,
            WorkflowRunStep,
            WorkflowStepStatus,
            BlockedOn,
            GateType,
            WorkflowTokenAggregate,
            WorkflowTokenTrendRow,
            StepTokenHeatmapRow,
            WorkflowFailureRateTrendRow,
            StepFailureHeatmapRow,
            StepRetryAnalyticsRow,
            WorkflowPercentiles,
            WorkflowRegressionSignal,
            GateAnalyticsRow,
            PendingGateAnalyticsRow,
            WorkflowRunMetricsRow,
            // Ticket types
            Ticket,
            TicketLabel,
            // Repo types
            Repo,
            GithubPr,
            DiscoveredRepo,
            // Worktree types
            Worktree,
            WorktreeStatus,
            WorktreeWithStatus,
            // Notification types
            Notification,
            NotificationSeverity,
            // Issue source types
            IssueSource,
            // Feature types
            FeatureRow,
            FeatureStatus,
            // Web layer request/response types
            RegisterRepoRequest,
            DiscoverableRepo,
            DiscoverReposQuery,
            CreateWorktreeRequest,
            CreateWorktreeResponse,
            WorktreeListQuery,
            LinkTicketRequest,
            TicketListQuery,
            TicketListResponse,
            SyncResult,
            TicketDetail,
            FeaturesResponse,
            CreateConversationRequest,
            ListConversationsQuery,
            SendMessageRequest,
            RespondToFeedbackRequest,
            RespondToFeedbackByIdRequest,
            WorkflowDefSummary,
            InputDeclSummary,
            RunWorkflowRequest,
            WorkflowRunResponse,
            PostWorkflowRunRequest,
            InstantiateTemplateRequest,
            CreateIssueSourceRequest,
            ListNotificationsQuery,
            UnreadCountResponse,
            ThemeUnlockStats,
            VapidPublicKeyResponse,
            PushSubscribeRequest,
            GlobalModelResponse,
            SetGlobalModelRequest,
            KnownModelResponse,
            SuggestModelRequest,
            SuggestModelResponse,
            HookSummary,
            TestHookRequest,
        )
    )
)]
pub struct ApiDoc;
