use super::RuntimeInner;
use crate::{
    memory::{MemoryActivationSeed, MemoryActivationSourceKind, MemoryScope},
    step::StepInput,
};
use std::sync::{Arc, atomic::Ordering};
use tokio_util::sync::CancellationToken;

pub(super) async fn clear_current_activated_memories(inner: &RuntimeInner) {
    let mut session = inner.session.lock().await;
    session.replace_activated_memories(Vec::new());
    inner.memory_projection_epoch.fetch_add(1, Ordering::AcqRel);
}

/// Clears pre-commit memory activation if the producer is aborted before the
/// provider has returned an event stream.
pub(super) struct ActivationProjectionGuard {
    inner: Arc<RuntimeInner>,
    token: CancellationToken,
    epoch: u64,
    armed: bool,
}

impl ActivationProjectionGuard {
    pub(super) fn new(inner: Arc<RuntimeInner>, token: CancellationToken, epoch: u64) -> Self {
        Self {
            inner,
            token,
            epoch,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActivationProjectionGuard {
    fn drop(&mut self) {
        if !self.armed || !self.token.is_cancelled() {
            return;
        }

        if self.inner.memory_projection_epoch.load(Ordering::Acquire) != self.epoch {
            return;
        }

        if clear_activated_memories_if_epoch_matches(&self.inner, self.epoch) {
            return;
        }

        let inner = Arc::clone(&self.inner);
        let epoch = self.epoch;
        tokio::spawn(async move {
            if inner.memory_projection_epoch.load(Ordering::Acquire) != epoch {
                return;
            }

            let mut session = inner.session.lock().await;
            if inner
                .memory_projection_epoch
                .compare_exchange(epoch, epoch + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                session.replace_activated_memories(Vec::new());
            }
        });
    }
}

fn clear_activated_memories_if_epoch_matches(inner: &RuntimeInner, epoch: u64) -> bool {
    let Ok(mut session) = inner.session.try_lock() else {
        return false;
    };

    if inner
        .memory_projection_epoch
        .compare_exchange(epoch, epoch + 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        session.replace_activated_memories(Vec::new());
    }

    true
}

pub(super) fn memory_activation_seed_from_step_input(
    input: &StepInput,
) -> Result<MemoryActivationSeed, crate::memory::MemoryError> {
    MemoryActivationSeed::new(
        input.text(),
        vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
        MemoryActivationSourceKind::UserQuery,
        "step input",
    )
}
