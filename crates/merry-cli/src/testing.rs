#[cfg(test)]
mod test_support;

#[cfg(test)]
pub(crate) use test_support::{
    CompletingProvider, FakeProcessRunner, RecordingProvider, ScriptedProvider, model_name,
    process_tool_call, tool_call, workspace_tool_call,
};
