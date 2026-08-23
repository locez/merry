//! CLI-owned adapters for the runtime-neutral local Web service.

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::BoxStream};
use merry_core::{ArtifactId, SessionId, TrajectoryEvent, TrajectorySnapshot};
use merry_runtime::{ArtifactContentKind, Runtime};
use merry_web::{
    WebArtifactContent, WebArtifactKind, WebBackend, WebBackendError, WebServerConfig,
    WebServerError, WebServerHandle,
};
use std::{
    net::{IpAddr, Ipv4Addr},
    process::Stdio,
};
use tokio::sync::broadcast;

const MAX_WEB_ARTIFACT_BYTES: usize = 1024 * 1024;

/// Adapts one runtime session to the local Web service boundary.
pub(crate) struct RuntimeWebBackend {
    runtime: Runtime,
}

/// Owns the local Web service for the lifetime of one TUI runtime.
pub(crate) struct RuntimeWebService {
    runtime: Runtime,
    server: Option<WebServerHandle>,
}

impl RuntimeWebService {
    /// Creates an unstarted service bound to one runtime session.
    #[must_use]
    pub(crate) fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            server: None,
        }
    }

    /// Starts the local service if it is not already running.
    pub(crate) async fn start(&mut self) -> Result<(), WebServerError> {
        if let Some(server) = self.server.as_ref()
            && !server.is_finished()
        {
            return Ok(());
        }
        if let Some(server) = self.server.take()
            && let Err(error) = server.shutdown().await
        {
            tracing::warn!(%error, "previous Web service task ended unexpectedly");
        }
        let server = start_server(self.runtime.clone()).await?;
        self.server = Some(server);
        Ok(())
    }

    /// Returns a session page URL, starting the service when necessary.
    pub(crate) async fn session_url(
        &mut self,
        session_id: &SessionId,
    ) -> Result<String, WebServerError> {
        self.start().await?;
        let Some(server) = self.server.as_ref() else {
            return Err(WebServerError::NotRunning);
        };
        Ok(server.session_trajectory_url(session_id))
    }

    /// Gracefully shuts down the owned service.
    pub(crate) async fn shutdown(&mut self) -> Result<(), WebServerError> {
        let Some(server) = self.server.take() else {
            return Ok(());
        };
        server.shutdown().await
    }
}

async fn start_server(runtime: Runtime) -> Result<WebServerHandle, WebServerError> {
    let default_config = WebServerConfig::default();
    match merry_web::start(RuntimeWebBackend::new(runtime.clone()), default_config).await {
        Err(WebServerError::Bind { source, .. })
            if source.kind() == std::io::ErrorKind::AddrInUse =>
        {
            merry_web::start(
                RuntimeWebBackend::new(runtime),
                WebServerConfig::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            )
            .await
        }
        result => result,
    }
}

impl RuntimeWebBackend {
    /// Creates a backend bound to one runtime-owned session.
    #[must_use]
    pub(crate) fn new(runtime: Runtime) -> Self {
        Self { runtime }
    }

    fn owns_session(&self, session_id: &SessionId) -> bool {
        self.runtime.session_id() == session_id
    }
}

