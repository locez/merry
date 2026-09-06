use merry_core::{InteractiveRunState, RuntimeEvent, SessionId};
use merry_runtime::{ChildRuntimeFactory, ChildRuntimeInput, Runtime};

pub(crate) fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

#[derive(Clone)]
pub(crate) struct NoopChildFactory;

impl ChildRuntimeFactory for NoopChildFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .build()
    }
}

pub(crate) async fn wait_for_interactive_waiting(
    stream: &mut merry_runtime::InteractiveRunEventStream,
) {
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            return;
        }
    }
    panic!("interactive run closed before returning to waiting");
}
