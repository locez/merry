use merry_runtime::{
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct RecordingProcessRunner {
    observed_intents: Arc<Mutex<Vec<ProcessActionIntent>>>,
    stdout_text: String,
}

impl RecordingProcessRunner {
    pub(crate) fn succeeding(stdout_text: &str) -> Self {
        Self {
            observed_intents: Arc::new(Mutex::new(Vec::new())),
            stdout_text: stdout_text.to_owned(),
        }
    }

    pub(crate) fn observed_intents(&self) -> Vec<ProcessActionIntent> {
        self.observed_intents
            .lock()
            .expect("process intents mutex should not be poisoned")
            .clone()
    }
}

impl ProcessRunner for RecordingProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }

            self.observed_intents
                .lock()
                .expect("process intents mutex should not be poisoned")
                .push(intent.clone());

            ProcessRunnerOutput::new(
                &intent,
                ProcessExitStatus::Exited(0),
                self.stdout_text.clone(),
                false,
                "",
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
}
