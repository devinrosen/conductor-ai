mod query;
mod syncer;

pub use syncer::TicketSyncer;

use serde::{Deserialize, Serialize};

use crate::error::{ConductorError, Result};

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub repo_id: String,
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub url: String,
    pub synced_at: String,
    pub raw_json: String,
    pub workflow: Option<String>,
    pub agent_map: Option<String>,
}

/// A normalized ticket from any source, ready to be upserted into the database.
pub struct TicketInput {
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub url: String,
    pub raw_json: Option<String>,
    /// Label details (name + color) for populating the ticket_labels join table.
    /// Pass `vec![]` for sources that do not supply color data.
    pub label_details: Vec<TicketLabelInput>,
    /// Source IDs (within the same source_type) of tickets that block this one.
    /// Resolved to internal ULIDs and written to ticket_dependencies during upsert.
    pub blocked_by: Vec<String>,
    /// Source IDs of child tickets (this ticket is the parent).
    /// Resolved to internal ULIDs and written to ticket_dependencies during upsert.
    pub children: Vec<String>,
    /// Source ID of the parent ticket (this ticket is a child).
    /// Resolved and written to ticket_dependencies during upsert.
    /// Setting this replaces any existing parent relationship for this ticket.
    pub parent: Option<String>,
}

pub(super) const VALID_TICKET_STATES: &[&str] = &["open", "in_progress", "closed"];

impl TicketInput {
    /// Validate this ticket input, returning an error if any field is invalid.
    pub fn validate(&self) -> Result<()> {
        if !VALID_TICKET_STATES.contains(&self.state.as_str()) {
            return Err(crate::error::ConductorError::InvalidInput(format!(
                "Invalid ticket state '{}'. Must be one of: open, in_progress, closed.",
                self.state
            )));
        }
        Ok(())
    }

    fn labels_json(&self) -> String {
        serde_json::to_string(&self.labels).unwrap_or_else(|_| "[]".to_string())
    }
}

/// Label detail passed in during sync. Carries color alongside the name.
pub struct TicketLabelInput {
    pub name: String,
    pub color: Option<String>,
}

/// A label row from the ticket_labels table.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLabel {
    pub ticket_id: String,
    pub label: String,
    pub color: Option<String>,
}

/// Dependency relationships for a single ticket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TicketDependencies {
    /// Tickets that must complete before this one (blocks this ticket).
    pub blocked_by: Vec<Ticket>,
    /// Tickets that this ticket blocks.
    pub blocks: Vec<Ticket>,
    /// Parent ticket, if any.
    pub parent: Option<Ticket>,
    /// Child tickets.
    pub children: Vec<Ticket>,
}

impl TicketDependencies {
    /// Returns `true` if this ticket has at least one unresolved (non-closed) blocker.
    pub fn is_actively_blocked(&self) -> bool {
        self.blocked_by.iter().any(|b| b.state != "closed")
    }

    /// Returns an iterator over unresolved (non-closed) blockers.
    pub fn active_blockers(&self) -> impl Iterator<Item = &Ticket> {
        self.blocked_by.iter().filter(|b| b.state != "closed")
    }
}

/// A ticket that is ready to be worked on: not closed, has no unresolved blockers,
/// and is not already linked to an active workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyTicket {
    pub id: String,
    pub source_id: String,
    pub title: String,
    pub url: String,
    /// The dep_type of an incoming parent_of edge, if any ('parent_of'), or `None` for
    /// unconstrained tickets with no dependency edges pointing at them.
    pub dep_type: Option<String>,
}

/// Filter options for [`TicketSyncer::list_filtered`].
#[derive(Default)]
pub struct TicketFilter {
    /// Only include tickets that have ALL of these labels.
    /// NOTE: label filtering uses the `ticket_labels` join table, which is only
    /// populated when `label_details` are provided during upsert. Tickets synced
    /// without label details will never match a label filter even if their JSON
    /// `labels` field is non-empty.
    pub labels: Vec<String>,
    /// Case-insensitive substring match against ticket title and body (ASCII only).
    pub search: Option<String>,
    /// When `false` (default), only open tickets are returned.
    pub include_closed: bool,
    /// When `true`, only include tickets with no entries in `ticket_labels`.
    pub unlabeled_only: bool,
}

impl Ticket {
    pub fn matches_filter(&self, query: &str) -> bool {
        self.title.to_lowercase().contains(query)
            || self.source_id.contains(query)
            || self.labels.to_lowercase().contains(query)
    }
}

pub(super) fn ticket_not_found(
    id: impl Into<String>,
) -> impl FnOnce(rusqlite::Error) -> ConductorError {
    let id = id.into();
    move |e| match e {
        rusqlite::Error::QueryReturnedNoRows => ConductorError::TicketNotFound { id },
        _ => ConductorError::Database(e),
    }
}

/// Build a rich agent prompt from a ticket's context.
pub fn build_agent_prompt(ticket: &Ticket) -> String {
    let labels_display = if ticket.labels.is_empty() || ticket.labels == "[]" {
        "None".to_string()
    } else {
        ticket.labels.clone()
    };

    let body_display = if ticket.body.is_empty() {
        "(No description provided)".to_string()
    } else {
        ticket.body.clone()
    };

    format!(
        "Work on the following GitHub issue in this repository.\n\
         \n\
         Issue: #{source_id} — {title}\n\
         State: {state}\n\
         Labels: {labels}\n\
         \n\
         Description:\n\
         {body}\n\
         \n\
         Implement the changes described in the issue. Follow existing code conventions and patterns. Write tests if appropriate.",
        source_id = ticket.source_id,
        title = ticket.title,
        state = ticket.state,
        labels = labels_display,
        body = body_display,
    )
}

#[cfg(test)]
mod tests;
