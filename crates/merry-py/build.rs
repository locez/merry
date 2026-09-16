//! Emits the linker arguments the `_merry` extension module needs on macOS.
//!
//! Building as an extension module (`pyo3/extension-module`) stops PyO3 from
//! linking `libpython`, so the macOS linker must instead leave the Python
//! symbols to be resolved by the host interpreter when the module is loaded.
//! PyO3 only adds those arguments for projects that ask for them, and
//! `maturin` is not in the `cargo build` path, so the crate owns them here.

fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
