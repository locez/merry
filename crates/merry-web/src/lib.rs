//! Local Web service boundary for Merry observability and future WebUI work.

mod assets;
mod backend;
mod config;
mod server;

pub use backend::{WebBackend, WebBackendError, WebToolOutput};
pub use config::{DEFAULT_PORT, WebServerConfig};
pub use server::{WebServerError, WebServerHandle, start};
