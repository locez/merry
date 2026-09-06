use crate::test_request;
use futures_executor::block_on;
use futures_util::StreamExt;
use merry_core::ProviderName;
use merry_llm::{
    ModelCapabilities, ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogErrorKind,
    ModelCatalogFuture, ModelCatalogProvider, ModelError, ModelEventStream, ModelName,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelStreamContext,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};
use tokio_util::sync::CancellationToken;

#[test]
fn model_provider_trait_is_object_safe() {
    struct EmptyProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
    }

    impl EmptyProvider {
        fn new() -> Self {
            Self {
                name: ProviderName::new("empty-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, false, false, false, None, None)
                    .expect("valid capabilities"),
            }
        }
    }

    impl ModelProvider for EmptyProvider {
        fn name(&self) -> &ProviderName {
            &self.name
        }

        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn stream_model<'a>(
            &'a self,
            _request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            Box::pin(async {
                let stream: ModelEventStream =
                    Box::pin(futures_util::stream::poll_fn(|_| Poll::Ready(None)));
                Ok(stream)
            })
        }
    }

    let provider: Arc<dyn ModelProvider> = Arc::new(EmptyProvider::new());
    assert_eq!(provider.name().as_str(), "empty-provider");
    assert!(provider.capabilities().supports_streaming());

    let stream = block_on(provider.stream_model(test_request(), ModelStreamContext::default()))
        .expect("empty provider should return a stream");
    let events = block_on(stream.collect::<Vec<_>>());
    assert!(events.is_empty());
}

#[test]
fn model_catalog_sorts_and_deduplicates_model_ids() {
    let catalog = ModelCatalog::new(vec![
        ModelCatalogEntry::new(
            ModelName::new("vendor/zeta").expect("valid model"),
            Some("gateway"),
        )
        .expect("valid catalog entry"),
        ModelCatalogEntry::new(
            ModelName::new("vendor/alpha").expect("valid model"),
            Some("first-owner"),
        )
        .expect("valid catalog entry"),
        ModelCatalogEntry::new(
            ModelName::new("vendor/alpha").expect("valid model"),
            Some("duplicate-owner"),
        )
        .expect("valid catalog entry"),
    ]);

    assert_eq!(catalog.models().len(), 2);
    assert_eq!(catalog.models()[0].id().as_str(), "vendor/alpha");
    assert_eq!(catalog.models()[0].owner(), Some("first-owner"));
    assert_eq!(catalog.models()[1].id().as_str(), "vendor/zeta");
}

#[test]
fn model_catalog_rejects_invalid_owner_metadata() {
    let error = ModelCatalogEntry::new(
        ModelName::new("vendor/model").expect("valid model"),
        Some("owner\nsecret"),
    )
    .expect_err("control characters should be rejected");

    assert_eq!(error.kind(), ModelCatalogErrorKind::Protocol);
    assert!(!error.diagnostic().contains("secret"));
}

#[test]
fn model_catalog_errors_have_bounded_single_line_diagnostics() {
    let error = ModelCatalogError::new(
        ModelCatalogErrorKind::Transport,
        &format!("request failed\n{}", "x".repeat(2_000)),
    );

    assert!(!error.diagnostic().contains('\n'));
    assert!(error.diagnostic().chars().count() <= 512);
}

#[test]
fn model_catalog_provider_trait_is_object_safe_and_observes_pre_cancel() {
    struct CatalogProvider {
        side_effect_started: Arc<AtomicBool>,
    }

    impl ModelCatalogProvider for CatalogProvider {
        fn list_models<'a>(
            &'a self,
            cancellation_token: CancellationToken,
        ) -> ModelCatalogFuture<'a> {
            Box::pin(async move {
                if cancellation_token.is_cancelled() {
                    return Err(ModelCatalogError::cancelled());
                }
                self.side_effect_started.store(true, Ordering::SeqCst);
                Ok(ModelCatalog::default())
            })
        }
    }

    let side_effect_started = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn ModelCatalogProvider> = Arc::new(CatalogProvider {
        side_effect_started: Arc::clone(&side_effect_started),
    });
    let cancellation_token = CancellationToken::new();
    cancellation_token.cancel();

    let error = block_on(provider.list_models(cancellation_token))
        .expect_err("pre-cancelled discovery should stop");

    assert_eq!(error.kind(), ModelCatalogErrorKind::Cancelled);
    assert!(!side_effect_started.load(Ordering::SeqCst));
}
