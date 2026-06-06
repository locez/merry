use futures_util::stream;
use merry_core::ProviderName;
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext,
};
use merry_runtime::{
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
pub(crate) struct FakeProcessRunner {
    calls: Arc<AtomicUsize>,
    observed_argv: Arc<Mutex<Vec<Vec<String>>>>,
    observed_cwd: Arc<Mutex<Vec<Option<String>>>>,
    outputs: Arc<Mutex<Vec<FakeProcessRunnerStep>>>,
}

impl FakeProcessRunner {
    pub(crate) fn succeeding(stdout: impl Into<String>) -> Self {
        Self::scripted([FakeProcessRunnerStep::success(stdout)])
    }

    pub(crate) fn scripted<const N: usize>(steps: [FakeProcessRunnerStep; N]) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            observed_argv: Arc::new(Mutex::new(Vec::new())),
            observed_cwd: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
        }
    }

    pub(crate) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn observed_argv(&self) -> Vec<Vec<String>> {
        self.observed_argv
            .lock()
            .expect("observed argv mutex should not be poisoned")
            .clone()
    }

    pub(crate) fn observed_cwd(&self) -> Vec<Option<String>> {
        self.observed_cwd
            .lock()
            .expect("observed cwd mutex should not be poisoned")
            .clone()
    }
}

#[derive(Clone)]
pub(crate) struct FakeProcessRunnerStep {
    status: ProcessExitStatus,
    stdout: String,
    stderr: String,
}

impl FakeProcessRunnerStep {
    pub(crate) fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: ProcessExitStatus::Exited(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub(crate) fn failure(stderr: impl Into<String>) -> Self {
        Self {
            status: ProcessExitStatus::Exited(1),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

impl ProcessRunner for FakeProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_argv
                .lock()
                .expect("observed argv mutex should not be poisoned")
                .push(intent.argv().to_vec());
            self.observed_cwd
                .lock()
                .expect("observed cwd mutex should not be poisoned")
                .push(intent.cwd().map(str::to_owned));
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }
            let output = self
                .outputs
                .lock()
                .expect("fake process outputs mutex should not be poisoned")
                .pop()
                .unwrap_or_else(|| FakeProcessRunnerStep::success(String::new()));

            ProcessRunnerOutput::new(
                &intent,
                output.status,
                output.stdout,
                false,
                output.stderr,
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
}

pub(crate) struct CompletingProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
}

impl CompletingProvider {
    pub(crate) fn new() -> Self {
        Self {
            name: ProviderName::new("debug-test-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
        }
    }
}

impl ModelProvider for CompletingProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async {
            let response = ModelResponse::new(
                vec![ModelOutput::text("hidden from runtime events")],
                FinishReason::Stop,
                None,
            );
            let events = vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::OutputTextDelta {
                    delta: "hidden".to_owned(),
                }),
                Ok(ModelEvent::Completed { response }),
            ];
            Ok(Box::pin(stream::iter(events)) as ModelEventStream)
        })
    }
}

pub(crate) struct RecordingProvider {
    inner: CompletingProvider,
    pub(crate) requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl RecordingProvider {
    pub(crate) fn new() -> Self {
        Self {
            inner: CompletingProvider::new(),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ModelProvider for RecordingProvider {
    fn name(&self) -> &ProviderName {
        self.inner.name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        self.requests
            .lock()
            .expect("request mutex should not be poisoned")
            .push(request.clone());
        self.inner.stream_model(request, context)
    }
}

pub(crate) type ScriptedStep = Vec<Result<ModelEvent, ModelError>>;

#[derive(Debug, Clone)]
pub(crate) struct ScriptedProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedStep>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ScriptedProvider {
    pub(crate) fn new(scripts: Vec<ScriptedStep>) -> Self {
        Self {
            name: ProviderName::new("debug-scripted-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(scripts.into_iter().rev().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub(crate) fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .expect("request mutex should not be poisoned")
            .clone()
    }
}

impl ModelProvider for ScriptedProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("request mutex should not be poisoned")
                .push(request);

            let script = self
                .steps
                .lock()
                .expect("step mutex should not be poisoned")
                .pop()
                .unwrap_or_default();

            Ok(Box::pin(stream::iter(script)) as ModelEventStream)
        })
    }
}

pub(crate) fn model_name() -> ModelName {
    ModelName::new("debug-model").expect("valid model name")
}
