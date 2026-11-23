#![allow(dead_code)]

//! Cabin's embedded proxy client, loopback listeners, and VM packet stack.

mod address;
mod anytls;
mod async_stream;
mod buf_reader;
mod client_proxy_chain;
mod client_proxy_selector;
mod copy_bidirectional;
mod copy_bidirectional_message;
mod crypto;
pub mod dns;
mod h2mux;
mod http_handler;
mod listener_tasks;
mod mixed_handler;
mod naiveproxy;
mod obfs;
mod option_util;
mod port_forward_handler;
mod prepend_stream;
mod quic_endpoint;
mod quic_stream;
mod reality;
mod reality_client_handler;
pub mod resolver;
pub mod resources;
mod routing;
mod rustls_config_util;
mod rustls_connection_util;
mod shadow_tls;
mod shadowsocks;
mod slide_buffer;
mod snell;
mod socket_util;
mod socks5_udp_relay;
mod socks_handler;
mod stream_reader;
mod sync_adapter;
mod tcp;
mod thread_util;
mod tls_client_handler;
mod trojan_handler;
mod udp_fragments;
mod uot;
mod util;
mod uuid_util;
mod vless;
mod vmess;
mod websocket;
mod xudp;

/// Configuration types.
pub mod config;

/// Desktop embedding entry points (Cabin addition).
pub mod embed;

/// User-space network stack for Cabin's VM.
pub mod tun;
