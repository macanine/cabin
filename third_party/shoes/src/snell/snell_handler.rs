use std::sync::Arc;

use argon2::{Config as Argon2Config, ThreadMode, Variant, Version};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::snell_fixed_target_stream::SnellFixedTargetStream;
use super::snell_udp_stream::SnellUdpClientStream;
use crate::address::ResolvedLocation;
use crate::async_stream::AsyncMessageStream;
use crate::async_stream::AsyncStream;
use crate::shadowsocks::{
    ShadowsocksCipher, ShadowsocksKey, ShadowsocksStream, ShadowsocksStreamType,
};
use crate::tcp::tcp_handler::{TcpClientHandler, TcpClientSetupResult};
use crate::util::write_all;

// Snell protocol Argon2 parameters
// ref: https://github.com/icpz/open-snell/blob/master/components/aead/cipher.go#L48
const SNELL_ARGON2_CONFIG: Argon2Config<'static> = Argon2Config {
    variant: Variant::Argon2id,
    version: Version::Version13,
    mem_cost: 8,  // 8 KB
    time_cost: 3, // 3 iterations
    lanes: 1,     // parallelism = 1
    thread_mode: ThreadMode::Sequential,
    secret: &[],
    ad: &[],
    hash_length: 32,
};

#[derive(Debug, Clone)]
struct SnellKey {
    password_bytes: Box<[u8]>,
    key_len: usize,
}

impl SnellKey {
    pub fn new(password: &str, key_len: usize) -> Self {
        Self {
            password_bytes: password.as_bytes().to_vec().into_boxed_slice(),
            key_len,
        }
    }
}

impl ShadowsocksKey for SnellKey {
    fn create_session_key(&self, salt: &[u8]) -> Box<[u8]> {
        let output = argon2::hash_raw(&self.password_bytes, salt, &SNELL_ARGON2_CONFIG).unwrap();

        if self.key_len == 32 {
            output.into_boxed_slice()
        } else {
            output[0..self.key_len].to_vec().into_boxed_slice()
        }
    }
}

#[derive(Debug)]
pub struct SnellClientHandler {
    cipher: ShadowsocksCipher,
    key: Arc<Box<dyn ShadowsocksKey>>,
    udp_enabled: bool,
}

impl SnellClientHandler {
    pub fn new(cipher: ShadowsocksCipher, password: &str, udp_enabled: bool) -> Self {
        let key: Arc<Box<dyn ShadowsocksKey>> = Arc::new(Box::new(SnellKey::new(
            password,
            cipher.algorithm().key_len(),
        )));
        Self {
            cipher,
            key,
            udp_enabled,
        }
    }
}

#[async_trait]
impl TcpClientHandler for SnellClientHandler {
    async fn setup_client_tcp_stream(
        &self,
        client_stream: Box<dyn AsyncStream>,
        remote_location: ResolvedLocation,
    ) -> std::io::Result<TcpClientSetupResult> {
        let mut client_stream: Box<dyn AsyncStream> = Box::new(ShadowsocksStream::new(
            client_stream,
            ShadowsocksStreamType::Aead,
            self.cipher.algorithm(),
            self.cipher.salt_len(),
            self.key.clone(),
            None,
        ));

        let hostname_bytes = remote_location.address().to_string().into_bytes();

        if hostname_bytes.len() > 255 {
            return Err(std::io::Error::other("hostname is too long"));
        }

        write_all(
            &mut client_stream,
            &[
                1, // snell version,
                1, // connect command,
                0, // client id length,
                hostname_bytes.len() as u8,
            ],
        )
        .await?;

        write_all(&mut client_stream, &hostname_bytes).await?;

        let port = remote_location.location().port();

        write_all(
            &mut client_stream,
            &[(port >> 8) as u8, (port & 0xff) as u8],
        )
        .await?;

        client_stream.flush().await?;

        let mut response = [0u8; 1];
        let n = client_stream.read(&mut response).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected EOF when reading tunnel response",
            ));
        }

        if response[0] != 0 {
            return Err(std::io::Error::other(format!(
                "Got non-tunnel response ({})",
                response[0]
            )));
        }

        Ok(TcpClientSetupResult {
            client_stream,
            early_data: None,
        })
    }

    fn supports_udp_over_tcp(&self) -> bool {
        self.udp_enabled
    }

    async fn setup_client_udp_bidirectional(
        &self,
        client_stream: Box<dyn AsyncStream>,
        target: ResolvedLocation,
    ) -> std::io::Result<Box<dyn AsyncMessageStream>> {
        let mut ss_stream = ShadowsocksStream::new(
            client_stream,
            ShadowsocksStreamType::Aead,
            self.cipher.algorithm(),
            self.cipher.salt_len(),
            self.key.clone(),
            None,
        );

        write_all(
            &mut ss_stream,
            &[
                1, // snell version
                6, // UDP command
                0, // client id length
            ],
        )
        .await?;
        ss_stream.flush().await?;

        let mut response = [0u8; 1];
        let n = ss_stream.read(&mut response).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected EOF when reading UDP ready response",
            ));
        }

        if response[0] != 0 {
            return Err(std::io::Error::other(format!(
                "Got non-UDP-ready response ({})",
                response[0]
            )));
        }

        // Wraps multi-directional Snell UDP with a fixed target adapter for single-target mode.
        let max_payload_size = ShadowsocksStreamType::Aead.max_payload_len();
        let snell_udp_client_stream =
            SnellUdpClientStream::new(Box::new(ss_stream), max_payload_size);
        let fixed_target_stream =
            SnellFixedTargetStream::new(snell_udp_client_stream, target.into_location());

        Ok(Box::new(fixed_target_stream))
    }
}
