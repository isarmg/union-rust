//! Absolute request-body upload deadline.
//!
//! A size limit cannot stop a chunked body that never ends. An inactivity timeout is also
//! insufficient because a peer can send one tiny chunk before every deadline forever. This
//! middleware measures one absolute interval from request admission until the body reaches EOF.
//! Once EOF is observed, the handler and response are allowed to run for as long as their own
//! contracts require.

use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderValue, Version, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http_body::Body as HttpBody;
use tokio::sync::oneshot;

use crate::error::AppError;

/// Shorter than the 60-second anonymous login window, so every admitted upload either finishes or
/// releases its task while its source-rate reservation is still active.
pub(super) const REQUEST_BODY_DEADLINE: Duration = Duration::from_secs(30);

pub(super) async fn enforce(
    State(deadline): State<Duration>,
    request: Request,
    next: Next,
) -> Response {
    // Photo/Dufs uploads have their own durable chunking, byte limits and idle/total deadlines.
    // Applying the console's 30-second whole-body deadline outside the streaming gateway would
    // abort valid large uploads before the worker contract can account for them.
    if crate::platform::is_compiled_gateway_path(request.uri().path()) {
        return next.run(request).await;
    }
    // Empty request bodies cannot hold the server open. Avoid allocating a channel and timer for
    // the overwhelmingly common GET/HEAD requests and body-less mutations.
    if request.body().is_end_stream() {
        return next.run(request).await;
    }

    let close_connection = matches!(request.version(), Version::HTTP_10 | Version::HTTP_11);
    let expires_at = tokio::time::Instant::now() + deadline;
    let (finished_tx, mut finished_rx) = oneshot::channel();
    let request = request.map(|body| Body::new(CompletionObservedBody::new(body, finished_tx)));
    let response = next.run(request);
    tokio::pin!(response);
    let deadline_timer = tokio::time::sleep_until(expires_at);
    tokio::pin!(deadline_timer);

    tokio::select! {
        // Prefer a body/response that completed exactly on the boundary over a timeout response.
        biased;
        finished = &mut finished_rx => {
            match finished {
                Ok(finished_at) if finished_at <= expires_at => response.await,
                // The sender always publishes from `finish`/`Drop`; a closed channel is therefore
                // an invariant failure and must not silently disable the deadline.
                Ok(_) | Err(_) => timeout_response(deadline, close_connection),
            }
        },
        response = &mut response => response,
        _ = &mut deadline_timer => {
            // `response` is polled before the timer in this biased select. That poll may consume
            // the final frame and publish completion while still leaving the handler Pending. The
            // receiver was already polled earlier in the same round, so check it once more before
            // deciding that the body actually missed its absolute deadline.
            match finished_rx.try_recv() {
                Ok(finished_at) if finished_at <= expires_at => response.await,
                Ok(_) | Err(_) => timeout_response(deadline, close_connection),
            }
        }
    }
}

fn timeout_response(deadline: Duration, close_connection: bool) -> Response {
    let mut response = AppError::RequestTimeout(format!(
        "request body was not completed within {} seconds",
        deadline.as_secs_f64()
    ))
    .into_response();
    // An HTTP/1 connection cannot safely parse another request while bytes belonging to the
    // timed-out body may still arrive. HTTP/2 cancels only the affected stream and forbids the
    // Connection header.
    if close_connection {
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

struct CompletionObservedBody {
    inner: Body,
    finished: Option<oneshot::Sender<tokio::time::Instant>>,
}

impl CompletionObservedBody {
    fn new(inner: Body, finished: oneshot::Sender<tokio::time::Instant>) -> Self {
        Self {
            inner,
            finished: Some(finished),
        }
    }

    fn finish(&mut self) {
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(tokio::time::Instant::now());
        }
    }
}

impl Drop for CompletionObservedBody {
    fn drop(&mut self) {
        // A handler may intentionally ignore or stop reading a body. Once it drops the body, that
        // upload can no longer retain the request task and the middleware deadline is unnecessary.
        self.finish();
    }
}

impl HttpBody for CompletionObservedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.inner).poll_frame(context);
        if matches!(frame, Poll::Ready(None) | Poll::Ready(Some(Err(_))))
            || this.inner.is_end_stream()
        {
            this.finish();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode, routing::post};
    use futures_util::stream;
    use tower::ServiceExt;

    fn with_deadline(router: Router, deadline: Duration) -> Router {
        router.layer(axum::middleware::from_fn_with_state(deadline, enforce))
    }

    #[tokio::test]
    async fn periodic_chunks_cannot_extend_the_absolute_deadline() {
        let app = with_deadline(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async { StatusCode::NO_CONTENT }),
            ),
            Duration::from_millis(30),
        );
        let body = Body::from_stream(stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Some((Ok::<_, std::io::Error>(Bytes::from_static(b"x")), ()))
        }));

        let response = tokio::time::timeout(
            Duration::from_millis(500),
            app.oneshot(
                Request::post("/upload")
                    .body(body)
                    .expect("build dripping request"),
            ),
        )
        .await
        .expect("dripping body outlived the test deadline")
        .expect("run upload route");

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(response.headers()[header::CONNECTION], "close");
        let payload: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .expect("read timeout response"),
        )
        .expect("decode timeout response");
        assert_eq!(payload["code"], "request_timeout");
    }

    #[tokio::test]
    async fn completed_body_disables_the_deadline_for_handler_work() {
        let app = with_deadline(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async {
                    tokio::time::sleep(Duration::from_millis(75)).await;
                    StatusCode::NO_CONTENT
                }),
            ),
            Duration::from_millis(20),
        );

        let response = app
            .oneshot(
                Request::post("/upload")
                    .body(Body::from("complete"))
                    .expect("build complete request"),
            )
            .await
            .expect("run slow handler");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn http2_timeout_does_not_emit_a_connection_header() {
        let app = with_deadline(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async { StatusCode::NO_CONTENT }),
            ),
            Duration::from_millis(20),
        );
        let body = Body::from_stream(stream::pending::<Result<Bytes, std::io::Error>>());
        let response = app
            .oneshot(
                Request::post("/upload")
                    .version(Version::HTTP_2)
                    .body(body)
                    .expect("build unfinished HTTP/2 request"),
            )
            .await
            .expect("run HTTP/2 upload route");

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(!response.headers().contains_key(header::CONNECTION));
    }
}
