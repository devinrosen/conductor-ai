use axum::Json;
use conductor_core::vantage::terminal_conductor_statuses;

/// GET /api/vantage/terminal-statuses
///
/// Returns the list of Vantage conductor statuses that represent a terminal
/// (done/approved) state. The frontend uses this list to determine whether a
/// blocked ticket's parent has reached a state that allows it to be unlocked,
/// avoiding a hardcoded duplicate of the Rust constant.
#[utoipa::path(
    get,
    path = "/api/vantage/terminal-statuses",
    responses(
        (status = 200, description = "List of terminal Vantage conductor statuses", body = Vec<String>),
    ),
    tag = "vantage",
)]
pub async fn get_terminal_statuses() -> Json<Vec<&'static str>> {
    Json(terminal_conductor_statuses().to_vec())
}
