use crate::AnthropicProvider;
use merry_llm::{
    ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogErrorKind, ModelCatalogFuture,
    ModelCatalogProvider, ModelName,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use tokio_util::sync::CancellationToken;

const MAX_CATALOG_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_CATALOG_MODELS: usize = 10_000;
const MAX_CATALOG_PAGES: usize = 100;
const MAX_CURSOR_CHARS: usize = 256;
const USER_AGENT_VALUE: &str = concat!("merry/", env!("CARGO_PKG_VERSION"));

impl ModelCatalogProvider for AnthropicProvider {
    fn list_models<'a>(&'a self, cancellation_token: CancellationToken) -> ModelCatalogFuture<'a> {
        Box::pin(async move {
            if cancellation_token.is_cancelled() {
                return Err(ModelCatalogError::cancelled());
            }

            let endpoint = models_endpoint(self.config().base_url())?;
            let mut after_id = None;
            let mut seen_cursors = BTreeSet::new();
            let mut models = Vec::new();

            for page_index in 0..MAX_CATALOG_PAGES {
                if cancellation_token.is_cancelled() {
                    return Err(ModelCatalogError::cancelled());
                }
                let mut page_endpoint = endpoint.clone();
                if let Some(cursor) = after_id.as_deref() {
                    page_endpoint
                        .query_pairs_mut()
                        .append_pair("after_id", cursor);
                }
                let request = self
                    .client
                    .get(page_endpoint)
                    .header("x-api-key", self.config().api_key())
                    .header("anthropic-version", self.config().api_version())
                    .header(reqwest::header::ACCEPT, "application/json")
                    .header(reqwest::header::USER_AGENT, USER_AGENT_VALUE);
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
                let page = serde_json::from_slice::<ModelsPage>(&body).map_err(|_| {
                    ModelCatalogError::new(
                        ModelCatalogErrorKind::Protocol,
                        "Anthropic model catalog returned malformed JSON",
                    )
                })?;
                if models.len().saturating_add(page.data.len()) > MAX_CATALOG_MODELS {
                    return Err(ModelCatalogError::new(
                        ModelCatalogErrorKind::Protocol,
                        "Anthropic model catalog exceeded 10000 entries",
                    ));
                }
                for (entry_index, model) in page.data.into_iter().enumerate() {
                    let id = ModelName::new(&model.id).map_err(|_| {
                        ModelCatalogError::new(
                            ModelCatalogErrorKind::Protocol,
                            &format!(
                                "Anthropic model catalog page {page_index} entry {entry_index} has an invalid model ID"
                            ),
                        )
                    })?;
                    models.push(ModelCatalogEntry::new(id, None)?);
                }
                if !page.has_more {
                    return Ok(ModelCatalog::new(models));
                }
                let cursor = validate_cursor(page.last_id.as_deref())?;
                if !seen_cursors.insert(cursor.clone()) {
                    return Err(ModelCatalogError::new(
                        ModelCatalogErrorKind::Protocol,
                        "Anthropic model catalog repeated a pagination cursor",
                    ));
                }
                after_id = Some(cursor);
            }

            Err(ModelCatalogError::new(
                ModelCatalogErrorKind::Protocol,
                "Anthropic model catalog exceeded 100 pages",
            ))
        })
    }
}

