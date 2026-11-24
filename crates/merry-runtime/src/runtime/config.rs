use crate::CitationCompactionPolicy;
use merry_llm::ReasoningEffort;
use std::num::{NonZeroU64, NonZeroUsize};

/// Policy governing how compacted session history is retained in memory and on disk.
///
/// Orthogonal to persistence (`session_store`):
/// - [`Self::Archive`] (the default) retains compacted history cleanly:
///   - When a `session_store` is attached, compaction always rotates turns `<= compacted_through`
///     out of hot memory and live `journal.jsonl` into `<session_dir>/archive/seg_NNNN.jsonl`
///     (plus a unified deterministic Markdown archive `seg_NNNN.md` + `INDEX.md` when enabled),
///     keeping startup and hot RAM bounded. By default there is no segment or byte cap, so full
///     history is preserved; optional `max_segments` / `max_bytes` caps enable rolling eviction
///     and orphan image GC for long-lived bots.
///   - When no `session_store` is attached, nothing is written to disk and turns stay in memory
///     so history is not silently discarded.
/// - [`Self::ContextOnly`] retains only the active post-compaction tail and checkpoint summary:
///   prunes compacted turns from hot memory, rewrites `journal.jsonl` when persisted, deletes
///   unreferenced sidecar images, and writes no `archive/` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRetention {
    /// Rotate compacted segments into `<session_dir>/archive/` when persisted
    /// (unbounded by default; optionally capped via [`ArchiveConfig`]).
    Archive(ArchiveConfig),
    /// Retain only the active post-compaction tail and compacted checkpoint summary.
    ContextOnly,
}

impl Default for HistoryRetention {
    fn default() -> Self {
        Self::Archive(ArchiveConfig::new())
    }
}

impl HistoryRetention {
    /// Retains all compacted history in `<session_dir>/archive/` with no retention cap.
    #[must_use]
    pub const fn archive() -> Self {
        Self::Archive(ArchiveConfig::new())
    }

    /// Retains compacted history in `<session_dir>/archive/` using custom [`ArchiveConfig`].
    #[must_use]
    pub const fn archive_with(config: ArchiveConfig) -> Self {
        Self::Archive(config)
    }

    /// Retains only the active context tail and compaction summary.
    #[must_use]
    pub const fn context_only() -> Self {
        Self::ContextOnly
    }

    /// Whether compacted turns should be pruned from hot memory (and rotated on disk when stored).
    #[must_use]
    pub(crate) const fn prunes_compacted_history(&self, has_session_store: bool) -> bool {
        match self {
            Self::ContextOnly => true,
            Self::Archive(_) => has_session_store,
        }
    }

    /// Returns archive settings when [`Self::Archive`] is active.
    #[must_use]
    pub const fn archive_config(&self) -> Option<ArchiveConfig> {
        match self {
            Self::Archive(config) => Some(*config),
            Self::ContextOnly => None,
        }
    }
}

/// Configuration for [`HistoryRetention::Archive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveConfig {
    max_segments: Option<NonZeroUsize>,
    max_bytes: Option<NonZeroU64>,
    export_markdown: bool,
}

impl ArchiveConfig {
    /// Creates archive settings with deterministic Markdown export enabled and no size cap.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_segments: None,
            max_bytes: None,
            export_markdown: true,
        }
    }

    /// Sets the maximum number of archived segments to retain on disk.
    #[must_use]
    pub const fn with_max_segments(mut self, max_segments: Option<NonZeroUsize>) -> Self {
        self.max_segments = max_segments;
        self
    }

    /// Sets the maximum total byte size across archived segment files.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: Option<NonZeroU64>) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Enables or disables deterministic Markdown archive export (`seg_NNNN.md` + `INDEX.md`).
    #[must_use]
    pub const fn with_export_markdown(mut self, export_markdown: bool) -> Self {
        self.export_markdown = export_markdown;
        self
    }

    /// Maximum number of archived segments retained on disk, if bounded.
    #[must_use]
    pub const fn max_segments(&self) -> Option<NonZeroUsize> {
        self.max_segments
    }

    /// Maximum total bytes retained across archived segment files, if bounded.
    #[must_use]
    pub const fn max_bytes(&self) -> Option<NonZeroU64> {
        self.max_bytes
    }

    /// Whether deterministic Markdown archives (`seg_NNNN.md` + `INDEX.md`) are written.
    #[must_use]
    pub const fn export_markdown(&self) -> bool {
        self.export_markdown
    }
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self::new()
    }
}

fn default_automatic_compaction_policy() -> CitationCompactionPolicy {
    CitationCompactionPolicy::default()
}

/// Runtime-owned policy for checkpoint compaction.
///
/// `enabled` and `policy` drive automatic compaction before a provider request. Manual
/// [`crate::Runtime::compact_context_once`] calls still take an explicit
/// [`CitationCompactionPolicy`] so tests and callers can run one-off compaction
/// passes without mutating runtime construction policy.
///
/// `reasoning_effort` is not path-specific: it applies to every compaction
/// request, automatic or manual, which is why this type is named for compaction
/// rather than for one of its callers.
///
/// Compaction is a summarization turn over the whole covered window, so it never
/// inherits the primary model's reasoning effort: a primary tuned for hard
/// coding turns can spend its entire output budget reasoning about history and
/// never write the checkpoint. `None` leaves the provider default in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionConfig {
    enabled: bool,
    policy: CitationCompactionPolicy,
    reasoning_effort: Option<ReasoningEffort>,
}

impl CompactionConfig {
    /// Enables automatic compaction with the provided policy.
    #[must_use]
    pub fn enabled(policy: CitationCompactionPolicy) -> Self {
        Self {
            enabled: true,
            policy,
            reasoning_effort: None,
        }
    }

    /// Disables automatic compaction.
    ///
    /// The policy remains populated with defaults so disabled configs can be
    /// inspected or re-enabled by callers without constructing a dummy policy.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            policy: default_automatic_compaction_policy(),
            reasoning_effort: None,
        }
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn policy(&self) -> CitationCompactionPolicy {
        self.policy
    }

    /// Returns a copy configured with a reasoning-effort level for compaction.
    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Optional reasoning-effort level for compaction model requests.
    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self::enabled(default_automatic_compaction_policy())
    }
}
