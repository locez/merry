use crate::{
    memory::{MemoryActivationSource, StoredMemoryActivationSource},
    model_config::RuntimeModelConfigs,
    runtime::{
        Runtime,
        tests::support::{
            common::{model_configs_with_primary, runtime_inner_with, session_id},
            model_provider::RecordingModelProvider,
        },
    },
};
use std::{num::NonZeroUsize, sync::Arc};

pub(in crate::runtime::tests) fn runtime_with_provider_and_memory_source<S>(
    session: &str,
    provider: RecordingModelProvider,
    source: S,
) -> Runtime
where
    S: MemoryActivationSource + 'static,
{
    Runtime {
        inner: Arc::new(runtime_inner_with(
            session_id(session),
            NonZeroUsize::new(16).expect("non-zero buffer"),
            model_configs_with_primary(provider),
            Arc::new(source),
        )),
    }
}

pub(in crate::runtime::tests) fn runtime_with_provider(
    session: &str,
    provider: RecordingModelProvider,
) -> Runtime {
    runtime_with_provider_and_memory_source(session, provider, StoredMemoryActivationSource)
}

pub(in crate::runtime::tests) fn runtime_without_provider_with_memory_source<S>(
    session: &str,
    source: S,
) -> Runtime
where
    S: MemoryActivationSource + 'static,
{
    Runtime {
        inner: Arc::new(runtime_inner_with(
            session_id(session),
            NonZeroUsize::new(16).expect("non-zero buffer"),
            RuntimeModelConfigs::default(),
            Arc::new(source),
        )),
    }
}
