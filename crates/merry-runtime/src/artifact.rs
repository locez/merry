//! Internal artifact registry skeleton.

/// Placeholder registry for durable artifact references.
///
/// M4 does not record or emit artifacts, but the runtime owns the boundary.
#[derive(Debug, Default)]
pub(crate) struct ArtifactRegistry {
    recorded_count: usize,
}

impl ArtifactRegistry {
    pub(crate) fn is_empty(&self) -> bool {
        self.recorded_count == 0
    }
}
