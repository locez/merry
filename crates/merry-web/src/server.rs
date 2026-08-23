//! HTTP routing and lifecycle for the local Web service.

use crate::{WebArtifactContent, WebBackend, WebBackendError, WebServerConfig, assets};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::get,
};
use futures_core::stream::Stream;
use futures_util::StreamExt;
use merry_core::{ArtifactId, SessionId, TrajectoryEvent, TrajectorySnapshot};
use serde::Serialize;
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tokio_util::sync::CancellationToken;

const SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors raised while binding or owning the local Web server.
#[derive(Debug, Error)]
pub enum WebServerError {
    /// The configured listener could not be bound.
    #[error("could not bind Merry Web service to {address}: {source}")]
    Bind {
        /// Address that was requested.
        address: SocketAddr,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A non-loopback listener was rejected by the local-only policy.
    #[error("Merry Web service only permits loopback binding: {address}")]
    NonLoopbackBind {
        /// Address that was rejected.
        address: SocketAddr,
    },
    /// The caller requested a URL from a service that has not started.
    #[error("Merry Web service is not running")]
    NotRunning,
    /// The server stopped with an operating-system error.
    #[error("Merry Web service stopped: {source}")]
    Serve {
        /// Underlying server error.
        #[source]
        source: std::io::Error,
    },
    /// The owned server task could not be joined.
    #[error("Merry Web service task failed to join: {source}")]
    Join {
        /// Underlying task join error.
        #[source]
        source: tokio::task::JoinError,
    },
    /// The server did not finish graceful shutdown within the bounded wait.
    #[error("Merry Web service did not shut down within {timeout:?}")]
    ShutdownTimeout {
        /// Maximum time allowed for graceful shutdown.
        timeout: Duration,
    },
}

/// Running local Web service handle.
pub struct WebServerHandle {
    base_url: String,
    shutdown: CancellationToken,
    task: Option<JoinHandle<Result<(), WebServerError>>>,
}

impl WebServerHandle {
    /// Returns the base URL clients can use to reach the service.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns whether the owned server task has already stopped.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    /// Returns the trajectory page URL for a validated session id.
    #[must_use]
    pub fn session_trajectory_url(&self, session_id: &SessionId) -> String {
        format!(
            "{}/app/sessions/{}/trajectory",
            self.base_url,
            session_id.as_str()
        )
    }

    /// Requests graceful shutdown and waits for the server task.
    pub async fn shutdown(mut self) -> Result<(), WebServerError> {
        self.shutdown.cancel();
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match timeout(SERVER_SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(result) => result.map_err(|source| WebServerError::Join { source })?,
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(WebServerError::ShutdownTimeout {
                    timeout: SERVER_SHUTDOWN_TIMEOUT,
                })
            }
        }
    }
}

impl Drop for WebServerHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Starts the local Web service on the configured address.
pub async fn start<B>(
    backend: B,
    config: WebServerConfig,
) -> Result<WebServerHandle, WebServerError>
where
    B: WebBackend,
{
    let requested_addr = config.socket_addr();
    if !requested_addr.ip().is_loopback() {
        return Err(WebServerError::NonLoopbackBind {
            address: requested_addr,
        });
    }
    let listener =
        TcpListener::bind(requested_addr)
            .await
            .map_err(|source| WebServerError::Bind {
                address: requested_addr,
                source,
            })?;
    let local_addr = listener
        .local_addr()
        .map_err(|source| WebServerError::Bind {
            address: requested_addr,
            source,
        })?;
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let app = router(backend, shutdown.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(server_shutdown.cancelled_owned())
            .await
            .map_err(|source| WebServerError::Serve { source })
    });
    Ok(WebServerHandle {
        base_url: format!("http://{local_addr}"),
        shutdown,
        task: Some(task),
    })
}

struct AppState<B> {
    backend: Arc<B>,
    shutdown: CancellationToken,
}

impl<B> Clone for AppState<B> {
    fn clone(&self) -> Self {
        Self {
            backend: Arc::clone(&self.backend),
            shutdown: self.shutdown.clone(),
        }
    }
}

pub(crate) fn router<B>(backend: B, shutdown: CancellationToken) -> Router
where
    B: WebBackend,
{
    Router::new()
        .route("/app", get(assets::app_shell))
        .route("/app/", get(assets::app_shell))
        .route(
            "/app/sessions/{session_id}/trajectory",
            get(assets::app_shell),
        )
        .route("/assets/app.js", get(assets::app_js))
        .route(
            "/assets/trajectory-contract.js",
            get(assets::app_contract_js),
        )
        .route(
            "/assets/trajectory-contract.generated.js",
            get(assets::app_generated_contract_js),
        )
        .route(
            "/assets/trajectory-message-model.js",
            get(assets::app_message_model_js),
        )
        .route(
            "/assets/trajectory-timeline.js",
            get(assets::app_timeline_js),
        )
        .route("/assets/trajectory-format.js", get(assets::app_format_js))
        .route("/assets/trajectory-view.js", get(assets::app_view_js))
        .route("/assets/app.css", get(assets::app_css))
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/capabilities", get(capabilities))
        .route(
            "/api/v1/sessions/{session_id}/trajectory",
            get(trajectory_snapshot::<B>),
        )
        .route(
            "/api/v1/sessions/{session_id}/artifacts/{artifact_id}",
            get(artifact_content::<B>),
        )
        .route(
            "/api/v1/sessions/{session_id}/events",
            get(trajectory_events::<B>),
        )
        .with_state(AppState {
            backend: Arc::new(backend),
            shutdown,
        })
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
}

