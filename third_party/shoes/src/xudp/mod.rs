// XUDP (Extended UDP) protocol implementation
// Protocol-agnostic UDP multiplexing over TCP connections
// Used by both VLESS and VMess protocols

pub mod frame;
#[cfg(test)]
pub mod message_stream;

#[cfg(test)]
pub use message_stream::XudpMessageStream;