#[derive(Debug, Deserialize)]
struct ModelsPage {
    data: Vec<ModelWire>,
    has_more: bool,
    #[serde(default)]
    last_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelWire {
    id: String,
}

fn models_endpoint(base_url: &str) -> Result<reqwest::Url, ModelCatalogError> {
    let mut endpoint = reqwest::Url::parse(base_url).map_err(|_| {
        ModelCatalogError::new(
            ModelCatalogErrorKind::Protocol,
            "Anthropic model catalog base URL is invalid",
        )
    })?;
    endpoint
        .path_segments_mut()
        .map_err(|()| {
            ModelCatalogError::new(
                ModelCatalogErrorKind::Protocol,
                "Anthropic model catalog base URL cannot contain path segments",
            )
        })?
        .pop_if_empty()
        .push("v1")
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

fn validate_cursor(cursor: Option<&str>) -> Result<String, ModelCatalogError> {
    let Some(cursor) = cursor else {
        return Err(invalid_cursor());
    };
    if cursor.trim().is_empty()
        || cursor.trim() != cursor
        || cursor.chars().any(char::is_control)
        || cursor.chars().count() > MAX_CURSOR_CHARS
    {
        return Err(invalid_cursor());
    }
    Ok(cursor.to_owned())
}

fn invalid_cursor() -> ModelCatalogError {
    ModelCatalogError::new(
        ModelCatalogErrorKind::Protocol,
        "Anthropic model catalog has_more response omitted a valid last_id cursor",
    )
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
        &format!("Anthropic model catalog returned HTTP {status}"),
    )
}

fn transport_error() -> ModelCatalogError {
    ModelCatalogError::new(
        ModelCatalogErrorKind::Transport,
        "Anthropic model catalog transport failed",
    )
}

fn catalog_too_large() -> ModelCatalogError {
    ModelCatalogError::new(
        ModelCatalogErrorKind::Protocol,
        "Anthropic model catalog page exceeded 2 MiB",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnthropicProviderConfig;
    use merry_llm::{ModelCatalogErrorKind, ModelCatalogProvider};
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn lists_paginated_anthropic_models_with_required_headers() {
        let pages = [
            Arc::new(
                br#"{"data":[{"id":"claude-zeta","type":"model"},{"id":"claude-alpha","type":"model"}],"has_more":true,"first_id":"claude-zeta","last_id":"claude-alpha"}"#.to_vec(),
            ),
            Arc::new(
                br#"{"data":[{"id":"claude-alpha","type":"model"},{"id":"claude-beta","type":"model"}],"has_more":false,"first_id":"claude-alpha","last_id":"claude-beta"}"#.to_vec(),
            ),
        ];
        let (base_url, requests) = serve_pages(vec![
            ("200 OK", pages[0].clone()),
            ("200 OK", pages[1].clone()),
        ])
        .await;
        let provider = AnthropicProvider::new(
            AnthropicProviderConfig::new("sk-ant-test-secret")
                .expect("valid config")
                .with_base_url(&base_url)
                .expect("valid base url")
                .with_api_version("2025-01-01")
                .expect("valid API version"),
        );

        let catalog = provider
            .list_models(CancellationToken::new())
            .await
            .expect("models should load");
        let requests = requests.await.expect("requests should be captured");
        let first = requests[0].to_ascii_lowercase();

        assert!(requests[0].starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(requests[1].starts_with("GET /v1/models?after_id=claude-alpha HTTP/1.1\r\n"));
        assert!(first.contains("x-api-key: sk-ant-test-secret\r\n"));
        assert!(first.contains("anthropic-version: 2025-01-01\r\n"));
        assert!(first.contains("user-agent: merry/"));
        assert_eq!(
            catalog
                .models()
                .iter()
                .map(|model| model.id().as_str())
                .collect::<Vec<_>>(),
            ["claude-alpha", "claude-beta", "claude-zeta"]
        );
    }

    #[tokio::test]
    async fn rejects_missing_pagination_cursor_and_maps_statuses() {
        let missing_cursor = Arc::new(
            br#"{"data":[{"id":"claude-alpha"}],"has_more":true,"last_id":null}"#.to_vec(),
        );
        let (base_url, _requests) = serve_pages(vec![("200 OK", missing_cursor)]).await;
        let error = provider_for(&base_url)
            .list_models(CancellationToken::new())
            .await
            .expect_err("missing cursor should fail");
        assert_eq!(error.kind(), ModelCatalogErrorKind::Protocol);

        for (status, expected) in [
            ("401 Unauthorized", ModelCatalogErrorKind::Authentication),
            ("429 Too Many Requests", ModelCatalogErrorKind::RateLimited),
            (
                "500 Internal Server Error",
                ModelCatalogErrorKind::Transport,
            ),
        ] {
            let body = Arc::new(b"provider secret body".to_vec());
            let (base_url, _requests) = serve_pages(vec![(status, body)]).await;
            let error = provider_for(&base_url)
                .list_models(CancellationToken::new())
                .await
                .expect_err("status should fail");
            assert_eq!(error.kind(), expected);
            assert!(!error.diagnostic().contains("provider secret body"));
        }
    }

    #[tokio::test]
    async fn rejects_oversized_anthropic_model_page() {
        let body = Arc::new(vec![b'x'; 2 * 1024 * 1024 + 1]);
        let (base_url, _requests) = serve_pages(vec![("200 OK", body)]).await;

        let error = provider_for(&base_url)
            .list_models(CancellationToken::new())
            .await
            .expect_err("oversized page should fail");

        assert_eq!(error.kind(), ModelCatalogErrorKind::Protocol);
    }

    #[tokio::test]
    async fn pre_cancelled_anthropic_catalog_stops_before_network() {
        let token = CancellationToken::new();
        token.cancel();

        let error = provider_for("http://127.0.0.1:1")
            .list_models(token)
            .await
            .expect_err("pre-cancelled discovery should stop");

        assert_eq!(error.kind(), ModelCatalogErrorKind::Cancelled);
    }

    fn provider_for(base_url: &str) -> AnthropicProvider {
        AnthropicProvider::new(
            AnthropicProviderConfig::new("sk-ant-test")
                .expect("valid config")
                .with_base_url(base_url)
                .expect("valid base URL"),
        )
    }

    async fn serve_pages(
        pages: Vec<(&'static str, Arc<Vec<u8>>)>,
    ) -> (String, oneshot::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let (requests_tx, requests_rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in pages {
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
                requests.push(String::from_utf8_lossy(&request).into_owned());
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(headers.as_bytes())
                    .await
                    .expect("fixture headers");
                let _ = socket.write_all(body.as_slice()).await;
            }
            let _ = requests_tx.send(requests);
        });
        (format!("http://{address}"), requests_rx)
    }
}
