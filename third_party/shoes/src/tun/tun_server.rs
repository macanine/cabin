//! TUN server configuration.
//!
//! Cabin runs the stack on a socket the host owns, so only the protocol
//! switches, MTU and resource limits are configurable here.

/// Configuration for the TUN server.
#[derive(Clone, Debug)]
pub struct TunServerConfig {
    pub resource_limits: crate::config::tun::TunResourceLimits,
    /// MTU size for the packet device. Default: 1500.
    pub mtu: u16,
    /// Enable TCP connection handling.
    /// Default: true
    pub tcp_enabled: bool,
    /// Enable UDP packet handling.
    /// Default: true
    pub udp_enabled: bool,
    /// Enable ICMP (ping) handling.
    /// Default: true
    pub icmp_enabled: bool,
}

impl Default for TunServerConfig {
    fn default() -> Self {
        Self {
            resource_limits: Default::default(),
            mtu: 1500,
            tcp_enabled: true,
            udp_enabled: true,
            icmp_enabled: true,
        }
    }
}

impl TunServerConfig {
    /// Create a new TunServerConfig with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the MTU.
    pub fn mtu(mut self, mtu: u16) -> Self {
        self.mtu = mtu;
        self
    }

    /// Enable or disable TCP connection handling.
    pub fn tcp_enabled(mut self, enabled: bool) -> Self {
        self.tcp_enabled = enabled;
        self
    }

    /// Enable or disable UDP packet handling.
    pub fn udp_enabled(mut self, enabled: bool) -> Self {
        self.udp_enabled = enabled;
        self
    }

    /// Enable or disable ICMP (ping) handling.
    pub fn icmp_enabled(mut self, enabled: bool) -> Self {
        self.icmp_enabled = enabled;
        self
    }
}