impl WebBackend for RuntimeWebBackend {
    fn trajectory_snapshot(
        &self,
        session_id: &SessionId,
    ) -> BoxFuture<'_, Result<TrajectorySnapshot, WebBackendError>> {
        if !self.owns_session(session_id) {
            return async { Err(WebBackendError::SessionUnavailable) }.boxed();
        }
        let runtime = self.runtime.clone();
        async move {
            runtime.trajectory_snapshot().await.map_err(|error| {
                tracing::debug!(error = %error, "trajectory snapshot read failed");
                WebBackendError::Unavailable
            })
        }
        .boxed()
    }

    fn trajectory_stream(
        &self,
        session_id: &SessionId,
    ) -> BoxFuture<
        '_,
        Result<BoxStream<'static, Result<TrajectoryEvent, WebBackendError>>, WebBackendError>,
    > {
        if !self.owns_session(session_id) {
            return async { Err(WebBackendError::SessionUnavailable) }.boxed();
        }
        let runtime = self.runtime.clone();
        async move {
            let (snapshot, receiver) =
                runtime.trajectory_subscription().await.map_err(|error| {
                    tracing::debug!(error = %error, "trajectory subscription failed");
                    WebBackendError::Unavailable
                })?;
            Ok(trajectory_stream(
                snapshot.clone(),
                receiver,
                snapshot.is_closed(),
            ))
        }
        .boxed()
    }

    fn artifact_content(
        &self,
        session_id: &SessionId,
        artifact_id: &ArtifactId,
    ) -> BoxFuture<'_, Result<WebArtifactContent, WebBackendError>> {
        if !self.owns_session(session_id) {
            return async { Err(WebBackendError::SessionUnavailable) }.boxed();
        }
        let runtime = self.runtime.clone();
        let artifact_id = artifact_id.clone();
        async move {
            let content = runtime
                .read_artifact_preview(&artifact_id, MAX_WEB_ARTIFACT_BYTES)
                .await
                .map_err(|error| {
                    tracing::debug!(error = %error, %artifact_id, "artifact content read failed");
                    WebBackendError::Unavailable
                })?;
            let kind = match content.kind() {
                ArtifactContentKind::Text => WebArtifactKind::Text,
                ArtifactContentKind::Json => WebArtifactKind::Json,
                ArtifactContentKind::Binary => WebArtifactKind::Binary,
                ArtifactContentKind::Image => WebArtifactKind::Image,
                ArtifactContentKind::Other => WebArtifactKind::Other,
            };
            let byte_length =
                u64::try_from(content.byte_length()).map_err(|_| WebBackendError::Unavailable)?;
            Ok(WebArtifactContent::new(
                artifact_id,
                kind,
                content.content().map(str::to_owned),
                content.truncated(),
                byte_length,
            ))
        }
        .boxed()
    }
}

fn trajectory_stream(
    snapshot: TrajectorySnapshot,
    receiver: broadcast::Receiver<TrajectoryEvent>,
    closed: bool,
) -> BoxStream<'static, Result<TrajectoryEvent, WebBackendError>> {
    let initial =
        futures_util::stream::once(async move { Ok(TrajectoryEvent::Snapshot { snapshot }) });
    let updates = if closed {
        futures_util::stream::empty().boxed()
    } else {
        futures_util::stream::unfold(Some(receiver), |receiver| async move {
            let mut receiver = receiver?;
            match receiver.recv().await {
                Ok(event) => {
                    let terminal = matches!(&event, TrajectoryEvent::SessionClosed { .. });
                    Some((Ok(event), (!terminal).then_some(receiver)))
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Some((Err(WebBackendError::ResyncRequired), None))
                }
                Err(broadcast::error::RecvError::Closed) => None,
            }
        })
        .boxed()
    };
    initial.chain(updates).boxed()
}

/// Opens a generated local Web URL with the platform browser launcher.
pub(crate) async fn open_in_browser(url: &str) -> Result<(), String> {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "linux") {
        ("xdg-open", vec![url])
    } else if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd.exe builtin on Windows. The URL is generated from
        // the local server and a validated SessionId, so no user shell text
        // enters this deliberate platform adapter.
        ("cmd", vec!["/C", "start", "", url])
    } else {
        return Err("automatic browser opening is unsupported on this platform".to_owned());
    };

    tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| format!("browser launcher {program} failed: {error}"))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("browser launcher {program} exited with {status}"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::trajectory_stream;
    use futures_util::StreamExt;
    use merry_core::{SessionId, TrajectoryEvent, TrajectorySnapshot};
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn closed_trajectory_stream_ends_after_the_terminal_snapshot() {
        let session_id = SessionId::new("closed-session").expect("valid session id");
        let mut snapshot = TrajectorySnapshot::new(session_id);
        snapshot.mark_closed();
        let (_sender, receiver) = broadcast::channel(1);
        let mut stream = trajectory_stream(snapshot, receiver, true);

        assert!(matches!(
            stream.next().await,
            Some(Ok(TrajectoryEvent::Snapshot { snapshot })) if snapshot.is_closed()
        ));
        assert!(stream.next().await.is_none());
    }
}
