//! Model provider trait and stream context.

use crate::{ModelCapabilities, ModelError, ModelEvent, ModelRequest};
use futures_core::Stream;
use merry_core::ProviderName;
use std::{future::Future, pin::Pin};
use tokio_util::sync::CancellationToken;

/// Boxed provider future used for object-safe async provider boundaries.
pub type ModelProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed stream of normalized model events.
pub type ModelEventStream =
    Pin<Box<dyn Stream<Item = Result<ModelEvent, ModelError>> + Send + 'static>>;

/// Context passed to model stream creation.
///
/// Dropping the setup future returned by `ModelProvider::stream_model` or the
/// returned `ModelEventStream` must stop provider work and avoid new side
/// effects. Cancelling the token asks producers to stop cooperatively and avoid
/// new side effects. A stream must not emit any event after `ModelEvent::Completed`.
#[derive(Debug, Clone)]
pub struct ModelStreamContext {
    cancellation_token: CancellationToken,
}

impl ModelStreamContext {
    /// Creates a context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    /// Returns the cancellation token.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

impl Default for ModelStreamContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
        }
    }
}

/// Object-safe Merry model provider boundary.
pub trait ModelProvider: Send + Sync {
    /// Stable Merry-owned provider name.
    fn name(&self) -> &ProviderName;

    /// Provider/model capability declaration.
    fn capabilities(&self) -> &ModelCapabilities;

    /// Starts streaming model events for a compiled provider input snapshot.
    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>>;
}
