use std::sync::mpsc::Sender;

use crate::events::{EngineEventData, EventSink};

/// An `EventSink` that forwards events over an `mpsc` channel.
///
/// Send errors (i.e. the receiver was dropped) are silently ignored so a
/// disconnected receiver does not affect the run.
pub struct ChannelEventSink(pub Sender<EngineEventData>);

impl EventSink for ChannelEventSink {
    fn emit(&self, event: &EngineEventData) {
        let _ = self.0.send(event.clone());
    }
}
