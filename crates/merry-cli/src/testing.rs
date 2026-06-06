#[cfg(test)]
mod test_support;

#[cfg(test)]
pub(crate) use test_support::{
    CompletingProvider, FakeProcessRunner, FakeProcessRunnerStep, RecordingProvider,
    ScriptedProvider, model_name,
};
