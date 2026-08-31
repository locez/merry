//! Stable error translation for the Python boundary.

use merry_core::{CoreError, MerryErrorDomain, MerryErrorInfo, MerryRetryability};
use pyo3::{
    create_exception,
    exceptions::{PyException, PyValueError},
    prelude::*,
};

create_exception!(_merry, NativeMerryError, PyException);

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add(
        "NativeMerryError",
        module.py().get_type::<NativeMerryError>(),
    )?;
    Ok(())
}

pub(crate) fn agent_build_error_to_py(error: merry::AgentBuildError) -> PyErr {
    let domain = match &error {
        merry::AgentBuildError::Tool { .. } => MerryErrorDomain::Tool,
        merry::AgentBuildError::Runtime { .. } => MerryErrorDomain::Runtime,
        merry::AgentBuildError::Profile { .. } | merry::AgentBuildError::LoopConfig { .. } => {
            MerryErrorDomain::Config
        }
        merry::AgentBuildError::MissingPrimaryProvider => MerryErrorDomain::Config,
    };
    let message = error.to_string();
    message_to_py(
        error.diagnostic_code(),
        domain,
        MerryRetryability::UserActionRequired,
        message,
        Some("Review the provider, workspace, and tool configuration."),
    )
}

pub(crate) fn agent_error_to_py(error: merry::AgentError) -> PyErr {
    let (domain, retryability, hint) = match &error {
        merry::AgentError::ToolInvocationBatchMismatch { .. }
        | merry::AgentError::ToolInvocationBatchResolved
        | merry::AgentError::ToolInvocationBatchPending
        | merry::AgentError::ToolInvocationBatchNotPending => (
            MerryErrorDomain::Tool,
            MerryRetryability::UserActionRequired,
            "Submit exactly one result for every invocation in the active batch.",
        ),
        merry::AgentError::AgentRunNotFinished
        | merry::AgentError::AgentRunResultConsumed
        | merry::AgentError::AgentRunResultMissing
        | merry::AgentError::AgentRunProtocol { .. } => (
            MerryErrorDomain::Runtime,
            MerryRetryability::UserActionRequired,
            "Follow the run message and terminal-result lifecycle.",
        ),
        merry::AgentError::FinalOutputContract { .. } => (
            MerryErrorDomain::Config,
            MerryRetryability::UserActionRequired,
            "Review the structured output model and its JSON schema.",
        ),
        merry::AgentError::StructuredOutputNotRecorded { .. }
        | merry::AgentError::StructuredOutputDecode { .. } => (
            MerryErrorDomain::Runtime,
            MerryRetryability::NotRetryable,
            "Inspect the terminal run result before retrying structured decoding.",
        ),
        merry::AgentError::Interactive { .. } | merry::AgentError::InteractiveProtocol { .. } => (
            MerryErrorDomain::Runtime,
            MerryRetryability::UserActionRequired,
            "Review the interactive run lifecycle.",
        ),
        merry::AgentError::ToolHandoffRequired => (
            MerryErrorDomain::Tool,
            MerryRetryability::UserActionRequired,
            "Use the host-tool handoff API for bridge tools.",
        ),
        merry::AgentError::Runtime { .. } | merry::AgentError::Loop { .. } => (
            MerryErrorDomain::Runtime,
            MerryRetryability::Unknown,
            "Inspect the returned diagnostic before retrying.",
        ),
    };

    message_to_py(
        error.diagnostic_code(),
        domain,
        retryability,
        error.to_string(),
        Some(hint),
    )
}

pub(crate) fn builder_consumed_to_py() -> PyErr {
    message_to_py(
        "builder_consumed",
        MerryErrorDomain::Config,
        MerryRetryability::UserActionRequired,
        "agent builder has already been consumed".to_owned(),
        Some("Create a new builder after a failed consuming operation."),
    )
}

pub(crate) fn cancelled_operation_to_py() -> PyErr {
    message_to_py(
        "operation_cancelled",
        MerryErrorDomain::Runtime,
        MerryRetryability::Cancelled,
        "the agent run was cancelled before the requested operation completed".to_owned(),
        Some("Inspect the terminal run result after cancellation."),
    )
}

pub(crate) fn run_state_message_to_py(message: impl Into<String>) -> PyErr {
    message_to_py(
        "runtime.run_state",
        MerryErrorDomain::Runtime,
        MerryRetryability::NotRetryable,
        message.into(),
        Some("Do not use the same AgentRun concurrently; await the active operation first."),
    )
}

