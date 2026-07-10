use crate::OpenAiProvider;
use merry_llm::{
    ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogErrorKind, ModelCatalogFuture,
    ModelCatalogProvider, ModelName,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

const MAX_CATALOG_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_CATALOG_MODELS: usize = 10_000;
const USER_AGENT_HEADER_VALUE: &str = concat!("merry/", env!("CARGO_PKG_VERSION"));

impl ModelCatalogProvider for OpenAiProvider {
    fn list_models<'a>(&'a self, cancellation_token: CancellationToken) -> ModelCatalogFuture<'a> {
        Box::pin(async move {
            if cancellation_token.is_cancelled() {
                return Err(ModelCatalogError::cancelled());
            }

            let endpoint = models_endpoint(self.config().base_url())?;
            let mut request = self
                .client
                .get(endpoint)
                .bearer_auth(self.config().api_key())
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::USER_AGENT, USER_AGENT_HEADER_VALUE);
            if let Some(organization) = self.config().organization() {
                request = request.header("OpenAI-Organization", organization);
            }
            if let Some(project) = self.config().project() {
                request = request.header("OpenAI-Project", project);
            }

            let response = tokio::select! {
                () = cancellation_token.cancelled() => {
                    return Err(ModelCatalogError::cancelled());
                }
                response = request.send() => response.map_err(|_| transport_error())?,
            };
            let status = response.status();
            if !status.is_success() {
                return Err(status_error(status));
            }
            let body = read_bounded_body(response, &cancellation_token).await?;
            let wire = serde_json::from_slice::<ModelsResponse>(&body).map_err(|_| {
                ModelCatalogError::new(
                    ModelCatalogErrorKind::Protocol,
                    "OpenAI-compatible model catalog returned malformed JSON",
                )
            })?;
            if wire.data.len() > MAX_CATALOG_MODELS {
                return Err(ModelCatalogError::new(
                    ModelCatalogErrorKind::Protocol,
                    "OpenAI-compatible model catalog exceeded 10000 entries",
                ));
            }

            let mut models = Vec::with_capacity(wire.data.len());
            for (index, model) in wire.data.into_iter().enumerate() {
                let id = ModelName::new(&model.id).map_err(|_| {
                    ModelCatalogError::new(
                        ModelCatalogErrorKind::Protocol,
                        &format!(
                            "OpenAI-compatible model catalog entry {index} has an invalid model ID"
                        ),
                    )
                })?;
                models.push(ModelCatalogEntry::new(id, model.owned_by.as_deref())?);
            }
            Ok(ModelCatalog::new(models))
        })
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelWire>,
}

#[derive(Debug, Deserialize)]
struct ModelWire {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

fn models_endpoint(base_url: &str) -> Result<reqwest::Url, ModelCatalogError> {
    let mut endpoint = reqwest::Url::parse(base_url).map_err(|_| {
        ModelCatalogError::new(
            ModelCatalogErrorKind::Protocol,
            "OpenAI-compatible model catalog base URL is invalid",
        )
    })?;
    endpoint
        .path_segments_mut()
        .map_err(|()| {
            ModelCatalogError::new(
                ModelCatalogErrorKind::Protocol,
                "OpenAI-compatible model catalog base URL cannot contain path segments",
            )
        })?
        .pop_if_empty()
        .push("models");
    Ok(endpoint)
}

async fn read_bounded_body(
    mut response: reqwest::Response,
    cancellation_token: &CancellationToken,
) -> Result<Vec<u8>, ModelCatalogError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BODY_BYTES as u64)
    {
        return Err(catalog_too_large());
    }
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            () = cancellation_token.cancelled() => {
                return Err(ModelCatalogError::cancelled());
            }
            chunk = response.chunk() => chunk.map_err(|_| transport_error())?,
        };
        let Some(chunk) = chunk else {
            return Ok(body);
        };
        if body.len().saturating_add(chunk.len()) > MAX_CATALOG_BODY_BYTES {
            return Err(catalog_too_large());
        }
        body.extend_from_slice(&chunk);
    }
}

fn status_error(status: reqwest::StatusCode) -> ModelCatalogError {
    let kind = match status.as_u16() {
        404 | 405 | 501 => ModelCatalogErrorKind::Unsupported,
        401 | 403 => ModelCatalogErrorKind::Authentication,
        429 => ModelCatalogErrorKind::RateLimited,
        _ => ModelCatalogErrorKind::Transport,
    };
    ModelCatalogError::new(
        kind,
        &format!("OpenAI-compatible model catalog returned HTTP {status}"),
    )
}

fn transport_error() -> ModelCatalogError {
    ModelCatalogError::new(
        ModelCatalogErrorKind::Transport,
        "OpenAI-compatible model catalog transport failed",
    )
}

