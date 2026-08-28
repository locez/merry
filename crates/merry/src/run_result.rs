//! High-level result projection for one Merry agent run.

use crate::errors::AgentError;
use merry_core::{RuntimeEvent, SessionUsage};
use merry_runtime::{AgentLoopStatus, FinalOutput, Runtime};

/// Completed or policy-stopped high-level run result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    status: AgentLoopStatus,
    events: Vec<RuntimeEvent>,
    model_turns_run: usize,
    final_output: Option<String>,
    final_output_json: Option<FinalOutput>,
    session_usage: Option<SessionUsage>,
}

impl RunResult {
    pub(crate) async fn from_runtime(
        runtime: &Runtime,
        result: merry_runtime::AgentLoopResult,
    ) -> Result<Self, AgentError> {
        let events = runtime
            .project_journal_events(result.events())
            .await
            .map_err(AgentError::from)?;
        Ok(Self {
            status: result.status().clone(),
            events,
            model_turns_run: result.model_turns_run(),
            final_output: result.final_output().map(str::to_owned),
            final_output_json: result.final_output_json().cloned(),
            session_usage: result.session_usage().cloned(),
        })
    }

    /// Returns the terminal or policy-blocked run status.
    #[must_use]
    pub fn status(&self) -> &AgentLoopStatus {
        &self.status
    }

    /// Returns SDK-facing events in durable emission order.
    #[must_use]
    pub fn events(&self) -> &[RuntimeEvent] {
        &self.events
    }

    /// Returns the number of model turns started by the run.
    #[must_use]
    pub fn model_turns_run(&self) -> usize {
        self.model_turns_run
    }

    /// Returns final assistant text when the run produced terminal text.
    #[must_use]
    pub fn final_output(&self) -> Option<&str> {
        self.final_output.as_deref()
    }

    /// Returns the runtime-recorded structured final-output record.
    #[must_use]
    pub fn final_output_json(&self) -> Option<&FinalOutput> {
        self.final_output_json.as_ref()
    }

    /// Returns the latest authoritative session usage snapshot.
    #[must_use]
    pub fn session_usage(&self) -> Option<&SessionUsage> {
        self.session_usage.as_ref()
    }

    /// Consumes the result and returns its public events.
    #[must_use]
    pub fn into_events(self) -> Vec<RuntimeEvent> {
        self.events
    }
}
