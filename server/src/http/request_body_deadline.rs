//! Core-enforced absolute request-body limits.
//!
//! A size limit cannot stop a chunked body that never ends. An inactivity timeout is also
//! insufficient because a peer can send one tiny chunk before every deadline forever. This
//! middleware therefore enforces both an absolute interval from request admission to body EOF and
//! a byte ceiling while the body streams through Core. Module gateway routes obtain their limits
//! from the most-specific Manifest route; every other route uses the conservative Core default.

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

#[derive(Clone)]
pub(super) struct IngressPolicy {
    platform: Option<crate::platform::PlatformState>,
    default_deadline: Duration,
    default_max_bytes: u64,
}

impl IngressPolicy {
    pub(super) fn with_platform(
        platform: crate::platform::PlatformState,
        default_deadline: Duration,
        default_max_bytes: u64,
    ) -> Self {
        Self {
            platform: Some(platform),
            default_deadline,
            default_max_bytes,
        }
    }

    #[cfg(test)]
    fn fallback(default_deadline: Duration, default_max_bytes: u64) -> Self {
        Self {
            platform: None,
            default_deadline,
            default_max_bytes,
        }
    }

    async fn resolve(&self, method: &axum::http::Method, path: &str) -> (Duration, u64) {
        let Some((module, suffix)) = module_api_path(path) else {
            return (self.default_deadline, self.default_max_bytes);
        };
        let Some(platform) = self.platform.as_ref() else {
            return (self.default_deadline, self.default_max_bytes);
        };
        let Some(policy) = platform
            .route_request_body_policy(module, method, suffix)
            .await
        else {
            return (self.default_deadline, self.default_max_bytes);
        };
        (
            Duration::from_secs(u64::from(policy.total_timeout_seconds)),
            policy.max_bytes,
        )
    }
}

pub(super) async fn enforce(
    State(policy): State<IngressPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let close_connection = matches!(request.version(), Version::HTTP_10 | Version::HTTP_11);
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let (deadline, max_bytes) = policy.resolve(&method, &path).await;

    // Reject a declared or exact lower-bound size before a worker connection is opened. Streaming
    // enforcement below remains authoritative for chunked bodies and dishonest Content-Length.
    if declared_length(&request).is_some_and(|length| length > max_bytes)
        || request.body().size_hint().lower() > max_bytes
    {
        return payload_too_large_response(max_bytes, close_connection);
    }

    // Empty request bodies cannot hold the server open. Avoid allocating a channel and timer for
    // the overwhelmingly common GET/HEAD requests and body-less mutations.
    if request.body().is_end_stream() {
        return next.run(request).await;
    }

    let expires_at = tokio::time::Instant::now() + deadline;
    let (finished_tx, mut finished_rx) = oneshot::channel();
    let request =
        request.map(|body| Body::new(CompletionObservedBody::new(body, max_bytes, finished_tx)));
    let response = next.run(request);
    tokio::pin!(response);
    let deadline_timer = tokio::time::sleep_until(expires_at);
    tokio::pin!(deadline_timer);

    tokio::select! {
        // Prefer an outcome published exactly on the boundary over a timeout response.
        biased;
        finished = &mut finished_rx => outcome_response(
            finished,
            &mut response,
            expires_at,
            deadline,
            max_bytes,
            close_connection,
        ).await,
        response_value = &mut response => {
            // Polling the response may consume the frame that publishes TooLarge. The receiver was
            // already polled in this select round, so inspect it again before returning a worker's
            // transport error instead of the stable Core 413 contract.
            match finished_rx.try_recv() {
                Ok(BodyOutcome::TooLarge) => payload_too_large_response(max_bytes, close_connection),
                _ => response_value,
            }
        },
        _ = &mut deadline_timer => {
            // `response` is polled before the timer in this biased select. That poll may consume
            // the final frame and publish completion while still leaving the handler Pending.
            match finished_rx.try_recv() {
                Ok(BodyOutcome::Completed(finished_at)) if finished_at <= expires_at => response.await,
                Ok(BodyOutcome::TooLarge) => payload_too_large_response(max_bytes, close_connection),
                Ok(BodyOutcome::Completed(_)) | Err(_) => timeout_response(deadline, close_connection),
            }
        }
    }
}

async fn outcome_response<F>(
    outcome: Result<BodyOutcome, oneshot::error::RecvError>,
    response: &mut Pin<&mut F>,
    expires_at: tokio::time::Instant,
    deadline: Duration,
    max_bytes: u64,
    close_connection: bool,
) -> Response
where
    F: std::future::Future<Output = Response>,
{
    match outcome {
        Ok(BodyOutcome::Completed(finished_at)) if finished_at <= expires_at => response.await,
        Ok(BodyOutcome::TooLarge) => payload_too_large_response(max_bytes, close_connection),
        Ok(BodyOutcome::Completed(_)) | Err(_) => timeout_response(deadline, close_connection),
    }
}

