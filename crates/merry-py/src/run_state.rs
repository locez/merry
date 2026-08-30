//! Exclusive ownership state for one Python-visible run.

use std::sync::Mutex;
use tokio::sync::Notify;

pub(crate) struct RunState {
    pub(crate) run: Mutex<Option<merry::binding::OwnedAgentRun>>,
    pub(crate) changed: Notify,
}

impl RunState {
    pub(crate) fn new(run: merry::binding::OwnedAgentRun) -> Self {
        Self {
            run: Mutex::new(Some(run)),
            changed: Notify::new(),
        }
    }
}

pub(crate) fn take_run(state: &RunState) -> Result<merry::binding::OwnedAgentRun, String> {
    state
        .run
        .lock()
        .map_err(|_| "agent run state is poisoned".to_owned())?
        .take()
        .ok_or_else(|| "agent run operation is already in progress".to_owned())
}

pub(crate) async fn take_run_for_cancel(
    state: &RunState,
) -> Result<merry::binding::OwnedAgentRun, String> {
    loop {
        let notified = state.changed.notified();
        if let Some(run) = try_take_run(state)? {
            return Ok(run);
        }
        notified.await;
    }
}

fn try_take_run(state: &RunState) -> Result<Option<merry::binding::OwnedAgentRun>, String> {
    state
        .run
        .lock()
        .map_err(|_| "agent run state is poisoned".to_owned())
        .map(|mut guard| guard.take())
}

pub(crate) fn restore_run(
    state: &RunState,
    run: merry::binding::OwnedAgentRun,
) -> Result<(), String> {
    let mut guard = state
        .run
        .lock()
        .map_err(|_| "agent run state is poisoned".to_owned())?;
    if guard.is_some() {
        return Err("agent run state was restored more than once".to_owned());
    }
    *guard = Some(run);
    state.changed.notify_waiters();
    Ok(())
}
