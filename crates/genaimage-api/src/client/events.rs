use std::pin::Pin;

use bytes::{Bytes, BytesMut};
use eventsource_stream::Eventsource;
use futures_util::{stream, Stream, StreamExt};
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
        Ok(Box::pin(
            with_safe_preamble(response.bytes_stream())
                .eventsource()
                .map(|frame| {
                    let frame = frame?;
                    if frame.event == "resync" {
                        Ok(StreamEvent::Resync(serde_json::from_str(&frame.data)?))
                    } else {
                        Ok(StreamEvent::Event(serde_json::from_str(&frame.data)?))
                    }
                }),
        ))
    }
}

const BOM: &[u8] = "\u{feff}".as_bytes();

fn with_safe_preamble<E>(
    source: impl Stream<Item = Result<Bytes, E>>,
) -> impl Stream<Item = Result<Bytes, E>> {
    let body = stream::unfold(
        (Box::pin(source.fuse()), Some(0)),
        |(mut source, prefix)| async move {
            let Some(mut matched) = prefix else {
                return source.next().await.map(|item| (item, (source, None)));
            };
            loop {
                let item = match source.next().await {
                    Some(Ok(chunk)) => chunk,
                    Some(Err(error)) => return Some((Err(error), (source, Some(matched)))),
                    None if matched != 0 => {
                        return Some((Ok(Bytes::from_static(&BOM[..matched])), (source, None)));
                    }
                    None => return None,
                };
                let remaining = &BOM[matched..];
                if item.starts_with(remaining) {
                    return Some((Ok(item.slice(remaining.len()..)), (source, None)));
                }
                if remaining.starts_with(&item) {
                    matched += item.len();
                    continue;
                }
                let item = if matched == 0 {
                    item
                } else {
                    let mut joined = BytesMut::with_capacity(matched + item.len());
                    joined.extend_from_slice(&BOM[..matched]);
                    joined.extend_from_slice(&item);
                    joined.freeze()
                };
                return Some((Ok(item), (source, None)));
            }
        },
    );
    // Avoid eventsource-stream 0.2.3's byte-1 slice of a leading BOM. Only
    // our filter may strip a BOM; a second BOM belongs to the stream data.
    stream::once(async { Ok(Bytes::from_static(b":\n\n")) }).chain(body)
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{FutureExt, TryStreamExt};
    use std::io;

    #[test]
    fn strips_bom_split_across_chunks_before_sse_decoding() {
        for chunks in [
            vec![&b"\xEF"[..], b"", b"\xBB", b"\xBFdata: first\n\n"],
            vec![&b"\xEF\xBB\xBF"[..], b"data: first\n\n"],
            vec![&b"data: first\n\n"[..]],
        ] {
            let input = stream::iter(
                chunks
                    .into_iter()
                    .map(|chunk| Ok::<_, io::Error>(Bytes::from_static(chunk))),
            );
            let events = with_safe_preamble(input)
                .eventsource()
                .try_collect::<Vec<_>>()
                .now_or_never()
                .unwrap()
                .unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].data, "first");
        }
    }
}