async fn healthz() -> impl IntoResponse {
    axum::Json(HealthResponse {
        status: "ok",
        service: "merry-web",
    })
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilitiesResponse {
    api_version: &'static str,
    routes: [&'static str; 5],
}

async fn capabilities() -> impl IntoResponse {
    axum::Json(CapabilitiesResponse {
        api_version: "v1",
        routes: [
            "/api/v1/healthz",
            "/api/v1/capabilities",
            "/api/v1/sessions/:session_id/trajectory",
            "/api/v1/sessions/:session_id/artifacts/:artifact_id",
            "/api/v1/sessions/:session_id/events",
        ],
    })
}

async fn trajectory_snapshot<B>(
    State(state): State<AppState<B>>,
    Path(raw_session_id): Path<String>,
) -> Result<axum::Json<TrajectorySnapshot>, ApiError>
where
    B: WebBackend,
{
    let session_id = parse_session_id(raw_session_id)?;
    state
        .backend
        .trajectory_snapshot(&session_id)
        .await
        .map(axum::Json)
        .map_err(ApiError::from)
}

async fn trajectory_events<B>(
    State(state): State<AppState<B>>,
    Path(raw_session_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError>
where
    B: WebBackend,
{
    let session_id = parse_session_id(raw_session_id)?;
    let stream = state
        .backend
        .trajectory_stream(&session_id)
        .await
        .map_err(ApiError::from)?;
    let stream = stream
        .take_until(state.shutdown.cancelled_owned())
        .map(|item| Ok(trajectory_sse_event(item)));
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

async fn artifact_content<B>(
    State(state): State<AppState<B>>,
    Path((raw_session_id, raw_artifact_id)): Path<(String, String)>,
) -> Result<axum::Json<WebArtifactContent>, ApiError>
where
    B: WebBackend,
{
    let session_id = parse_session_id(raw_session_id)?;
    let artifact_id = ArtifactId::new(&raw_artifact_id).map_err(|_| ApiError::InvalidArtifactId)?;
    state
        .backend
        .artifact_content(&session_id, &artifact_id)
        .await
        .map(axum::Json)
        .map_err(ApiError::from)
}

fn trajectory_sse_event(item: Result<TrajectoryEvent, WebBackendError>) -> Event {
    match item {
        Ok(event) => Event::default()
            .event("trajectory")
            .data(serialize_event(&event)),
        Err(error) => Event::default()
            .event("error")
            .data(serialize_sse_error(&error)),
    }
}

fn backend_error_code(error: &WebBackendError) -> &'static str {
    match error {
        WebBackendError::SessionUnavailable => "session_unavailable",
        WebBackendError::Unavailable => "backend_unavailable",
        WebBackendError::ResyncRequired => "resync_required",
    }
}

fn serialize_event(event: &TrajectoryEvent) -> String {
    match serde_json::to_string(event) {
        Ok(json) => json,
        Err(_) => "{\"type\":\"serialization_error\"}".to_owned(),
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SseErrorBody {
    code: &'static str,
    message: String,
}

fn serialize_sse_error(error: &WebBackendError) -> String {
    let body = SseErrorBody {
        code: backend_error_code(error),
        message: error.to_string(),
    };
    match serde_json::to_string(&body) {
        Ok(json) => json,
        Err(_) => "{\"code\":\"serialization_error\",\"message\":\"event serialization failed\"}"
            .to_owned(),
    }
}

fn parse_session_id(raw: String) -> Result<SessionId, ApiError> {
    SessionId::new(&raw).map_err(|_| ApiError::InvalidSessionId)
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ApiErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Debug)]
enum ApiError {
    InvalidSessionId,
    InvalidArtifactId,
    SessionUnavailable,
    BackendUnavailable,
}

impl From<WebBackendError> for ApiError {
    fn from(error: WebBackendError) -> Self {
        match error {
            WebBackendError::SessionUnavailable => Self::SessionUnavailable,
            WebBackendError::Unavailable | WebBackendError::ResyncRequired => {
                Self::BackendUnavailable
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::InvalidSessionId => (
                StatusCode::BAD_REQUEST,
                ApiErrorBody {
                    code: "invalid_session_id",
                    message: "session id is invalid",
                },
            ),
            Self::SessionUnavailable => (
                StatusCode::NOT_FOUND,
                ApiErrorBody {
                    code: "session_unavailable",
                    message: "session is not available",
                },
            ),
            Self::InvalidArtifactId => (
                StatusCode::BAD_REQUEST,
                ApiErrorBody {
                    code: "invalid_artifact_id",
                    message: "artifact id is invalid",
                },
            ),
            Self::BackendUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ApiErrorBody {
                    code: "backend_unavailable",
                    message: "trajectory backend is unavailable",
                },
            ),
        };
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use futures_util::{FutureExt, stream, stream::BoxStream};
    use merry_core::TrajectorySnapshot;
    use tower::ServiceExt;

    struct FakeBackend;

    fn test_router() -> Router {
        router(FakeBackend, CancellationToken::new())
    }

    impl WebBackend for FakeBackend {
        fn trajectory_snapshot(
            &self,
            session_id: &SessionId,
        ) -> futures_util::future::BoxFuture<'_, Result<TrajectorySnapshot, WebBackendError>>
        {
            let session_id = session_id.clone();
            async move { Ok(TrajectorySnapshot::new(session_id)) }.boxed()
        }

        fn trajectory_stream(
            &self,
            session_id: &SessionId,
        ) -> futures_util::future::BoxFuture<
            '_,
            Result<BoxStream<'static, Result<TrajectoryEvent, WebBackendError>>, WebBackendError>,
        > {
            let event = TrajectoryEvent::Snapshot {
                snapshot: TrajectorySnapshot::new(session_id.clone()),
            };
            async move { Ok(stream::once(async move { Ok(event) }).boxed()) }.boxed()
        }

        fn artifact_content(
            &self,
            _session_id: &SessionId,
            artifact_id: &ArtifactId,
        ) -> futures_util::future::BoxFuture<'_, Result<WebArtifactContent, WebBackendError>>
        {
            let artifact_id = artifact_id.clone();
            async move {
                Ok(WebArtifactContent::new(
                    artifact_id,
                    crate::WebArtifactKind::Text,
                    Some("fixture".to_owned()),
                    false,
                    7,
                ))
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn health_and_trajectory_routes_are_separate_api_surfaces() {
        let app = test_router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/healthz")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("health response");
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/sessions/session-1/trajectory")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("trajectory response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn app_shell_and_assets_are_served_outside_the_api_namespace() {
        let app = test_router();
        for uri in [
            "/app/sessions/session-1/trajectory",
            "/assets/app.js",
            "/assets/trajectory-contract.js",
            "/assets/trajectory-contract.generated.js",
            "/assets/trajectory-message-model.js",
            "/assets/trajectory-timeline.js",
            "/assets/trajectory-format.js",
            "/assets/trajectory-view.js",
            "/assets/app.css",
            "/api/v1/sessions/session-1/artifacts/artifact-1",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("valid request"),
                )
                .await
                .expect("static response");
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn invalid_session_ids_are_rejected_at_the_http_boundary() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/sessions/%2E%2E/trajectory")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("trajectory response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn local_trajectory_routes_do_not_require_url_tokens() {
        let app = test_router();
        for uri in [
            "/api/v1/sessions/session-1/trajectory",
            "/api/v1/sessions/session-1/events",
            "/api/v1/sessions/session-1/artifacts/artifact-1",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("valid request"),
                )
                .await
                .expect("local trajectory response");
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn session_trajectory_url_contains_no_credentials() {
        let server = start(
            FakeBackend,
            WebServerConfig::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0),
        )
        .await
        .expect("local Web server starts");
        let session_id = SessionId::new("session-1").expect("valid session id");

        let url = server.session_trajectory_url(&session_id);
        assert_eq!(
            url,
            format!("{}/app/sessions/session-1/trajectory", server.base_url())
        );
        assert!(!url.contains('?'));

        server.shutdown().await.expect("local Web server stops");
    }

    #[test]
    fn default_web_port_is_1225_and_remote_bind_is_not_local() {
        let config = WebServerConfig::default();
        assert_eq!(config.port(), crate::DEFAULT_PORT);
        assert!(format!("{config:?}").contains("WebServerConfig"));
        assert!(!std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED).is_loopback());
    }

    #[test]
    fn artifact_byte_lengths_use_lossless_wire_strings() {
        let artifact = WebArtifactContent::new(
            ArtifactId::new("artifact-1").expect("valid artifact id"),
            crate::WebArtifactKind::Binary,
            None,
            false,
            9_007_199_254_740_993,
        );
        let value = serde_json::to_value(artifact).expect("artifact serializes");
        assert_eq!(value["byte_length"], "9007199254740993");
    }

    #[tokio::test]
    async fn server_rejects_non_loopback_binding_before_opening_a_listener() {
        let result = start(
            FakeBackend,
            WebServerConfig::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0),
        )
        .await;
        assert!(matches!(
            result,
            Err(WebServerError::NonLoopbackBind { .. })
        ));
    }
}
