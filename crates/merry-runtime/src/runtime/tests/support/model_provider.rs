    #[derive(Debug)]
    enum ScriptedModelProviderResponse {
        SetupError(ModelError),
        PendingSetup(oneshot::Sender<()>),
        PendingSetupWithDrop {
            started: oneshot::Sender<()>,
            dropped: oneshot::Sender<()>,
        },
        Stream(Vec<Result<ModelEvent, ModelError>>),
    }
    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl NotifyOnDrop {
        fn new(sender: oneshot::Sender<()>) -> Self {
            Self(Some(sender))
        }
    }

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingModelProvider {
        requests: Arc<StdMutex<Vec<ModelRequest>>>,
        contexts: Arc<StdMutex<Vec<ModelStreamContext>>>,
        calls: Arc<AtomicUsize>,
        responses: Arc<StdMutex<Vec<ScriptedModelProviderResponse>>>,
    }

    impl RecordingModelProvider {
        fn new() -> Self {
            Self::with_script(Vec::new())
        }

        fn with_script(responses: Vec<ScriptedModelProviderResponse>) -> Self {
            Self {
                requests: Arc::new(StdMutex::new(Vec::new())),
                contexts: Arc::new(StdMutex::new(Vec::new())),
                calls: Arc::new(AtomicUsize::new(0)),
                responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
            }
        }

        fn recorded_requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .expect("recorded requests mutex should not be poisoned")
                .clone()
        }

        fn recorded_contexts(&self) -> Vec<ModelStreamContext> {
            self.contexts
                .lock()
                .expect("recorded contexts mutex should not be poisoned")
                .clone()
        }

        fn next_response(&self) -> ScriptedModelProviderResponse {
            self.responses
                .lock()
                .expect("model response mutex should not be poisoned")
                .pop()
                .unwrap_or_else(|| {
                    ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())])
                })
        }
    }

    impl ModelProvider for RecordingModelProvider {
        fn name(&self) -> &merry_core::ProviderName {
            static PROVIDER_NAME: std::sync::OnceLock<merry_core::ProviderName> =
                std::sync::OnceLock::new();
            PROVIDER_NAME.get_or_init(|| {
                merry_core::ProviderName::new("runtime-test-provider").expect("valid provider name")
            })
        }

        fn capabilities(&self) -> &ModelCapabilities {
            static CAPABILITIES: std::sync::OnceLock<ModelCapabilities> =
                std::sync::OnceLock::new();
            CAPABILITIES.get_or_init(|| {
                ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities")
            })
        }

        fn stream_model<'a>(
            &'a self,
            request: ModelRequest,
            context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            Box::pin(async move {
                if context.cancellation_token().is_cancelled() {
                    return Err(ModelError::Cancelled);
                }

                self.calls.fetch_add(1, Ordering::SeqCst);
                self.requests
                    .lock()
                    .expect("recorded requests mutex should not be poisoned")
                    .push(request);
                self.contexts
                    .lock()
                    .expect("recorded contexts mutex should not be poisoned")
                    .push(context.clone());
                match self.next_response() {
                    ScriptedModelProviderResponse::SetupError(error) => Err(error),
                    ScriptedModelProviderResponse::PendingSetup(started) => {
                        let _ = started.send(());
                        std::future::pending::<Result<ModelEventStream, ModelError>>().await
                    }
                    ScriptedModelProviderResponse::PendingSetupWithDrop { started, dropped } => {
                        let _notify_on_drop = NotifyOnDrop::new(dropped);
                        let _ = started.send(());
                        std::future::pending::<Result<ModelEventStream, ModelError>>().await
                    }
                    ScriptedModelProviderResponse::Stream(events) => {
                        let stream: ModelEventStream = Box::pin(futures_util::stream::iter(events));
                        Ok(stream)
                    }
                }
            })
        }
    }
