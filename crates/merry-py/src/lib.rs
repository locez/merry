//! Thin Python bindings for the public Merry facade.
//!
//! PyO3 types in this crate only bridge construction, owned run messages, and
//! lifecycle methods. Runtime state, policy, tool outputs, and tool admission
//! remain owned by the Rust facade and runtime crates.

mod agent;
mod builder;
mod error;
mod interactive;
mod protocol;
mod run;
mod run_state;
#[cfg(feature = "test-utils")]
mod testing;

use pyo3::prelude::*;

#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[pymodule]
#[pyo3(name = "_merry")]
fn merry_py(module: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(module)?;
    module.add_class::<builder::PyAgentBuilder>()?;
    module.add_class::<agent::PyAgent>()?;
    module.add_class::<run::PyAgentRun>()?;
    module.add_class::<interactive::PyInteractiveRun>()?;
    #[cfg(feature = "test-utils")]
    testing::register(module)?;
    module.add_function(wrap_pyfunction!(version, module)?)?;
    Ok(())
}