fn catalog_too_large() -> ModelCatalogError {
    ModelCatalogError::new(
        ModelCatalogErrorKind::Protocol,
        "OpenAI-compatible model catalog exceeded 2 MiB",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenAiProviderConfig;
    use merry_llm::{ModelCatalogErrorKind, ModelCatalogProvider};
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn lists_models_with_openai_compatible_headers_and_normalization() {
        let body = br#"{"object":"list","data":[{"id":"zeta","owned_by":"gateway"},{"id":"alpha","owned_by":"openai"},{"id":"alpha","owned_by":"duplicate"}]}"#;
        let (base_url, request) = serve_response("200 OK", body).await;
        let provider = OpenAiProvider::new(
            OpenAiProviderConfig::new("sk-test-secret")
                .expect("valid config")
                .with_base_url(&format!("{base_url}/v1"))
                .expect("valid base url")
                .with_organization("org-test")
                .expect("valid organization")
                .with_project("proj-test")
                .expect("valid project"),
        );

        let catalog = provider
            .list_models(CancellationToken::new())
            .await
            .expect("models should load");
        let request = request.await.expect("request should be captured");
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(request_lower.contains("authorization: bearer sk-test-secret\r\n"));
        assert!(request_lower.contains("openai-organization: org-test\r\n"));
        assert!(request_lower.contains("openai-project: proj-test\r\n"));
        assert!(request_lower.contains("user-agent: merry/"));
        assert_eq!(catalog.models().len(), 2);
        assert_eq!(catalog.models()[0].id().as_str(), "alpha");
        assert_eq!(catalog.models()[0].owner(), Some("openai"));
        assert_eq!(catalog.models()[1].id().as_str(), "zeta");
    }

    #[tokio::test]
    async fn maps_model_catalog_http_statuses_without_response_bodies() {
        for (status, expected) in [
            ("404 Not Found", ModelCatalogErrorKind::Unsupported),
            ("401 Unauthorized", ModelCatalogErrorKind::Authentication),
            ("429 Too Many Requests", ModelCatalogErrorKind::RateLimited),
            (
                "500 Internal Server Error",
                ModelCatalogErrorKind::Transport,
            ),
        ] {
            let (base_url, _request) = serve_response(status, b"provider secret body").await;
            let provider = OpenAiProvider::new(
                OpenAiProviderConfig::new("sk-test")
                    .expect("valid config")
                    .with_base_url(&format!("{base_url}/v1"))
                    .expect("valid base url"),
            );

            let error = provider
                .list_models(CancellationToken::new())
                .await
                .expect_err("status should fail");

            assert_eq!(error.kind(), expected);
            assert!(!error.diagnostic().contains("provider secret body"));
        }
    }

    #[tokio::test]
    async fn rejects_malformed_and_oversized_model_catalogs() {
        let (base_url, _request) = serve_response("200 OK", b"not json").await;
        let provider = provider_for(&base_url);
        let malformed = provider
            .list_models(CancellationToken::new())
            .await
            .expect_err("malformed JSON should fail");
        assert_eq!(malformed.kind(), ModelCatalogErrorKind::Protocol);

        let oversized_body = Arc::new(vec![b'x'; 2 * 1024 * 1024 + 1]);
        let (base_url, _request) = serve_response_arc("200 OK", oversized_body).await;
        let oversized = provider_for(&base_url)
            .list_models(CancellationToken::new())
            .await
            .expect_err("oversized response should fail");
        assert_eq!(oversized.kind(), ModelCatalogErrorKind::Protocol);
    }

    #[tokio::test]
    async fn pre_cancelled_model_catalog_does_not_start_http_request() {
        let provider = provider_for("http://127.0.0.1:1");
        let token = CancellationToken::new();
        token.cancel();

        let error = provider
            .list_models(token)
            .await
            .expect_err("pre-cancelled discovery should stop");

        assert_eq!(error.kind(), ModelCatalogErrorKind::Cancelled);
    }

    fn provider_for(base_url: &str) -> OpenAiProvider {
        OpenAiProvider::new(
            OpenAiProviderConfig::new("sk-test")
                .expect("valid config")
                .with_base_url(&format!("{base_url}/v1"))
                .expect("valid base url"),
        )
    }

    async fn serve_response(
        status: &'static str,
        body: &'static [u8],
    ) -> (String, oneshot::Receiver<String>) {
        serve_response_arc(status, Arc::new(body.to_vec())).await
    }

    async fn serve_response_arc(
        status: &'static str,
        body: Arc<Vec<u8>>,
    ) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let (request_tx, request_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("fixture accept");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.expect("fixture read");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .await
                .expect("fixture headers");
            let _ = socket.write_all(body.as_slice()).await;
        });
        (format!("http://{address}"), request_rx)
    }
}
