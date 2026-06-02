# Merry Py SDK MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the first `merry-py` vertical slice so Python can construct an embedded Merry runtime, run a fake-provider agent loop, consume structured events/results through an async Python API, and receive stable `MerryErrorInfo` exceptions.

**Architecture:** Add a PyO3 extension crate at `crates/merry-py` and pure Python wrappers under `sdks/python/merry`. Rust remains the runtime owner: Python calls a narrow binding facade, the facade builds `merry-runtime` with provider-neutral fake/scripted providers for deterministic SDK tests, and Python wrapper code exposes ergonomic `Runtime`, `RunResult`, `RuntimeEvent`, and `MerryError` classes. The first slice deliberately does not wire live OpenAI config or full async Python tool callbacks; it proves embedding, event/result shape, and error boundaries without moving provider wire formats or Python behavior into runtime crates.

**Tech Stack:** Rust 2024, PyO3 native extension module, maturin mixed Rust/Python layout, Tokio current-thread runtime inside the binding facade, pytest for Python API tests, existing `merry-core`, `merry-llm`, and `merry-runtime` fake-provider/test utilities. PyO3 and maturin layout choices should follow the current PyO3/maturin guidance: a `cdylib` native module, `#[pymodule]` name matching the extension module, and SDK-local `tool.maturin.python-source = "."` with `module-name = "merry._merry"`.

---

## Scope

This plan implements the first Python SDK MVP slice from `specs/2026-05-31-python-first-runtime-sdk-contract.md`.

Included:

- Add `crates/merry-py` to the Cargo workspace.
- Add Python SDK package layout under `sdks/python/merry`.
- Add `sdks/python/pyproject.toml` for maturin builds.
- Add SDK-facing `MerryErrorInfo` in `merry-core` with stable domain and retryability fields.
- Add Python `MerryError` classes that expose `.info`, `.code`, `.domain`, and `.retryability`.
- Expose `Runtime.with_fake_response(final_text)` and `await runtime.run(task)`.
- Expose `async for event in runtime.run_stream(task)` using the same event payloads as `run()`.
- Add deterministic Python tests that do not require live provider credentials or network.
- Add one binding-level tool-domain-failure versus tool-executor-exception test using scripted fake provider calls.

Not included:

- Live OpenAI-compatible provider construction from Python.
- XDG config loading from Python.
- Real workspace tools from Python.
- True incremental event streaming while the Rust step is still running.
- Async Python tool callbacks.
- Pydantic integration or decorator-based tool registration.
- Publishing wheels or CI release automation.

## File Structure

- Modify `Cargo.toml`: add `crates/merry-py` to workspace members and default members, add workspace dependency entries needed by `merry-py`.
- Create `sdks/python/pyproject.toml`: maturin build metadata for the Python SDK mixed Python/Rust package.
- Create `crates/merry-py/Cargo.toml`: PyO3 extension crate manifest with `cdylib` and `rlib`.
- Create `crates/merry-py/src/lib.rs`: `#[pymodule]` entrypoint and exported Python classes/functions.
- Create `crates/merry-py/src/error.rs`: Rust-to-Python `MerryErrorInfo` and exception mapping.
- Create `crates/merry-py/src/runtime.rs`: Python-facing runtime facade using `merry-runtime`.
- Create `crates/merry-py/src/serde_py.rs`: small serde-json-to-Python conversion helpers.
- Create `crates/merry-py/tests/bindings.rs`: Rust-side PyO3/binding unit tests that do not import the built wheel.
- Modify `crates/merry-core/src/event.rs`: add SDK-facing `MerryErrorInfo`, `MerryErrorDomain`, and `MerryRetryability`.
- Modify `crates/merry-core/src/lib.rs`: export new error info types.
- Modify `crates/merry-core/tests/protocol.rs`: serialization and validation tests for `MerryErrorInfo`.
- Create `sdks/python/merry/__init__.py`: public Python wrapper API.
- Create `sdks/python/merry/_errors.py`: Python exception classes and `MerryErrorInfo` dataclass.
- Create `sdks/python/merry/_runtime.py`: Python async runtime wrappers around `_merry`.
- Create `sdks/python/merry/py.typed`: typing marker.
- Create `sdks/python/tests/test_runtime.py`: pytest coverage for the public Python API.
- Create `sdks/python/tests/test_errors.py`: pytest coverage for Python exception mapping.

## Task 1: Add Core MerryErrorInfo

**Files:**

- Modify: `crates/merry-core/src/event.rs`
- Modify: `crates/merry-core/src/lib.rs`
- Modify: `crates/merry-core/tests/protocol.rs`

- [x] **Step 1: Write failing serialization tests**

Add these tests to `crates/merry-core/tests/protocol.rs`:

```rust
use merry_core::{MerryErrorDomain, MerryErrorInfo, MerryRetryability};
use serde_json::json;

#[test]
fn merry_error_info_serializes_stable_sdk_shape() {
    let diagnostic = MerryErrorInfo::builder(
        "tool.executor_exception",
        MerryErrorDomain::Tool,
        "Tool `lookup_order` raised an unexpected exception.",
        MerryRetryability::NotRetryable,
    )
    .hint("Handle expected business failures inside the tool.")
    .context("tool_name", "lookup_order")
    .context("call_id", "call_123")
    .build()
    .expect("valid SDK error info");

    assert_eq!(
        serde_json::to_value(&diagnostic).expect("serializes"),
        json!({
            "code": "tool.executor_exception",
            "domain": "tool",
            "message": "Tool `lookup_order` raised an unexpected exception.",
            "hint": "Handle expected business failures inside the tool.",
            "retryability": "not_retryable",
            "context": {
                "call_id": "call_123",
                "tool_name": "lookup_order"
            }
        })
    );
}

#[test]
fn merry_error_info_rejects_unbounded_or_sensitive_context_keys() {
    let error = MerryErrorInfo::builder(
        "provider.stream_failed",
        MerryErrorDomain::Provider,
        "Provider stream failed.",
        MerryRetryability::Retryable,
    )
    .context("authorization", "Bearer secret")
    .build()
    .expect_err("authorization context must be rejected");

    assert!(error.to_string().contains("context key is not allowed"));
}
```

