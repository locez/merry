//! Shared stopping rules for manual and automatic context reduction.

use crate::CompactionError;

const MAX_COMPACTION_PASSES: usize = 12;

/// Tracks measured progress without treating the preferred tail as a hard limit.
pub(in crate::runtime) struct CompactionProgress {
    previous_body_tokens: u64,
    passes: usize,
}

impl CompactionProgress {
    pub(in crate::runtime) fn new(initial_body_tokens: u64) -> Self {
        Self {
            previous_body_tokens: initial_body_tokens,
            passes: 0,
        }
    }

    /// Returns whether reduction is complete, rejecting a stalled unsafe request.
    /// A safe indivisible tail may exceed the preferred target, never the watermark.
    pub(in crate::runtime) fn observe(
        &mut self,
        body_tokens: u64,
        target_tokens: u64,
        hard_limit_tokens: u64,
        installed_checkpoint: bool,
    ) -> Result<bool, CompactionError> {
        self.passes += 1;
        let shrank = body_tokens < self.previous_body_tokens;
        self.previous_body_tokens = body_tokens;
        if body_tokens < target_tokens.min(hard_limit_tokens) {
            return Ok(true);
        }
        if !installed_checkpoint || !shrank || self.passes >= MAX_COMPACTION_PASSES {
            self.finish(body_tokens, hard_limit_tokens)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Validates the hard limit when no further checkpoint can be installed.
    pub(in crate::runtime) fn finish(
        &self,
        body_tokens: u64,
        hard_limit_tokens: u64,
    ) -> Result<(), CompactionError> {
        if body_tokens >= hard_limit_tokens {
            return Err(CompactionError::ConvergenceExhausted {
                passes: self.passes,
                estimated_tokens: body_tokens,
                hard_limit_tokens,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_the_trigger_does_not_end_progress_toward_the_target() {
        let mut progress = CompactionProgress::new(100_000);
        assert!(
            !progress
                .observe(40_000, 10_000, 50_000, true)
                .expect("progress")
        );
        assert!(
            progress
                .observe(9_000, 10_000, 50_000, true)
                .expect("target reached")
        );
    }

    #[test]
    fn indivisible_tail_is_accepted_only_below_the_hard_watermark() {
        for body_tokens in [10_000, 49_999, 50_000, 60_000] {
            let mut progress = CompactionProgress::new(body_tokens);
            let result = progress.observe(body_tokens, 10_000, 50_000, false);
            assert_eq!(result.is_ok(), body_tokens < 50_000);
        }
    }

    #[test]
    fn stalled_or_growing_replacements_do_not_repeat_forever() {
        for body_tokens in [60_000, 70_000] {
            let mut progress = CompactionProgress::new(60_000);
            assert!(matches!(
                progress.observe(body_tokens, 10_000, 50_000, true),
                Err(CompactionError::ConvergenceExhausted { passes: 1, .. })
            ));
        }
    }

    #[test]
    fn reduction_passes_are_bounded_even_when_each_one_makes_progress() {
        let mut progress = CompactionProgress::new(100_000);
        for pass in 1..MAX_COMPACTION_PASSES {
            assert!(
                !progress
                    .observe(100_000 - pass as u64, 10_000, 50_000, true)
                    .expect("progress")
            );
        }
        assert!(matches!(
            progress.observe(90_000, 10_000, 50_000, true),
            Err(CompactionError::ConvergenceExhausted {
                passes: MAX_COMPACTION_PASSES,
                ..
            })
        ));
    }
}
