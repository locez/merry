//! Versioned, provider-neutral external tool identities and session catalogs.

use crate::{
    CoreError, ToolAdapterId, ToolBindingName, ToolSourceFingerprint, ToolSourceId, ToolSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An adapter's stable operation identity, without credentials or transport state.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalToolBinding {
    adapter: ToolAdapterId,
    source: ToolSourceId,
    operation: ToolBindingName,
    source_fingerprint: ToolSourceFingerprint,
}

impl ExternalToolBinding {
    /// Binds an operation to a source identity; the fingerprint must not contain secrets.
    #[must_use]
    pub fn new(
        adapter: ToolAdapterId,
        source: ToolSourceId,
        operation: ToolBindingName,
        source_fingerprint: ToolSourceFingerprint,
    ) -> Self {
        Self {
            adapter,
            source,
            operation,
            source_fingerprint,
        }
    }

    /// Returns the adapter namespace responsible for rebinding the operation.
    #[must_use]
    pub fn adapter(&self) -> &ToolAdapterId {
        &self.adapter
    }

    /// Returns the configured, non-secret source identity.
    #[must_use]
    pub fn source(&self) -> &ToolSourceId {
        &self.source
    }

    /// Returns the adapter-native operation name, not a provider wire type.
    #[must_use]
    pub fn operation(&self) -> &ToolBindingName {
        &self.operation
    }

    /// Returns the opaque source fingerprint used to detect retargeting.
    #[must_use]
    pub fn source_fingerprint(&self) -> &ToolSourceFingerprint {
        &self.source_fingerprint
    }
}

/// One frozen provider-visible definition and its separate execution binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionToolCatalogEntry {
    spec: ToolSpec,
    binding: ExternalToolBinding,
}

impl SessionToolCatalogEntry {
    /// Creates an entry from validated specification and binding values.
    #[must_use]
    pub fn new(spec: ToolSpec, binding: ExternalToolBinding) -> Self {
        Self { spec, binding }
    }

    /// Returns the exact provider-visible definition saved by this session.
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Returns the identity an adapter must check before executing this tool.
    #[must_use]
    pub fn binding(&self) -> &ExternalToolBinding {
        &self.binding
    }
}

/// Ordered external tool definitions pinned to a session, including an empty catalog.
///
/// Availability never changes this value. An empty catalog explicitly means that
/// the session has no external tools; persisted sessions must contain a catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "StoredCatalog")]
pub struct SessionToolCatalog {
    format_version: u32,
    entries: Vec<SessionToolCatalogEntry>,
}

impl SessionToolCatalog {
    /// Validates unique provider names and source bindings while retaining input order.
    pub fn new(entries: Vec<SessionToolCatalogEntry>) -> Result<Self, CoreError> {
        let mut names = BTreeSet::new();
        let mut bindings = BTreeSet::new();
        for entry in &entries {
            if !names.insert(entry.spec().name()) || !bindings.insert(entry.binding()) {
                return Err(CoreError::InvalidToolSpec {
                    reason: "external tool catalog contains duplicate names or bindings",
                });
            }
        }
        Ok(Self {
            format_version: 1,
            entries,
        })
    }

    /// Returns entries in their frozen registration order.
    #[must_use]
    pub fn entries(&self) -> &[SessionToolCatalogEntry] {
        &self.entries
    }
}

impl Default for SessionToolCatalog {
    fn default() -> Self {
        Self {
            format_version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCatalog {
    format_version: u32,
    entries: Vec<SessionToolCatalogEntry>,
}

impl TryFrom<StoredCatalog> for SessionToolCatalog {
    type Error = CoreError;

    fn try_from(stored: StoredCatalog) -> Result<Self, Self::Error> {
        if stored.format_version != 1 {
            return Err(CoreError::InvalidToolSpec {
                reason: "unsupported external tool catalog version",
            });
        }

        Self::new(stored.entries)
    }
}

#[cfg(test)]
#[path = "tool_catalog_tests.rs"]
mod tests;