fn declared_length(request: &Request) -> Option<u64> {
    request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn module_api_path(path: &str) -> Option<(&str, &str)> {
    let remainder = path.strip_prefix("/api/modules/")?;
    let (module, suffix) = remainder
        .split_once('/')
        .map_or((remainder, "/"), |(module, suffix)| (module, suffix));
    if module.is_empty() {
        return None;
    }
    if suffix == "/" {
        Some((module, suffix))
    } else {
        Some((module, path.get(path.len() - suffix.len() - 1..)?))
    }
}

fn timeout_response(deadline: Duration, close_connection: bool) -> Response {
    let response = AppError::RequestTimeout(format!(
        "request body was not completed within {} seconds",
        deadline.as_secs_f64()
    ))
    .into_response();
    close_http1(response, close_connection)
}

fn payload_too_large_response(max_bytes: u64, close_connection: bool) -> Response {
    let response = AppError::PayloadTooLarge(format!(
        "request body exceeds the endpoint limit of {max_bytes} bytes"
    ))
    .into_response();
    close_http1(response, close_connection)
}

fn close_http1(mut response: Response, close_connection: bool) -> Response {
    // Once Core stops consuming an HTTP/1 body, the remaining bytes cannot be parsed as a new
    // request. HTTP/2 cancels only the affected stream and forbids the Connection header.
    if close_connection {
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

#[derive(Debug)]
enum BodyOutcome {
    Completed(tokio::time::Instant),
    TooLarge,
}

struct CompletionObservedBody {
    inner: Body,
    max_bytes: u64,
    observed_bytes: u64,
    finished: Option<oneshot::Sender<BodyOutcome>>,
}

impl CompletionObservedBody {
    fn new(inner: Body, max_bytes: u64, finished: oneshot::Sender<BodyOutcome>) -> Self {
        Self {
            inner,
            max_bytes,
            observed_bytes: 0,
            finished: Some(finished),
        }
    }

    fn publish(&mut self, outcome: BodyOutcome) {
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(outcome);
        }
    }

    fn finish(&mut self) {
        self.publish(BodyOutcome::Completed(tokio::time::Instant::now()));
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
        if let Poll::Ready(Some(Ok(frame))) = &frame
            && let Some(data) = frame.data_ref()
        {
            this.observed_bytes = this
                .observed_bytes
                .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
            if this.observed_bytes > this.max_bytes {
                this.publish(BodyOutcome::TooLarge);
                return Poll::Ready(Some(Err(axum::Error::new(std::io::Error::other(
                    "request body exceeded the Core ingress limit",
                )))));
            }
        }
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

    fn with_policy(router: Router, deadline: Duration, max_bytes: u64) -> Router {
        router.layer(axum::middleware::from_fn_with_state(
            IngressPolicy::fallback(deadline, max_bytes),
            enforce,
        ))
    }

    #[tokio::test]
    async fn periodic_chunks_cannot_extend_the_absolute_deadline() {
        let app = with_policy(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async { StatusCode::NO_CONTENT }),
            ),
            Duration::from_millis(30),
            1024,
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
        let app = with_policy(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async {
                    tokio::time::sleep(Duration::from_millis(75)).await;
                    StatusCode::NO_CONTENT
                }),
            ),
            Duration::from_millis(20),
            1024,
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
    async fn streaming_body_over_the_limit_has_a_stable_413_contract() {
        let app = with_policy(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async { StatusCode::NO_CONTENT }),
            ),
            Duration::from_secs(1),
            3,
        );
        let body = Body::from_stream(stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"ab")),
            Ok::<_, std::io::Error>(Bytes::from_static(b"cd")),
        ]));

        let response = app
            .oneshot(
                Request::post("/upload")
                    .body(body)
                    .expect("build oversized request"),
            )
            .await
            .expect("run oversized upload");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response.headers()[header::CONNECTION], "close");
        let payload: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .expect("read payload-too-large body"),
        )
        .expect("decode payload-too-large response");
        assert_eq!(payload["code"], "payload_too_large");
    }

    #[tokio::test]
    async fn declared_oversized_http2_body_has_no_connection_header() {
        let app = with_policy(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async { StatusCode::NO_CONTENT }),
            ),
            Duration::from_secs(1),
            3,
        );
        let response = app
            .oneshot(
                Request::post("/upload")
                    .version(Version::HTTP_2)
                    .header(header::CONTENT_LENGTH, "4")
                    .body(Body::from("data"))
                    .expect("build oversized HTTP/2 request"),
            )
            .await
            .expect("run oversized HTTP/2 upload");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!response.headers().contains_key(header::CONNECTION));
    }

    #[tokio::test]
    async fn http2_timeout_does_not_emit_a_connection_header() {
        let app = with_policy(
            Router::new().route(
                "/upload",
                post(|_body: Bytes| async { StatusCode::NO_CONTENT }),
            ),
            Duration::from_millis(20),
            1024,
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

    #[test]
    fn parses_module_root_and_nested_suffixes() {
        assert_eq!(
            module_api_path("/api/modules/photo-backup"),
            Some(("photo-backup", "/"))
        );
        assert_eq!(
            module_api_path("/api/modules/photo-backup/v1/uploads/one"),
            Some(("photo-backup", "/v1/uploads/one"))
        );
        assert_eq!(module_api_path("/api/modules/"), None);
        assert_eq!(module_api_path("/overview"), None);
    }
}
