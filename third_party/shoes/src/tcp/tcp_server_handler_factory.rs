//! Factory functions for creating TCP server handlers from config.

use std::net::IpAddr;
use std::sync::Arc;

use crate::client_proxy_selector::ClientProxySelector;
use crate::config::ServerProxyConfig;
use crate::http_handler::HttpTcpServerHandler;
use crate::mixed_handler::MixedTcpServerHandler;
use crate::resolver::Resolver;
use crate::socks_handler::SocksTcpServerHandler;
use crate::tcp::tcp_handler::TcpServerHandler;

fn create_auth_credentials(
    username: Option<String>,
    password: Option<String>,
) -> Option<(String, String)> {
    match (&username, &password) {
        (None, None) => None,
        _ => Some((username.unwrap_or_default(), password.unwrap_or_default())),
    }
}

/// Create a TCP server handler from config.
///
/// # Arguments
/// * `server_proxy_config` - The protocol configuration
/// * `client_proxy_selector` - Selector for outbound proxy routing
/// * `resolver` - DNS resolver
/// * `bind_ip` - Optional bind IP for handlers that need it (e.g., Socks5 UDP, Mixed)
///
/// The `bind_ip` is required for:
/// - `Socks` with `udp_enabled: true` (for UDP ASSOCIATE)
/// - `Mixed` with `udp_enabled: true` (for UDP ASSOCIATE)
pub fn create_tcp_server_handler(
    server_proxy_config: ServerProxyConfig,
    client_proxy_selector: &Arc<ClientProxySelector>,
    resolver: &Arc<dyn Resolver>,
    bind_ip: Option<IpAddr>,
) -> Box<dyn TcpServerHandler> {
    match server_proxy_config {
        ServerProxyConfig::Http { username, password } => Box::new(HttpTcpServerHandler::new(
            create_auth_credentials(username, password),
            client_proxy_selector.clone(),
        )),
        ServerProxyConfig::Socks {
            username,
            password,
            udp_enabled,
        } => {
            // Use 0.0.0.0 as default if bind_ip not provided
            let ip = bind_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
            Box::new(SocksTcpServerHandler::new(
                create_auth_credentials(username, password),
                udp_enabled,
                ip,
                client_proxy_selector.clone(),
                resolver.clone(),
            ))
        }
        ServerProxyConfig::Mixed {
            username,
            password,
            udp_enabled,
        } => {
            // Use 0.0.0.0 as default if bind_ip not provided
            let ip = bind_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
            Box::new(MixedTcpServerHandler::new(
                create_auth_credentials(username, password),
                udp_enabled,
                ip,
                client_proxy_selector.clone(),
                resolver.clone(),
            ))
        }
        _ => unreachable!("embedded listeners are validated before startup"),
    }
}