pub(crate) fn provider_message_to_py(message: impl Into<String>) -> PyErr {
    message_to_py(
        "provider.config",
        MerryErrorDomain::Provider,
        MerryRetryability::UserActionRequired,
        message.into(),
        Some("Review the provider configuration without exposing credentials."),
    )
}

pub(crate) fn profile_message_to_py(message: impl Into<String>) -> PyErr {
    message_to_py(
        "profile.config",
        MerryErrorDomain::Config,
        MerryRetryability::UserActionRequired,
        message.into(),
        Some("Review the workspace profile and its path scopes."),
    )
}

pub(crate) fn profile_build_error_to_py(
    error: merry::profiles::CodingAgentProfileBuildError,
) -> PyErr {
    let message = match error {
        merry::profiles::CodingAgentProfileBuildError::WorkspaceTools(_) => {
            "workspace tool configuration was rejected"
        }
        merry::profiles::CodingAgentProfileBuildError::ProcessTool(_) => {
            "process tool configuration was rejected"
        }
        merry::profiles::CodingAgentProfileBuildError::PermissionTool(_) => {
            "permission tool configuration was rejected"
        }
        merry::profiles::CodingAgentProfileBuildError::Core(_) => {
            "workspace profile protocol configuration was rejected"
        }
        merry::profiles::CodingAgentProfileBuildError::RuntimeProfile(_) => {
            "workspace runtime profile configuration was rejected"
        }
        merry::profiles::CodingAgentProfileBuildError::HashSerialization(_) => {
            "workspace profile identity could not be created"
        }
        merry::profiles::CodingAgentProfileBuildError::Prompt(_) => {
            "workspace profile prompt configuration was rejected"
        }
    };
    profile_message_to_py(message)
}

pub(crate) fn config_message_to_py(message: impl Into<String>) -> PyErr {
    message_to_py(
        "config.invalid",
        MerryErrorDomain::Config,
        MerryRetryability::UserActionRequired,
        message.into(),
        Some("Review the supplied configuration value."),
    )
}

pub(crate) fn protocol_message_to_py(message: impl Into<String>) -> PyErr {
    message_to_py(
        "protocol.invalid",
        MerryErrorDomain::Runtime,
        MerryRetryability::UserActionRequired,
        message.into(),
        Some("Use the documented provider-neutral JSON protocol."),
    )
}

pub(crate) fn serialization_error_to_py(error: serde_json::Error) -> PyErr {
    message_to_py(
        "protocol.serialization",
        MerryErrorDomain::Runtime,
        MerryRetryability::NotRetryable,
        error.to_string(),
        Some("The Rust-owned protocol could not be serialized."),
    )
}

fn message_to_py(
    code: &str,
    domain: MerryErrorDomain,
    retryability: MerryRetryability,
    message: String,
    hint: Option<&str>,
) -> PyErr {
    let message = bounded_message(&message);
    let builder = MerryErrorInfo::builder(code, domain, &message, retryability);
    let builder = match hint {
        Some(hint) => builder.hint(hint),
        None => builder,
    };

    match builder.build() {
        Ok(info) => match serde_json::to_string(&info) {
            Ok(payload) => NativeMerryError::new_err(payload),
            Err(error) => PyValueError::new_err(format!(
                "failed to serialize native Merry diagnostic: {error}"
            )),
        },
        Err(error) => PyValueError::new_err(format!(
            "failed to construct native Merry diagnostic: {error}"
        )),
    }
}

fn bounded_message(message: &str) -> String {
    const MAX_MESSAGE_CHARS: usize = 4096;
    let mut bounded = String::new();
    for character in message.chars() {
        if character.is_control() {
            if !bounded.is_empty() && !bounded.ends_with(' ') {
                bounded.push(' ');
            }
        } else {
            bounded.push(character);
        }
        if bounded.chars().count() >= MAX_MESSAGE_CHARS {
            break;
        }
    }
    let bounded = bounded.trim().to_owned();
    if bounded.is_empty() {
        "native Merry operation failed".to_owned()
    } else {
        bounded
    }
}

#[allow(dead_code)]
pub(crate) fn core_error_to_py(error: CoreError) -> PyErr {
    config_message_to_py(error.to_string())
}
