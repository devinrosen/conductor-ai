use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::state::AppState;

pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Some(Ok(Event::default().event(event.event_name()).data(data)))
        }
        Err(_) => {
            // Client fell behind the broadcast buffer. Send a hint so the
            // frontend knows to re-fetch all data, then continue the stream.
            Some(Ok(Event::default()
                .event("lagged")
                .data("{\"missed\":true}")))
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
