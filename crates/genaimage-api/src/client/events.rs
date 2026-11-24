use std::pin::Pin;

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{header, RequestBuilder};

use super::{has_content_type, Client, Error};
use crate::{EventEnvelope, ResyncEvent};

#[derive(Debug, Default)]
pub struct EventsQuery<'a> {
    pub run_id: Option<&'a str>,
    /// A replay cursor from a previous event on the same bus.
    pub last_event_id: Option<u64>,
}

pub enum StreamEvent {
    Event(Box<EventEnvelope>),
    /// The server could not replay or continue the stream. Refresh state
    /// before opening a new subscription.
    Resync(ResyncEvent),
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, Error>> + Send>>;

impl Client {
    /// Opens an SSE stream. Heartbeat comments are ignored; there is no
    /// automatic reconnect. Use the last event ID to request replay explicitly.
    pub async fn events(&self, query: &EventsQuery<'_>) -> Result<EventStream, Error> {
        let request = event_options(self.get(&["events"])?, query)
            .header(header::ACCEPT, "text/event-stream");
        let response = self.send(request, false).await?;
        if !has_content_type(&response, "text/event-stream") {
            return Err(Error::UnexpectedEventContentType);
        }
        Ok(Box::pin(response.bytes_stream().eventsource().map(
            |frame| {
                let frame = frame?;
                if frame.event == "resync" {
                    Ok(StreamEvent::Resync(serde_json::from_str(&frame.data)?))
                } else {
                    Ok(StreamEvent::Event(serde_json::from_str(&frame.data)?))
                }
            },
        )))
    }
}

pub(super) fn event_options(
    mut request: RequestBuilder,
    query: &EventsQuery<'_>,
) -> RequestBuilder {
    if let Some(run_id) = query.run_id {
        request = request.query(&[("run_id", run_id)]);
    }
    if let Some(id) = query.last_event_id {
        request = request.header("last-event-id", id.to_string());
    }
    request
}
