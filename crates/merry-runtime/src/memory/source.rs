use super::{
    ActivatedMemory, MemoryActivationSeed, MemoryActivator, MemoryError, MemoryId, MemoryItem,
};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    future::Future,
    pin::Pin,
};
use tokio_util::sync::CancellationToken;

/// Deterministic in-memory candidate store owned by a session.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryStore {
    candidates: BTreeMap<MemoryId, MemoryItem>,
}

impl MemoryStore {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record(&mut self, item: MemoryItem) -> Result<(), MemoryError> {
        let id = item.id().clone();
        match self.candidates.entry(id.clone()) {
            Entry::Occupied(_) => Err(MemoryError::DuplicateMemoryId { id }),
            Entry::Vacant(entry) => {
                entry.insert(item);
                Ok(())
            }
        }
    }

    #[must_use]
    pub(crate) fn candidate_snapshot(&self) -> Vec<MemoryItem> {
        self.candidates.values().cloned().collect()
    }
}

/// Result returned by a crate-internal memory activation source.
pub(crate) type MemoryActivationResult = Result<Vec<ActivatedMemory>, MemoryError>;

/// Boxed memory activation future used for object-safe async boundaries.
pub(crate) type MemoryActivationFuture<'a> =
    Pin<Box<dyn Future<Output = MemoryActivationResult> + Send + 'a>>;

/// Context passed to a memory activation source.
#[derive(Debug, Clone)]
pub(crate) struct MemoryActivationContext {
    cancellation_token: CancellationToken,
}

impl MemoryActivationContext {
    #[must_use]
    pub(crate) fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    #[must_use]
    pub(crate) fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

/// Crate-internal source for the current provider request's memory projection.
pub(crate) trait MemoryActivationSource: Send + Sync {
    fn activate<'a>(
        &'a self,
        seed: MemoryActivationSeed,
        candidates: Vec<MemoryItem>,
        context: MemoryActivationContext,
    ) -> MemoryActivationFuture<'a>;
}

/// Production MVP source backed by the session-owned in-memory store.
#[derive(Debug, Default)]
pub(crate) struct StoredMemoryActivationSource;

impl MemoryActivationSource for StoredMemoryActivationSource {
    fn activate<'a>(
        &'a self,
        seed: MemoryActivationSeed,
        candidates: Vec<MemoryItem>,
        _context: MemoryActivationContext,
    ) -> MemoryActivationFuture<'a> {
        Box::pin(async move { MemoryActivator::activate(&seed, &candidates) })
    }
}
