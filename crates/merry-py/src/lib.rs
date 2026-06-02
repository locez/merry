//! Python bindings for the Merry runtime.

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
    module.add_class::<runtime::NativeRuntimeEventStream>()?;
    module.add_class::<runtime::PyRuntime>()?;
    module.add_function(wrap_pyfunction!(version, module)?)?;
    Ok(())
}
