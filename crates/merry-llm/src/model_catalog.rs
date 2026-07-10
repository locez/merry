//! Provider-neutral model discovery contracts.

use crate::ModelName;
use std::{future::Future, pin::Pin};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_OWNER_CHARS: usize = 256;
const MAX_DIAGNOSTIC_CHARS: usize = 512;

/// One model advertised by a provider catalog endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalogEntry {
    id: ModelName,
    owner: Option<String>,
}

impl ModelCatalogEntry {
    /// Creates a validated model catalog entry.
    pub fn new(id: ModelName, owner: Option<&str>) -> Result<Self, ModelCatalogError> {
        let owner = owner.map(validate_owner).transpose()?;
        Ok(Self { id, owner })
    }

    /// Returns the provider model identifier.
    #[must_use]
    pub fn id(&self) -> &ModelName {
        &self.id
    }

    /// Returns optional provider-supplied ownership metadata.
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
}

/// A normalized, sorted provider model catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCatalog {
    models: Vec<ModelCatalogEntry>,
}

impl ModelCatalog {
    /// Creates a catalog sorted by model ID with duplicate IDs removed.
    #[must_use]
    pub fn new(mut models: Vec<ModelCatalogEntry>) -> Self {
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models.dedup_by(|left, right| left.id == right.id);
        Self { models }
    }

    /// Borrows the normalized model entries.
    #[must_use]
    pub fn models(&self) -> &[ModelCatalogEntry] {
        &self.models
    }

    /// Consumes the catalog and returns its normalized model entries.
    #[must_use]
    pub fn into_models(self) -> Vec<ModelCatalogEntry> {
        self.models
    }
}

/// Provider-neutral model discovery failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCatalogErrorKind {
    /// The endpoint does not expose model discovery.
    Unsupported,
    /// Provider authentication or authorization failed.
    Authentication,
    /// The provider rate-limited model discovery.
    RateLimited,
    /// The request failed at the transport layer.
    Transport,
    /// The provider returned an invalid or unsupported response.
    Protocol,
    /// Discovery was cancelled cooperatively.
    Cancelled,
}

/// A bounded provider-neutral model discovery error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("model catalog error ({kind:?}): {diagnostic}")]
pub struct ModelCatalogError {
    kind: ModelCatalogErrorKind,
    diagnostic: String,
}

impl ModelCatalogError {
    /// Creates an error with a bounded, single-line diagnostic.
    #[must_use]
    pub fn new(kind: ModelCatalogErrorKind, diagnostic: &str) -> Self {
        Self {
            kind,
            diagnostic: sanitize_diagnostic(diagnostic),
        }
    }

    /// Creates a cooperative cancellation error.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(
            ModelCatalogErrorKind::Cancelled,
            "model discovery cancelled",
        )
    }

    /// Returns the provider-neutral failure category.
    #[must_use]
    pub fn kind(&self) -> ModelCatalogErrorKind {
        self.kind
    }

    /// Borrows the bounded provider-neutral diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

/// Boxed future used by the object-safe model catalog boundary.
pub type ModelCatalogFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelCatalog, ModelCatalogError>> + Send + 'a>>;

/// Object-safe provider model discovery boundary.
pub trait ModelCatalogProvider: Send + Sync {
    /// Lists models, stopping before new side effects when cancellation is observed.
    fn list_models<'a>(&'a self, cancellation_token: CancellationToken) -> ModelCatalogFuture<'a>;
}

fn validate_owner(owner: &str) -> Result<String, ModelCatalogError> {
    if owner.trim().is_empty() {
        return Err(ModelCatalogError::new(
            ModelCatalogErrorKind::Protocol,
            "model owner metadata must not be blank",
        ));
    }
    if owner.chars().any(char::is_control) {
        return Err(ModelCatalogError::new(
            ModelCatalogErrorKind::Protocol,
            "model owner metadata contains control characters",
        ));
    }
    if owner.chars().count() > MAX_OWNER_CHARS {
        return Err(ModelCatalogError::new(
            ModelCatalogErrorKind::Protocol,
            "model owner metadata exceeds 256 characters",
        ));
    }
    Ok(owner.to_owned())
}

fn sanitize_diagnostic(diagnostic: &str) -> String {
    let mut sanitized = String::with_capacity(diagnostic.len().min(MAX_DIAGNOSTIC_CHARS));
    let mut whitespace_pending = false;
    for character in diagnostic.chars() {
        if sanitized.chars().count() >= MAX_DIAGNOSTIC_CHARS {
            break;
        }
        if character.is_control() || character.is_whitespace() {
            whitespace_pending = !sanitized.is_empty();
            continue;
        }
        if whitespace_pending {
            sanitized.push(' ');
            whitespace_pending = false;
        }
        sanitized.push(character);
    }
    if sanitized.is_empty() {
        "model catalog operation failed".to_owned()
    } else {
        sanitized
    }
}
