//! SSE response helpers. Wraps a `Stream<CanonicalChunk>` (or a single
//! `CanonicalResponse`) as the OpenAI-compatible SSE wire format.

use super::super::schema::canonical::{CanonicalChunk, CanonicalResponse};
use super::super::schema::outbound::{chunk_to_openai, done_sentinel, response_to_openai};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use std::convert::Infallible;

/// SSE stream from canonical chunks. Emits one `data: <json>` line per
/// chunk, plus the terminal `data: [DONE]`.
pub fn chunks_to_sse<S>(stream: S) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    S: Stream<Item = CanonicalChunk> + Send + 'static,
{
    use async_stream::stream;
    let body = stream! {
        tokio::pin!(stream);
        while let Some(chunk) = stream.next().await {
            if let Some(v) = chunk_to_openai(&chunk) {
                yield Ok(Event::default().data(v.to_string()));
            }
        }
        yield Ok(Event::default().data(done_sentinel().to_string()));
    };
    Sse::new(body).keep_alive(KeepAlive::new())
}

use futures::StreamExt;

/// Wrap a single `CanonicalResponse` as a non-streaming JSON value
/// (NOT as SSE — that's the streaming path's job). Just a helper to
/// keep handler code clean.
pub fn response_json(resp: CanonicalResponse) -> serde_json::Value {
    response_to_openai(&resp)
}
