//! Bounded output accounting at the provider's stream-receive boundary.

use merry_core::OutputTimingQuality;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Observed output bytes and the interval between first and last effective output.
/// Does not retain reasoning text, tool arguments, or a process-local clock in serialized data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelOutputProgress {
    utf8_bytes: u64,
    elapsed_nanos: u64,
    reasoning_observed: bool,
    timing_quality: OutputTimingQuality,
}

impl ModelOutputProgress {
    /// Creates a snapshot; zero elapsed time means throughput is not yet measurable.
    #[must_use]
    pub fn new(utf8_bytes: u64, elapsed: Duration) -> Self {
        Self {
            utf8_bytes,
            elapsed_nanos: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            reasoning_observed: false,
            timing_quality: OutputTimingQuality::ReceiveWindow,
        }
    }

    /// Total observed UTF-8 bytes across text, reasoning, and tool output.
    #[must_use]
    pub const fn utf8_bytes(self) -> u64 {
        self.utf8_bytes
    }

    /// Receive interval, excluding first-output latency and protocol completion tails.
    #[must_use]
    pub const fn elapsed(self) -> Duration {
        Duration::from_nanos(self.elapsed_nanos)
    }

    /// Marks whether raw reasoning, rather than just a summary, was observed.
    #[must_use]
    pub const fn with_reasoning_observed(mut self, observed: bool) -> Self {
        self.reasoning_observed = observed;
        self
    }

    /// Records known receive-window limitations without retaining provider content.
    #[must_use]
    pub const fn with_timing_quality(mut self, quality: OutputTimingQuality) -> Self {
        self.timing_quality = quality;
        self
    }

    /// Whether raw reasoning contributed to this observation.
    #[must_use]
    pub const fn reasoning_observed(self) -> bool {
        self.reasoning_observed
    }

    /// Quality of the client receive interval.
    #[must_use]
    pub const fn timing_quality(self) -> OutputTimingQuality {
        self.timing_quality
    }
}

/// Classification of model-generated stream content; metadata is not output.
#[derive(Debug, Clone, Copy)]
pub enum StreamOutputKind {
    /// Visible assistant text or tool name/argument fragments.
    Content,
    /// Provider-exposed reasoning text.
    Reasoning,
    /// A reasoning summary, used only when raw reasoning is unavailable.
    ReasoningSummary,
}

/// Per-attempt receive-side accounting shared by protocol adapters.
/// A new instance is required for every attempt. Empty deltas do not move either endpoint.
#[derive(Debug, Default)]
pub struct OutputProgressTracker {
    first: Option<Instant>,
    last: Option<Instant>,
    content_bytes: u64,
    reasoning_bytes: u64,
    summary_bytes: u64,
    incomplete_reasoning: bool,
}

impl OutputProgressTracker {
    /// Marks opaque or otherwise unobservable reasoning without counting it as visible text.
    pub fn mark_incomplete_reasoning(&mut self) {
        self.incomplete_reasoning = true;
    }

    /// Accounts for a validated output fragment at the time its network data was received.
    /// Raw reasoning takes precedence over summaries to avoid counting both representations.
    pub fn observe(&mut self, kind: StreamOutputKind, text: &str, received_at: Instant) {
        if text.is_empty() {
            return;
        }
        let bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        match kind {
            StreamOutputKind::Content => {
                self.content_bytes = self.content_bytes.saturating_add(bytes)
            }
            StreamOutputKind::Reasoning => {
                self.reasoning_bytes = self.reasoning_bytes.saturating_add(bytes)
            }
            StreamOutputKind::ReasoningSummary => {
                self.summary_bytes = self.summary_bytes.saturating_add(bytes);
                if self.reasoning_bytes > 0 {
                    return;
                }
            }
        }
        self.first.get_or_insert(received_at);
        self.last = Some(received_at);
    }

    /// Latest observation; None until effective output has arrived.
    #[must_use]
    pub fn snapshot(&self) -> Option<ModelOutputProgress> {
        let elapsed = self.last?.saturating_duration_since(self.first?);
        let reasoning = if self.reasoning_bytes > 0 {
            self.reasoning_bytes
        } else {
            self.summary_bytes
        };
        let quality =
            if self.incomplete_reasoning || (self.reasoning_bytes == 0 && self.summary_bytes > 0) {
                OutputTimingQuality::PartialOutput
            } else {
                OutputTimingQuality::ReceiveWindow
            };
        Some(
            ModelOutputProgress::new(self.content_bytes.saturating_add(reasoning), elapsed)
                .with_reasoning_observed(self.reasoning_bytes > 0)
                .with_timing_quality(quality),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_progress_counts_utf8_independent_of_fragment_boundaries() {
        let now = Instant::now();
        let mut tracker = OutputProgressTracker::default();
        tracker.observe(StreamOutputKind::Content, "", now);
        assert_eq!(tracker.snapshot(), None);
        tracker.observe(
            StreamOutputKind::Reasoning,
            "思",
            now + Duration::from_secs(10),
        );
        tracker.observe(
            StreamOutputKind::Reasoning,
            "考",
            now + Duration::from_secs(10),
        );
        tracker.observe(
            StreamOutputKind::Content,
            "ok",
            now + Duration::from_secs(12),
        );
        tracker.observe(
            StreamOutputKind::Content,
            "",
            now + Duration::from_secs(100),
        );
        assert_eq!(
            tracker.snapshot(),
            Some(ModelOutputProgress::new(8, Duration::from_secs(2)).with_reasoning_observed(true))
        );
        assert_eq!(OutputProgressTracker::default().snapshot(), None);
    }

    #[test]
    fn output_progress_prefers_raw_reasoning_and_ignores_late_summary_timing() {
        let now = Instant::now();
        let mut tracker = OutputProgressTracker::default();
        tracker.observe(StreamOutputKind::ReasoningSummary, "summary", now);
        tracker.observe(
            StreamOutputKind::Reasoning,
            "actual reasoning",
            now + Duration::from_secs(1),
        );
        tracker.observe(
            StreamOutputKind::Content,
            "body",
            now + Duration::from_secs(2),
        );
        tracker.observe(
            StreamOutputKind::ReasoningSummary,
            "duplicate",
            now + Duration::from_secs(30),
        );
        assert_eq!(
            tracker.snapshot(),
            Some(
                ModelOutputProgress::new(20, Duration::from_secs(2)).with_reasoning_observed(true)
            )
        );
    }

    #[test]
    fn output_progress_round_trips_without_serializing_clock_or_text() {
        let event = crate::ModelEvent::OutputProgress {
            progress: Some(ModelOutputProgress::new(7, Duration::from_millis(250))),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type":"output_progress","progress":{"utf8_bytes":7,"elapsed_nanos":250_000_000,"reasoning_observed":false,"timing_quality":"receive_window"}})
        );
        assert_eq!(
            serde_json::from_value::<crate::ModelEvent>(json).unwrap(),
            event
        );
    }
}
