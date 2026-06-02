use merry_core::{MerryErrorDomain, MerryErrorInfo, MerryRetryability};
use merry_runtime::{AgentLoopError, RuntimeError};
use pyo3::{create_exception, exceptions::PyException, prelude::*};

create_exception!(_merry, NativeMerryError, PyException);

const MAX_TOOL_EXCEPTION_DETAIL_CHARS: usize = 240;

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add(
        "NativeMerryError",
        module.py().get_type::<NativeMerryError>(),
    )?;
    Ok(())
}

fn error_info(
    code: &str,
    domain: MerryErrorDomain,
    message: &str,
    retryability: MerryRetryability,
    hint: Option<&str>,
) -> MerryErrorInfo {
    let builder = MerryErrorInfo::builder(code, domain, message, retryability);
    let builder = if let Some(hint) = hint {
        builder.hint(hint)
    } else {
        builder
    };

    builder
        .build()
        .expect("static error metadata must be valid")
}

fn runtime_error_info(code: &str, message: &str, hint: Option<&str>) -> MerryErrorInfo {
    error_info(
        code,
        MerryErrorDomain::Runtime,
        message,
        MerryRetryability::UserActionRequired,
        hint,
    )
}

pub(crate) fn runtime_message_to_py(code: &str, message: &str, hint: Option<&str>) -> PyErr {
    let info = runtime_error_info(code, message, hint);
    merry_info_to_py_err(info)
}

pub(crate) fn config_message_to_py(code: &str, message: &str, hint: Option<&str>) -> PyErr {
    let info = error_info(
        code,
        MerryErrorDomain::Config,
        message,
        MerryRetryability::UserActionRequired,
        hint,
    );
    merry_info_to_py_err(info)
}

pub(crate) fn runtime_error_to_py(error: RuntimeError) -> PyErr {
    runtime_message_to_py(
        "runtime.error",
        &error.to_string(),
        Some("Inspect runtime input and configuration."),
    )
}

pub(crate) fn agent_loop_error_to_py(error: AgentLoopError) -> PyErr {
    if matches!(
        error.runtime_error(),
        RuntimeError::ToolExecutionFailed { .. }
    ) {
        return tool_executor_exception(error.runtime_error().to_string());
    }

    runtime_message_to_py(
        "runtime.agent_loop_error",
        &error.to_string(),
        Some("Inspect runtime input, provider output, and tool configuration."),
    )
}

fn tool_executor_exception(message: String) -> PyErr {
    let detail = sanitize_tool_exception_detail(&message);
    let diagnostic_message = format!("Tool executor raised an unexpected exception: {detail}");
    let info = MerryErrorInfo::builder(
        "tool.executor_exception",
        MerryErrorDomain::Tool,
        &diagnostic_message,
        MerryRetryability::NotRetryable,
    )
    .hint("Handle expected business failures inside the tool result instead of raising.")
    .build()
    .unwrap_or_else(|_error| {
        MerryErrorInfo::builder(
            "tool.executor_exception",
            MerryErrorDomain::Tool,
            "Tool executor raised an unexpected exception.",
            MerryRetryability::NotRetryable,
        )
        .hint("Handle expected business failures inside the tool result instead of raising.")
        .build()
        .expect("static tool executor error metadata must be valid")
    });
    merry_info_to_py_err(info)
}

fn sanitize_tool_exception_detail(message: &str) -> String {
    let mut sanitized = String::with_capacity(message.len().min(MAX_TOOL_EXCEPTION_DETAIL_CHARS));
    let mut last_was_space = false;

    for character in message.chars() {
        let is_space = character.is_whitespace() || character.is_control();
        if is_space {
            if !last_was_space && !sanitized.is_empty() {
                sanitized.push(' ');
                last_was_space = true;
            }
        } else {
            sanitized.push(character);
            last_was_space = false;
        }

        if sanitized.chars().count() >= MAX_TOOL_EXCEPTION_DETAIL_CHARS {
            break;
        }
    }

    let sanitized = sanitized.trim().to_owned();
    if sanitized.is_empty() {
        "unspecified executor failure".to_owned()
    } else {
        sanitized
    }
}

pub(crate) fn merry_info_to_py_err(info: MerryErrorInfo) -> PyErr {
    let payload =
        serde_json::to_string(&info).expect("MerryErrorInfo serialization must be infallible");
    NativeMerryError::new_err(payload)
}
