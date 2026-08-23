//! Local Web service binding configuration.

use std::net::{IpAddr, SocketAddr};

/// Default local observability port.
pub const DEFAULT_PORT: u16 = 1225;

/// Bind settings for the local Web service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebServerConfig {
    bind_addr: IpAddr,
    port: u16,
}

impl Default for WebServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: DEFAULT_PORT,
        }
    }
}

impl WebServerConfig {
    /// Creates a bind configuration for an explicit TCP port.
    #[must_use]
    pub fn new(bind_addr: IpAddr, port: u16) -> Self {
        Self { bind_addr, port }
    }

    /// Returns the configured bind address.
    #[must_use]
    pub fn bind_addr(&self) -> IpAddr {
        self.bind_addr
    }

    /// Returns the configured port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_addr, self.port)
    }
}