- [x] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p merry-core merry_error_info --test protocol
```

Expected: FAIL because `MerryErrorInfo`, `MerryErrorDomain`, and `MerryRetryability` do not exist.

- [x] **Step 3: Implement minimal core type**

In `crates/merry-core/src/event.rs`, add the SDK-facing type near the existing `ErrorInfo`:

```rust
use std::collections::BTreeMap;

const MAX_ERROR_HINT_LEN: usize = 512;
const MAX_ERROR_CONTEXT_VALUE_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MerryErrorDomain {
    Config,
    Provider,
    Runtime,
    Tool,
    Policy,
    Context,
    Compaction,
    Artifact,
    Sandbox,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MerryRetryability {
    Retryable,
    NotRetryable,
    UserActionRequired,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MerryErrorInfo {
    code: String,
    domain: MerryErrorDomain,
    message: String,
    hint: Option<String>,
    retryability: MerryRetryability,
    context: BTreeMap<String, String>,
}

impl MerryErrorInfo {
    pub fn builder(
        code: &str,
        domain: MerryErrorDomain,
        message: &str,
        retryability: MerryRetryability,
    ) -> MerryErrorInfoBuilder {
        MerryErrorInfoBuilder {
            code: code.to_owned(),
            domain,
            message: message.to_owned(),
            hint: None,
            retryability,
            context: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn domain(&self) -> MerryErrorDomain {
        self.domain
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    #[must_use]
    pub fn retryability(&self) -> MerryRetryability {
        self.retryability
    }

    #[must_use]
    pub fn context(&self) -> &BTreeMap<String, String> {
        &self.context
    }
}

pub struct MerryErrorInfoBuilder {
    code: String,
    domain: MerryErrorDomain,
    message: String,
    hint: Option<String>,
    retryability: MerryRetryability,
    context: BTreeMap<String, String>,
}

impl MerryErrorInfoBuilder {
    #[must_use]
    pub fn hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_owned());
        self
    }

    #[must_use]
    pub fn context(mut self, key: &str, value: &str) -> Self {
        self.context.insert(key.to_owned(), value.to_owned());
        self
    }

    pub fn build(self) -> Result<MerryErrorInfo, CoreError> {
        validate_diagnostic_code(&self.code)?;
        validate_diagnostic_message(&self.message)?;
        if let Some(hint) = self.hint.as_deref() {
            validate_error_hint(hint)?;
        }
        for (key, value) in &self.context {
            validate_error_context(key, value)?;
        }
        Ok(MerryErrorInfo {
            code: self.code,
            domain: self.domain,
            message: self.message,
            hint: self.hint,
            retryability: self.retryability,
            context: self.context,
        })
    }
}
```

Add validators in the same file:

```rust
fn validate_error_hint(hint: &str) -> Result<(), CoreError> {
    if hint.trim().is_empty() {
        return Err(invalid_diagnostic("MerryErrorInfo hint", hint, "must not be blank"));
    }
    if hint.chars().count() > MAX_ERROR_HINT_LEN {
        return Err(invalid_diagnostic(
            "MerryErrorInfo hint",
            hint,
            "is longer than the allowed maximum length",
        ));
    }
    if hint.chars().any(char::is_control) {
        return Err(invalid_diagnostic(
            "MerryErrorInfo hint",
            hint,
            "must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_error_context(key: &str, value: &str) -> Result<(), CoreError> {
    match key {
        "session_id" | "turn_id" | "call_id" | "tool_name" | "provider_name"
        | "model_role" | "config_path" | "field_path" | "artifact_id"
        | "checkpoint_id" | "http_status" | "exit_code" => {}
        _ => {
            return Err(CoreError::InvalidIdentifier {
                kind: "MerryErrorInfo context key",
                value: key.to_owned(),
                reason: "context key is not allowed",
            });
        }
    }
    validate_common_error_context_value(value)
}

fn validate_common_error_context_value(value: &str) -> Result<(), CoreError> {
    if value.chars().count() > MAX_ERROR_CONTEXT_VALUE_LEN {
        return Err(invalid_diagnostic(
            "MerryErrorInfo context value",
            value,
            "is longer than the allowed maximum length",
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_diagnostic(
            "MerryErrorInfo context value",
            value,
            "must not contain control characters",
        ));
    }
    Ok(())
}
```

In `crates/merry-core/src/lib.rs`, export the new types:

```rust
pub use event::{
    ErrorInfo, MerryErrorDomain, MerryErrorInfo, MerryRetryability, RuntimeEvent,
    RuntimeEventKind,
};
```

- [x] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p merry-core merry_error_info --test protocol
```

Expected: PASS.

## Task 2: Add Python Packaging Skeleton

**Files:**

- Modify: `Cargo.toml`
- Create: `sdks/python/pyproject.toml`
- Create: `crates/merry-py/Cargo.toml`
- Create: `crates/merry-py/src/lib.rs`
- Create: `sdks/python/merry/__init__.py`
- Create: `sdks/python/merry/py.typed`
- Create: `sdks/python/tests/test_import.py`

- [x] **Step 1: Write failing import test**

Create `sdks/python/tests/test_import.py`:

```python
def test_import_exposes_version():
    import merry

    assert isinstance(merry.__version__, str)
    assert merry.__version__
```

- [x] **Step 2: Run test to verify RED**

Run:

```bash
python -m pytest sdks/python/tests/test_import.py -q
```

Expected: FAIL with `ModuleNotFoundError: No module named 'merry'`.

- [x] **Step 3: Add workspace and maturin package skeleton**

Modify root `Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = [
    "crates/merry-core",
    "crates/merry-llm",
    "crates/merry-runtime",
    "crates/merry-tool-workspace",
    "crates/merry-provider-openai",
    "crates/merry-cli",
    "crates/merry-py",
]
default-members = [
    "crates/merry-core",
    "crates/merry-llm",
    "crates/merry-runtime",
    "crates/merry-tool-workspace",
    "crates/merry-provider-openai",
    "crates/merry-cli",
    "crates/merry-py",
]

[workspace.dependencies]
pyo3 = { version = "0.28.3", features = ["extension-module"] }
```

Create `sdks/python/pyproject.toml`:

```toml
[build-system]
requires = ["maturin>=1.0,<2.0"]
build-backend = "maturin"

[project]
name = "merry"
version = "0.1.0"
description = "Python bindings for the Merry agent runtime"
requires-python = ">=3.10"
classifiers = [
    "Programming Language :: Python",
    "Programming Language :: Rust",
]

[tool.maturin]
manifest-path = "../../crates/merry-py/Cargo.toml"
python-source = "."
module-name = "merry._merry"
bindings = "pyo3"
```

Create `crates/merry-py/Cargo.toml`:

```toml
[package]
name = "merry-py"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish = false

[lib]
name = "_merry"
crate-type = ["cdylib", "rlib"]

[dependencies]
merry-core = { path = "../merry-core" }
pyo3.workspace = true

[lints]
workspace = true
```

Create `crates/merry-py/src/lib.rs`:

```rust
//! Python bindings for the Merry runtime.

use pyo3::prelude::*;

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pymodule]
#[pyo3(name = "_merry")]
fn merry_py(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(version, module)?)?;
    Ok(())
}
```

Create `sdks/python/merry/__init__.py`:

```python
from . import _merry

__version__ = _merry.version()

__all__ = ["__version__"]
```

Create empty `sdks/python/merry/py.typed`.

- [x] **Step 4: Build editable package and verify import**

Run:

```bash
python -m pip install -e sdks/python
python -m pytest sdks/python/tests/test_import.py -q
```

Expected: PASS.

If `pip install -e sdks/python` fails because maturin is not installed or Cargo needs to fetch `pyo3`, rerun with the required approved network/dependency installation path. Do not replace PyO3 with a hand-written fallback.

## Task 3: Add Python Error Mapping

**Files:**

- Create: `crates/merry-py/src/error.rs`
- Modify: `crates/merry-py/src/lib.rs`
- Create: `sdks/python/merry/_errors.py`
- Modify: `sdks/python/merry/__init__.py`
- Create: `sdks/python/tests/test_errors.py`

- [x] **Step 1: Write failing Python error test**

Create `sdks/python/tests/test_errors.py`:

```python
import pytest

import merry


def test_merry_error_exposes_stable_info():
    info = merry.MerryErrorInfo(
        code="config.invalid",
        domain="config",
        message="Config is invalid.",
        hint="Fix the TOML file.",
        retryability="user_action_required",
        context={"config_path": "merry.toml"},
    )
    error = merry.MerryError(info)

    assert str(error) == "Config is invalid."
    assert error.info == info
    assert error.code == "config.invalid"
    assert error.domain == "config"
    assert error.retryability == "user_action_required"


def test_native_invalid_session_error_maps_to_merry_error():
    with pytest.raises(merry.MerryRuntimeError) as raised:
        merry.Runtime(session_id=" ")

    assert raised.value.code == "runtime.invalid_session_id"
    assert raised.value.domain == "runtime"
    assert raised.value.retryability == "user_action_required"
```

- [x] **Step 2: Run test to verify RED**

Run:

```bash
python -m pytest sdks/python/tests/test_errors.py -q
```

Expected: FAIL because `MerryErrorInfo`, `MerryError`, `MerryRuntimeError`, and `Runtime` do not exist.

- [x] **Step 3: Add Rust error conversion helpers**

Create `crates/merry-py/src/error.rs`:

```rust
use merry_core::{CoreError, MerryErrorDomain, MerryErrorInfo, MerryRetryability};
use pyo3::{create_exception, exceptions::PyException, prelude::*};

create_exception!(_merry, NativeMerryError, PyException);

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("NativeMerryError", module.py().get_type::<NativeMerryError>())?;
    Ok(())
}

pub(crate) fn runtime_error_info(
    code: &str,
    message: impl AsRef<str>,
    hint: Option<&str>,
) -> MerryErrorInfo {
    let mut builder = MerryErrorInfo::builder(
        code,
        MerryErrorDomain::Runtime,
        message.as_ref(),
        MerryRetryability::UserActionRequired,
    );
    if let Some(hint) = hint {
        builder = builder.hint(hint);
    }
    builder
        .build()
        .expect("binding-generated runtime error info should be valid")
}

pub(crate) fn core_error_to_py(error: CoreError, code: &str, hint: &str) -> PyErr {
    let info = runtime_error_info(code, error.to_string(), Some(hint));
    merry_info_to_py_err(info)
}

pub(crate) fn merry_info_to_py_err(info: MerryErrorInfo) -> PyErr {
    Python::attach(|py| {
        let payload = serde_json::to_string(&info)
            .expect("MerryErrorInfo must serialize for Python exception payload");
        NativeMerryError::new_err(payload).into_pyobject(py).unwrap().into()
    })
}
```

Add `serde_json = "1.0"` to `crates/merry-py/Cargo.toml`.

- [x] **Step 4: Add Python exception wrappers**

Create `sdks/python/merry/_errors.py`:

```python
from __future__ import annotations

from dataclasses import dataclass, field
import json
from typing import Mapping

from . import _merry


@dataclass(frozen=True)
class MerryErrorInfo:
    code: str
    domain: str
    message: str
    hint: str | None = None
    retryability: str = "unknown"
    context: Mapping[str, str] = field(default_factory=dict)


class MerryError(Exception):
    def __init__(self, info: MerryErrorInfo):
        super().__init__(info.message)
        self.info = info

    @property
    def code(self) -> str:
        return self.info.code

    @property
    def domain(self) -> str:
        return self.info.domain

    @property
    def retryability(self) -> str:
        return self.info.retryability


class MerryConfigError(MerryError):
    pass


class MerryProviderError(MerryError):
    pass


class MerryRuntimeError(MerryError):
    pass


class MerryToolError(MerryError):
    pass


class MerryPolicyError(MerryError):
    pass


class MerryContextError(MerryError):
    pass


class MerryCompactionError(MerryError):
    pass


class MerryInternalError(MerryError):
    pass


class MerryTurnError(MerryError):
    pass


_DOMAIN_TO_ERROR = {
    "config": MerryConfigError,
    "provider": MerryProviderError,
    "runtime": MerryRuntimeError,
    "tool": MerryToolError,
    "policy": MerryPolicyError,
    "context": MerryContextError,
    "compaction": MerryCompactionError,
    "internal": MerryInternalError,
}


def _decode_native_error(error: _merry.NativeMerryError) -> MerryError:
    payload = error.args[0] if error.args else "{}"
    raw = json.loads(payload)
    info = MerryErrorInfo(
        code=raw["code"],
        domain=raw["domain"],
        message=raw["message"],
        hint=raw.get("hint"),
        retryability=raw["retryability"],
        context=raw.get("context", {}),
    )
    error_type = _DOMAIN_TO_ERROR.get(info.domain, MerryInternalError)
    return error_type(info)
```

Modify `sdks/python/merry/__init__.py`:

```python
from . import _merry
from ._errors import (
    MerryCompactionError,
    MerryConfigError,
    MerryContextError,
    MerryError,
    MerryErrorInfo,
    MerryInternalError,
    MerryPolicyError,
    MerryProviderError,
    MerryRuntimeError,
    MerryToolError,
    MerryTurnError,
)

__version__ = _merry.version()

__all__ = [
    "__version__",
    "MerryCompactionError",
    "MerryConfigError",
    "MerryContextError",
    "MerryError",
    "MerryErrorInfo",
    "MerryInternalError",
    "MerryPolicyError",
    "MerryProviderError",
    "MerryRuntimeError",
    "MerryToolError",
    "MerryTurnError",
]
```

- [x] **Step 5: Add minimal native Runtime constructor**

Extend `crates/merry-py/src/lib.rs`:

```rust
mod error;

use merry_core::SessionId;
use pyo3::prelude::*;

#[pyclass(name = "Runtime")]
struct PyRuntime {
    session_id: SessionId,
}

#[pymethods]
impl PyRuntime {
    #[new]
    fn new(session_id: String) -> PyResult<Self> {
        let session_id = SessionId::new(&session_id).map_err(|error| {
            error::core_error_to_py(
                error,
                "runtime.invalid_session_id",
                "Use a non-empty stable session id without surrounding whitespace.",
            )
        })?;
        Ok(Self { session_id })
    }
}

#[pymodule]
#[pyo3(name = "_merry")]
fn merry_py(module: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(module)?;
    module.add_class::<PyRuntime>()?;
    module.add_function(wrap_pyfunction!(version, module)?)?;
    Ok(())
}
```

- [x] **Step 6: Rebuild and verify GREEN**

Run:

```bash
python -m pip install -e sdks/python
python -m pytest sdks/python/tests/test_errors.py -q
```

Expected: PASS.

## Task 4: Expose Fake-Provider Runtime Run

**Files:**

- Create: `crates/merry-py/src/runtime.rs`
- Create: `crates/merry-py/src/serde_py.rs`
- Modify: `crates/merry-py/src/lib.rs`
- Modify: `crates/merry-py/Cargo.toml`
- Create: `sdks/python/merry/_runtime.py`
- Modify: `sdks/python/merry/__init__.py`
- Create: `sdks/python/tests/test_runtime.py`

- [x] **Step 1: Write failing Python runtime test**

Create `sdks/python/tests/test_runtime.py`:

```python
import merry


async def test_runtime_run_returns_final_output_and_events():
    runtime = merry.Runtime.with_fake_response("done")

    result = await runtime.run("Say done.")

    assert result.final_output == "done"
    assert result.status == "completed"
    assert result.steps_run == 1
    assert [event["kind"]["type"] for event in result.events] == [
        "session_started",
        "step_started",
        "artifact_recorded",
        "step_completed",
    ]


async def test_runtime_run_stream_yields_event_dicts_in_order():
    runtime = merry.Runtime.with_fake_response("streamed")

    event_types = []
    async for event in runtime.run_stream("Say streamed."):
        event_types.append(event["kind"]["type"])

    assert event_types[-1] == "step_completed"
```

- [x] **Step 2: Run test to verify RED**

Run:

```bash
python -m pytest sdks/python/tests/test_runtime.py -q
```

Expected: FAIL because `Runtime.with_fake_response`, `run`, and `run_stream` do not exist.

- [x] **Step 3: Add Rust runtime facade**

Modify `crates/merry-py/Cargo.toml`:

```toml
[dependencies]
futures-executor.workspace = true
merry-core = { path = "../merry-core" }
merry-llm = { path = "../merry-llm" }
merry-runtime = { path = "../merry-runtime" }
pyo3.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio = { workspace = true, features = ["rt"] }
tokio-util.workspace = true
```

Create `crates/merry-py/src/serde_py.rs`:

```rust
use pyo3::{prelude::*, types::{PyDict, PyList}};
use serde_json::Value;

pub(crate) fn json_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(match value {
        Value::Null => py.None(),
        Value::Bool(value) => value.into_pyobject(py)?.unbind().into_any(),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                integer.into_pyobject(py)?.unbind().into_any()
            } else if let Some(unsigned) = value.as_u64() {
                unsigned.into_pyobject(py)?.unbind().into_any()
            } else {
                value.as_f64().unwrap().into_pyobject(py)?.unbind().into_any()
            }
        }
        Value::String(value) => value.into_pyobject(py)?.unbind().into_any(),
        Value::Array(values) => {
            let list = PyList::empty(py);
            for item in values {
                list.append(json_to_py(py, item)?)?;
            }
            list.unbind().into_any()
        }
        Value::Object(values) => {
            let dict = PyDict::new(py);
            for (key, item) in values {
                dict.set_item(key, json_to_py(py, item)?)?;
            }
            dict.unbind().into_any()
        }
    })
}
```

Create `crates/merry-py/src/runtime.rs`:

```rust
use crate::{error, serde_py::json_to_py};
use merry_core::SessionId;
use merry_llm::{testing::FakeModelProvider, FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse};
use merry_runtime::{AgentLoopConfig, AgentLoopStatus, Runtime, StepContext, StepInput};
use pyo3::{prelude::*, types::PyDict};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[pyclass(name = "Runtime")]
pub(crate) struct PyRuntime {
    runtime: Runtime,
}

#[pymethods]
impl PyRuntime {
    #[new]
    fn new(session_id: String) -> PyResult<Self> {
        let session_id = SessionId::new(&session_id).map_err(|error| {
            error::core_error_to_py(
                error,
                "runtime.invalid_session_id",
                "Use a non-empty stable session id without surrounding whitespace.",
            )
        })?;
        let runtime = Runtime::builder(session_id).build().map_err(error::runtime_error_to_py)?;
        Ok(Self { runtime })
    }

    #[staticmethod]
    fn with_fake_response(final_text: String) -> PyResult<Self> {
        let provider = FakeModelProvider::new(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: final_text.clone(),
            }),
            Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(&final_text)],
                    FinishReason::Stop,
                    None,
                ),
            }),
        ]);
        let runtime = Runtime::builder(
            SessionId::new("python-sdk-fake").expect("static session id is valid"),
        )
        .model_provider(
            Arc::new(provider),
            ModelName::new("fake/python-sdk").expect("static model name is valid"),
        )
        .build()
        .map_err(error::runtime_error_to_py)?;
        Ok(Self { runtime })
    }

    fn run_blocking(&self, py: Python<'_>, task: String) -> PyResult<Py<PyAny>> {
        let runtime = self.runtime.clone();
        py.allow_threads(move || run_to_python(runtime, task))
    }
}

fn run_to_python(runtime: Runtime, task: String) -> PyResult<Py<PyAny>> {
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| error::runtime_message_to_py("runtime.tokio_init_failed", error.to_string()))?;
    let result = tokio_runtime
        .block_on(async move {
            let input = StepInput::new(&task)?;
            let context = StepContext::new(CancellationToken::new());
            runtime
                .run_agent_loop(input, context, AgentLoopConfig::default())
                .await
        })
        .map_err(error::agent_loop_error_to_py)?;

    Python::attach(|py| {
        let dict = PyDict::new(py);
        let status = match result.status() {
            AgentLoopStatus::Completed => "completed",
            AgentLoopStatus::Failed { .. } => "failed",
            AgentLoopStatus::Cancelled { .. } => "cancelled",
            AgentLoopStatus::Blocked { .. } => "blocked",
        };
        dict.set_item("status", status)?;
        dict.set_item("steps_run", result.steps_run())?;
        dict.set_item("final_output", result.final_output())?;
        let events = serde_json::to_value(result.events())
            .expect("RuntimeEvent values must serialize for Python");
        dict.set_item("events", json_to_py(py, &events)?)?;
        Ok(dict.unbind().into_any())
    })
}
```

Extend `crates/merry-py/src/error.rs` with runtime mappings:

```rust
use merry_runtime::{AgentLoopError, RuntimeError};

pub(crate) fn runtime_error_to_py(error: RuntimeError) -> PyErr {
    runtime_message_to_py("runtime.error", error.to_string())
}

pub(crate) fn agent_loop_error_to_py(error: AgentLoopError) -> PyErr {
    runtime_message_to_py("runtime.agent_loop_error", error.to_string())
}

pub(crate) fn runtime_message_to_py(code: &str, message: String) -> PyErr {
    let info = runtime_error_info(code, message, None);
    merry_info_to_py_err(info)
}
```

Modify `crates/merry-py/src/lib.rs`:

```rust
mod error;
mod runtime;
mod serde_py;

use pyo3::prelude::*;

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pymodule]
#[pyo3(name = "_merry")]
fn merry_py(module: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(module)?;
    module.add_class::<runtime::PyRuntime>()?;
    module.add_function(wrap_pyfunction!(version, module)?)?;
    Ok(())
}
```

- [x] **Step 4: Add Python async wrappers**

Create `sdks/python/merry/_runtime.py`:

```python
from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any, AsyncIterator

from . import _merry
from ._errors import NativeMerryError, _decode_native_error


@dataclass(frozen=True)
class RunResult:
    status: str
    steps_run: int
    final_output: str | None
    events: list[dict[str, Any]]


class Runtime:
    def __init__(self, session_id: str = "python-sdk"):
        try:
            self._native = _merry.Runtime(session_id)
        except _merry.NativeMerryError as error:
            raise _decode_native_error(error) from None

    @classmethod
    def with_fake_response(cls, final_text: str) -> "Runtime":
        instance = cls.__new__(cls)
        try:
            instance._native = _merry.Runtime.with_fake_response(final_text)
        except _merry.NativeMerryError as error:
            raise _decode_native_error(error) from None
        return instance

    async def run(self, task: str) -> RunResult:
        try:
            raw = await asyncio.to_thread(self._native.run_blocking, task)
        except _merry.NativeMerryError as error:
            raise _decode_native_error(error) from None
        return RunResult(
            status=raw["status"],
            steps_run=raw["steps_run"],
            final_output=raw["final_output"],
            events=list(raw["events"]),
        )

    async def run_stream(self, task: str) -> AsyncIterator[dict[str, Any]]:
        result = await self.run(task)
        for event in result.events:
            yield event
```

Modify `sdks/python/merry/_errors.py` to export the native exception alias:

```python
NativeMerryError = _merry.NativeMerryError
```

Modify `sdks/python/merry/__init__.py`:

```python
from ._runtime import RunResult, Runtime

__all__ = [
    "__version__",
    "RunResult",
    "Runtime",
    ...
]
```

- [x] **Step 5: Rebuild and verify GREEN**

Run:

```bash
python -m pip install -e sdks/python
python -m pytest sdks/python/tests/test_runtime.py -q
```

Expected: PASS.

## Task 5: Add Scripted Tool Boundary Tests

**Files:**

- Modify: `crates/merry-py/src/runtime.rs`
- Modify: `sdks/python/merry/_runtime.py`
- Modify: `sdks/python/merry/__init__.py`
- Create: `sdks/python/tests/test_tools.py`

- [x] **Step 1: Write failing Python tool tests**

Create `sdks/python/tests/test_tools.py`:

```python
import pytest

import merry


async def test_scripted_tool_domain_failure_resolves_tool_and_continues():
    runtime = merry.Runtime.with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="Order was not found.",
    )

    runtime.register_static_tool_failure(
        name="lookup_order",
        description="Look up an order.",
        diagnostic_code="tool.domain_failed",
        message="Order was not found.",
        content={"found": False},
    )

    result = await runtime.run("Check order A123.")

    assert result.status == "completed"
    resolved = [
        event for event in result.events
        if event["kind"]["type"] == "tool_call_resolved"
    ]
    assert resolved[0]["kind"]["result"]["status"] == "failed"
    assert resolved[0]["kind"]["result"]["diagnostic"]["code"] == "tool.domain_failed"


async def test_scripted_tool_executor_exception_raises_tool_error():
    runtime = merry.Runtime.with_scripted_tool_call(
        tool_name="lookup_order",
        arguments={"order_id": "A123"},
        final_text="unreachable",
    )
    runtime.register_static_tool_exception(
        name="lookup_order",
        description="Look up an order.",
        message="database unavailable",
    )

    with pytest.raises(merry.MerryToolError) as raised:
        await runtime.run("Check order A123.")

    assert raised.value.code == "tool.executor_exception"
    assert raised.value.domain == "tool"
    assert raised.value.retryability == "not_retryable"
```

- [x] **Step 2: Run test to verify RED**

Run:

```bash
python -m pytest sdks/python/tests/test_tools.py -q
```

Expected: FAIL because `with_scripted_tool_call` and static tool registration helpers do not exist.

- [x] **Step 3: Add scripted provider constructor**

In `crates/merry-py/src/runtime.rs`, add imports:

```rust
use merry_core::{ToolCallArguments, ToolCallId, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{ModelToolCall, ModelToolCallId, ToolArguments};
use serde_json::{json, Map, Value};
```

Add a new static constructor:

```rust
#[staticmethod]
fn with_scripted_tool_call(
    tool_name: String,
    arguments_json: String,
    final_text: String,
) -> PyResult<Self> {
    let arguments: Value = serde_json::from_str(&arguments_json)
        .map_err(|error| error::runtime_message_to_py("tool.input_invalid", error.to_string()))?;
    let arguments = match arguments {
        Value::Object(arguments) => arguments,
        _ => {
            return Err(error::runtime_message_to_py(
                "tool.input_invalid",
                "tool arguments must be a JSON object".to_owned(),
            ));
        }
    };
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-python-tool").map_err(error::model_error_to_py)?,
        ToolName::new(&tool_name).map_err(|error| {
            error::core_error_to_py(
                error,
                "tool.registration_invalid",
                "Use a provider-portable tool name.",
            )
        })?,
        ToolArguments::new(arguments),
    );
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested { call }),
        Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(&final_text)],
                FinishReason::Stop,
                None,
            ),
        }),
    ]);
    let runtime = Runtime::builder(
        SessionId::new("python-sdk-scripted-tool").expect("static session id is valid"),
    )
    .model_provider(
        Arc::new(provider),
        ModelName::new("fake/python-sdk").expect("static model name is valid"),
    )
    .build()
    .map_err(error::runtime_error_to_py)?;
    Ok(Self { runtime })
}
```

- [x] **Step 4: Add static tool executors**

In `crates/merry-py/src/runtime.rs`, add:

```rust
use merry_runtime::{
    ArtifactContent, RegisteredTool, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;

#[derive(Clone)]
enum StaticToolBehavior {
    DomainFailure {
        diagnostic_code: String,
        message: String,
        content: String,
    },
    ExecutorException {
        message: String,
    },
}

#[derive(Clone)]
struct StaticToolExecutor {
    behavior: StaticToolBehavior,
}

impl ToolExecutor for StaticToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            match &self.behavior {
                StaticToolBehavior::DomainFailure {
                    diagnostic_code,
                    message,
                    content,
                } => {
                    let diagnostic = merry_core::ErrorInfo::new(diagnostic_code, message)
                        .expect("static tool diagnostic should be valid");
                    Ok(ToolExecutionOutcome::failed_json(content.clone(), diagnostic))
                }
                StaticToolBehavior::ExecutorException { message } => Err(
                    ToolExecutionError::infrastructure(message.clone()),
                ),
            }
        })
    }
}
```

Add pyclass methods:

```rust
fn register_static_tool_failure(
    &mut self,
    name: String,
    description: String,
    diagnostic_code: String,
    message: String,
    content_json: String,
) -> PyResult<()> {
    let tool = static_tool(
        &name,
        &description,
        StaticToolBehavior::DomainFailure {
            diagnostic_code,
            message,
            content: content_json,
        },
    )?;
    self.runtime = self
        .runtime
        .clone()
        .rebuild_with_registered_tool(tool)
        .map_err(error::runtime_error_to_py)?;
    Ok(())
}

fn register_static_tool_exception(
    &mut self,
    name: String,
    description: String,
    message: String,
) -> PyResult<()> {
    let tool = static_tool(
        &name,
        &description,
        StaticToolBehavior::ExecutorException { message },
    )?;
    self.runtime = self
        .runtime
        .clone()
        .rebuild_with_registered_tool(tool)
        .map_err(error::runtime_error_to_py)?;
    Ok(())
}
```

This step requires one small runtime API before the code compiles:

```rust
impl Runtime {
    pub fn rebuild_with_registered_tool(&self, tool: RegisteredTool) -> Result<Self, RuntimeError> {
        let session_id = self.session_id().clone();
        Runtime::builder(session_id).register_tool(tool).build()
    }
}
```

If preserving existing provider config through rebuild is more invasive than this MVP justifies, replace the rebuild helper with binding-owned `PyRuntimeConfig` state that stores fake-provider script inputs and registered tools, then builds the Rust `Runtime` inside each `run_blocking` call. Prefer the binding-owned config state because it keeps new public runtime APIs smaller.

Add helper:

```rust
fn static_tool(
    name: &str,
    description: &str,
    behavior: StaticToolBehavior,
) -> PyResult<RegisteredTool> {
    let schema = Schema::try_from(json!({
        "type": "object",
        "additionalProperties": true
    }))
    .expect("static object schema should be valid");
    let spec = ToolSpec::new(
        ToolName::new(name).map_err(|error| {
            error::core_error_to_py(
                error,
                "tool.registration_invalid",
                "Use a provider-portable tool name.",
            )
        })?,
        description,
        ToolInputSchema::new(schema).map_err(|error| {
            error::core_error_to_py(
                error,
                "tool.registration_invalid",
                "Use an object JSON schema for tool input.",
            )
        })?,
    )
    .map_err(|error| {
        error::core_error_to_py(
            error,
            "tool.registration_invalid",
            "Use a non-empty tool description.",
        )
    })?;
    Ok(RegisteredTool::read_only(
        spec,
        Arc::new(StaticToolExecutor { behavior }),
    ))
}
```

- [x] **Step 5: Map executor infrastructure failure to tool.executor_exception**

In `crates/merry-py/src/error.rs`, add:

```rust
pub(crate) fn tool_executor_exception(message: String) -> PyErr {
    let info = MerryErrorInfo::builder(
        "tool.executor_exception",
        MerryErrorDomain::Tool,
        &format!("Tool executor raised an unexpected exception: {message}"),
        MerryRetryability::NotRetryable,
    )
    .hint("Handle expected business failures inside the tool result instead of raising.")
    .build()
    .expect("binding-generated tool error info should be valid");
    merry_info_to_py_err(info)
}
```

Use this mapping when `AgentLoopError::runtime_error()` matches `RuntimeError::ToolExecutionFailed`.

- [x] **Step 6: Add Python wrapper methods**

In `sdks/python/merry/_runtime.py`, add:

```python
import json
from collections.abc import Mapping

@classmethod
def with_scripted_tool_call(
    cls,
    *,
    tool_name: str,
    arguments: Mapping[str, object],
    final_text: str,
) -> "Runtime":
    instance = cls.__new__(cls)
    try:
        instance._native = _merry.Runtime.with_scripted_tool_call(
            tool_name,
            json.dumps(arguments, sort_keys=True),
            final_text,
        )
    except _merry.NativeMerryError as error:
        raise _decode_native_error(error) from None
    return instance

def register_static_tool_failure(
    self,
    *,
    name: str,
    description: str,
    diagnostic_code: str,
    message: str,
    content: Mapping[str, object],
) -> None:
    try:
        self._native.register_static_tool_failure(
            name,
            description,
            diagnostic_code,
            message,
            json.dumps(content, sort_keys=True),
        )
    except _merry.NativeMerryError as error:
        raise _decode_native_error(error) from None

def register_static_tool_exception(
    self,
    *,
    name: str,
    description: str,
    message: str,
) -> None:
    try:
        self._native.register_static_tool_exception(name, description, message)
    except _merry.NativeMerryError as error:
        raise _decode_native_error(error) from None
```

- [x] **Step 7: Rebuild and verify GREEN**

Run:

```bash
python -m pip install -e sdks/python
python -m pytest sdks/python/tests/test_tools.py -q
```

Expected: PASS.

## Task 6: Add Rust Binding Tests

**Files:**

- Create: `crates/merry-py/tests/bindings.rs`

- [x] **Step 1: Write Rust binding tests**

Create `crates/merry-py/tests/bindings.rs`:

```rust
use merry_core::{MerryErrorDomain, MerryErrorInfo, MerryRetryability};

#[test]
fn binding_error_info_shape_matches_sdk_contract() {
    let info = MerryErrorInfo::builder(
        "runtime.invalid_session_id",
        MerryErrorDomain::Runtime,
        "invalid session id",
        MerryRetryability::UserActionRequired,
    )
    .hint("Use a non-empty session id.")
    .build()
    .expect("valid error info");

    let value = serde_json::to_value(&info).expect("serializes");
    assert_eq!(value["code"], "runtime.invalid_session_id");
    assert_eq!(value["domain"], "runtime");
    assert_eq!(value["retryability"], "user_action_required");
    assert_eq!(value["hint"], "Use a non-empty session id.");
}
```

- [x] **Step 2: Run test to verify GREEN**

Run:

```bash
cargo test -p merry-py
```

Expected: PASS.

## Task 7: Document MVP Boundaries And Run Full Verification

**Files:**

- Modify: `README.md`
- Modify: `ROADMAP.md`

- [x] **Step 1: Add README Python SDK example**

Add a short section to `README.md`:

```markdown
## Python SDK MVP

The first Python binding slice exposes an embedded runtime through a PyO3
extension module and pure Python wrappers:

```python
import merry

runtime = merry.Runtime.with_fake_response("done")
result = await runtime.run("Say done.")
print(result.final_output)
```

This MVP is deterministic and fake-provider backed. It proves the Python package
shape, async API, event/result serialization, and structured `MerryErrorInfo`
exceptions. Live provider config, real workspace tools, async Python tool
callbacks, and true incremental event streaming are later slices.
```

- [x] **Step 2: Update roadmap status without changing product priority**

In `ROADMAP.md`, add a `Recently Completed` bullet after the current coding-loop entries:

```markdown
- Python SDK MVP first slice is implemented as `merry-py` plus `sdks/python/merry`:
  it exposes a PyO3-backed package import, fake-provider `Runtime.run`,
  buffered async `run_stream`, structured `MerryErrorInfo` Python exceptions,
  and deterministic Python/Rust tests. This proves Merry can be embedded from
  Python without live provider credentials. It does not yet expose live OpenAI
  config, real workspace tools, Pydantic decorators, async Python tool
  callbacks, or true incremental streaming.
```

Do not move the roadmap `Next Active` priority unless the user explicitly asks to replace it.

- [x] **Step 3: Run focused verification**

Run:

```bash
cargo fmt --all --check
cargo clippy -p merry-core -p merry-py --all-targets --all-features -- -D warnings
cargo test -p merry-core merry_error_info --test protocol
cargo test -p merry-py
python -m pip install -e sdks/python
python -m pytest sdks/python/tests -q
```

Expected: all commands PASS.

- [x] **Step 4: Run repository Rust verification**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: PASS. If `merry-py` introduces host Python linker requirements that break all-feature workspace checks, adjust only `merry-py` crate features or PyO3 configuration; do not weaken workspace lints or remove `merry-py` from default verification without recording the reason.

## Self-Review Notes

- Verification so far: Task 1-5 focused checks passed (`cargo fmt --all --check`, `cargo test -p merry-core merry_error_info --test protocol`, `cargo test -p merry-py`, and Python pytest for import/errors/runtime/tools through `uv run --with pytest --with maturin --with-editable sdks/python`).
- Final verification: `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --all` passed after Task 7 docs/status updates.
- Spec coverage: The plan covers package layout, embedded runtime construction, fake-provider agent loop run, async Python result/stream API shape, stable error info, and first tool-domain versus executor-exception distinction.
- Intentional gap: The spec's full config initialization, workspace tools, Pydantic decorators, async Python tool callbacks, live provider mapping, and true incremental streaming are excluded from this first MVP and named as follow-up slices.
- Boundary check: PyO3 stays in `crates/merry-py`; Python package code stays under `sdks/python/merry`; provider wire formats remain outside runtime and Python tests use fake provider only.
- Verification: Required Rust checks remain `cargo fmt`, `cargo clippy`, and `cargo test`; Python adds `python -m pip install -e sdks/python` and `python -m pytest sdks/python/tests -q`.
