use std::time::SystemTime;

use async_trait::async_trait;
use aws_lc_rs::aead::{
    AES_128_GCM, Aad, BoundKey, CHACHA20_POLY1305, OpeningKey, SealingKey, UnboundKey,
};
use aws_lc_rs::cipher::{
    AES_128, EncryptingKey as CipherEncryptingKey, EncryptionContext, UnboundCipherKey,
};
use rand::{Rng, RngExt};
use shake::Shake128;
use shake::digest::{ExtendableOutput, Update};
use tokio::io::AsyncWriteExt;

use super::fnv1a::Fnv1aHasher;
use super::md5::{compute_md5, create_chacha_key};
use super::nonce::{SingleUseNonce, VmessNonceSequence};
use super::vmess_stream::{ReadHeaderInfo, VmessStream};
use crate::address::{Address, NetLocation, ResolvedLocation};
use crate::async_stream::{AsyncMessageStream, AsyncStream};
use crate::tcp::tcp_handler::{TcpClientHandler, TcpClientSetupResult};
use crate::util::write_all;
use crate::uuid_util::parse_uuid;

const TAG_LEN: usize = 16;

// VMess protocol command types
const COMMAND_TCP: u8 = 1;
const COMMAND_UDP: u8 = 2;
const COMMAND_MUX: u8 = 3; // MUX/XUDP mode

#[derive(Debug, Clone, PartialEq, Eq)]
enum DataCipher {
    Any,
    Aes128Gcm,
    ChaCha20Poly1305,
    None,
}

impl From<&str> for DataCipher {
    fn from(name: &str) -> Self {
        match name {
            "" | "any" => DataCipher::Any,
            "aes-128-gcm" => DataCipher::Aes128Gcm,
            "chacha20-poly1305" | "chacha20-ietf-poly1305" => DataCipher::ChaCha20Poly1305,
            "none" => DataCipher::None,
            _ => {
                panic!("Unknown cipher: {name}");
            }
        }
    }
}

pub struct VmessTcpClientHandler {
    data_cipher: DataCipher,
    instruction_key: [u8; 16],
    aead_encrypting_key: CipherEncryptingKey,
    udp_enabled: bool,
}

impl std::fmt::Debug for VmessTcpClientHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmessTcpClientHandler")
            .field("data_cipher", &self.data_cipher)
            .field("udp_enabled", &self.udp_enabled)
            .finish_non_exhaustive()
    }
}

impl VmessTcpClientHandler {
    pub fn new(cipher_name: &str, user_id: &str, udp_enabled: bool) -> Self {
        let mut user_id_bytes = parse_uuid(user_id).unwrap();
        user_id_bytes.extend(b"c48619fe-8f02-49e0-b9e9-edf763e17e21");
        let instruction_key: [u8; 16] = compute_md5(&user_id_bytes);

        let derived_key = super::sha2::kdf(&instruction_key, &[b"AES Auth ID Encryption"]);
        let unbound_key = UnboundCipherKey::new(&AES_128, &derived_key[0..16]).unwrap();
        let aead_encrypting_key = CipherEncryptingKey::ecb(unbound_key).unwrap();

        Self {
            data_cipher: cipher_name.into(),
            aead_encrypting_key,
            instruction_key,
            udp_enabled,
        }
    }
}

#[async_trait]
impl TcpClientHandler for VmessTcpClientHandler {
    async fn setup_client_tcp_stream(
        &self,
        mut client_stream: Box<dyn AsyncStream>,
        remote_location: ResolvedLocation,
    ) -> std::io::Result<TcpClientSetupResult> {
        // AEAD allows 120 second delta from the current time.
        // See authid.go in v2ray-core.
        let random_delta: u64 = rand::rng().random_range(0..241);
        let time_secs: u64 =
            SystemTime::UNIX_EPOCH.elapsed().unwrap().as_secs() - 120u64 + random_delta;

        let mut aead_bytes = [0u8; 16];
        let time_bytes = time_secs.to_be_bytes();
        aead_bytes[0..8].copy_from_slice(&time_bytes);

        rand::rng().fill_bytes(&mut aead_bytes[8..12]);

        let checksum_value = super::crc32::crc32c(&aead_bytes[0..12]);
        let checksum = checksum_value.to_be_bytes();
        aead_bytes[12..16].copy_from_slice(&checksum);

        self.aead_encrypting_key
            .less_safe_encrypt(&mut aead_bytes, EncryptionContext::None)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "AEAD auth ID encryption failed",
                )
            })?;

        let cert_hash = aead_bytes;

        // max length of encrypted header:
        // 41 (instructions up to addr type) + 256 (max domain name length 255 + 1 length byte) +
        // 15 (max margin length, 4 bits) + 4 (fnv1a hash) = 316 + TAG_LEN
        let mut header_bytes = [0u8; 316 + TAG_LEN];

        header_bytes[0] = 1;

        // this fills:
        // - data encryption iv (16 bytes)
        // - data encryption key (16 bytes)
        // - response authentication v (1 byte)
        rand::rng().fill_bytes(&mut header_bytes[1..34]);

        let data_encryption_iv: &[u8] = &header_bytes[1..17];
        let data_encryption_key: &[u8] = &header_bytes[17..33];
        let response_authentication_v = header_bytes[33];

        // construct everything where we need data_encryption_iv and data_encryption_key now,
        // because instructions_to_addr_type will be encrypted once it's filled.
        // AEAD mode only - use SHA256 for response header keys
        let mut truncated_iv = [0u8; 16];
        let mut truncated_key = [0u8; 16];
        truncated_iv.copy_from_slice(&super::sha2::compute_sha256(data_encryption_iv)[0..16]);
        truncated_key.copy_from_slice(&super::sha2::compute_sha256(data_encryption_key)[0..16]);
        let response_header_iv = truncated_iv;
        let response_header_key = truncated_key;

        let (read_length_shake_reader, write_length_shake_reader) = {
            let mut request_hasher = Shake128::default();
            request_hasher.update(data_encryption_iv);
            let request_reader = request_hasher.finalize_xof();

            let mut response_hasher = Shake128::default();
            response_hasher.update(&response_header_iv);
            let response_reader = response_hasher.finalize_xof();

            (Some(response_reader), Some(request_reader))
        };

        let (encryption_method, unbound_keys) = match self.data_cipher {
            DataCipher::Aes128Gcm => {
                // key is 16 bytes
                (
                    3u8,
                    Some((
                        UnboundKey::new(&AES_128_GCM, &response_header_key).unwrap(),
                        UnboundKey::new(&AES_128_GCM, data_encryption_key).unwrap(),
                    )),
                )
            }
            DataCipher::ChaCha20Poly1305 | DataCipher::Any => {
                // default to chacha.
                // key is 32 bytes
                (
                    4u8,
                    Some((
                        UnboundKey::new(
                            &CHACHA20_POLY1305,
                            &create_chacha_key(&response_header_key),
                        )
                        .unwrap(),
                        UnboundKey::new(
                            &CHACHA20_POLY1305,
                            &create_chacha_key(data_encryption_key),
                        )
                        .unwrap(),
                    )),
                )
            }
            DataCipher::None => (5u8, None),
        };

        let data_keys = if let Some((unbound_opening_key, unbound_sealing_key)) = unbound_keys {
            let opening_key = OpeningKey::new(
                unbound_opening_key,
                VmessNonceSequence::new(&response_header_iv),
            );
            let sealing_key = SealingKey::new(
                unbound_sealing_key,
                VmessNonceSequence::new(data_encryption_iv),
            );
            Some((opening_key, sealing_key))
        } else {
            None
        };

        // continue filling out other parts of instructions_to_addr_type.

        // set options, standard format data stream and metadata obfuscation
        header_bytes[34] = 0x01 | 0x04;

        // only 4 bits, generate this now before our first await.
        let margin_len: u8 = rand::random::<u8>() & 0xf;
        header_bytes[35] = (margin_len << 4) | encryption_method;

        // specify tcp protocol
        header_bytes[37] = 1;

        let (remote_address, remote_port) = remote_location.into_location().unwrap_components();

        header_bytes[38] = (remote_port >> 8) as u8;
        header_bytes[39] = (remote_port & 0xff) as u8;

        let mut cursor = match remote_address {
            Address::Ipv4(v4addr) => {
                header_bytes[40] = 1;
                header_bytes[41..45].copy_from_slice(&v4addr.octets());
                45
            }
            Address::Ipv6(v6addr) => {
                header_bytes[40] = 3;
                header_bytes[41..57].copy_from_slice(&v6addr.octets());
                57
            }
            Address::Hostname(hostname) => {
                if hostname.len() > 255 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Hostname is too long: {hostname}"),
                    ));
                }
                header_bytes[40] = 2;
                header_bytes[41] = hostname.len() as u8;
                header_bytes[42..42 + hostname.len()].copy_from_slice(hostname.as_bytes());
                42 + hostname.len()
            }
        };

        if margin_len > 0 {
            rand::rng().fill_bytes(&mut header_bytes[cursor..cursor + margin_len as usize]);
            cursor += margin_len as usize;
        }

        let mut fnv_hasher = Fnv1aHasher::new();
        fnv_hasher.write(&header_bytes[0..cursor]);
        let check_bytes = fnv_hasher.finish().to_be_bytes();
        header_bytes[cursor..cursor + 4].copy_from_slice(&check_bytes);
        cursor += 4;

        // AEAD mode only
        let mut encrypted_payload_length = [0u8; 18];
        let mut nonce = [0u8; 8];
        rand::rng().fill_bytes(&mut nonce);

        let header_length_aead_key = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Key_Length", &cert_hash, &nonce],
        );

        let header_length_nonce = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Nonce_Length", &cert_hash, &nonce],
        );

        let unbound_key = UnboundKey::new(&AES_128_GCM, &header_length_aead_key[0..16]).unwrap();

        let mut sealing_key = SealingKey::new(
            unbound_key,
            SingleUseNonce::new(&header_length_nonce[0..12]),
        );

        encrypted_payload_length[0] = (cursor >> 8) as u8;
        encrypted_payload_length[1] = (cursor & 0xff) as u8;

        let tag = sealing_key
            .seal_in_place_separate_tag(Aad::from(&cert_hash), &mut encrypted_payload_length[0..2])
            .unwrap();

        encrypted_payload_length[2..].copy_from_slice(tag.as_ref());

        let header_aead_key = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Key", &cert_hash, &nonce],
        );

        let header_nonce = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Nonce", &cert_hash, &nonce],
        );

        // TODO: don't unwrap
        let unbound_key = UnboundKey::new(&AES_128_GCM, &header_aead_key[0..16]).unwrap();
        let mut sealing_key =
            SealingKey::new(unbound_key, SingleUseNonce::new(&header_nonce[0..12]));
        let tag = sealing_key
            .seal_in_place_separate_tag(Aad::from(&cert_hash), &mut header_bytes[0..cursor])
            .unwrap();

        header_bytes[cursor..cursor + TAG_LEN].copy_from_slice(tag.as_ref());
        cursor += TAG_LEN;

        // Build complete AEAD request in a single buffer to avoid multiple writes as some
        // servers expect to read the entire header in one shot
        let total_len = 16 + 18 + 8 + cursor;
        let mut complete_request = Vec::with_capacity(total_len);
        complete_request.extend_from_slice(&cert_hash);
        complete_request.extend_from_slice(&encrypted_payload_length);
        complete_request.extend_from_slice(&nonce);
        complete_request.extend_from_slice(&header_bytes[0..cursor]);

        write_all(&mut client_stream, &complete_request).await?;

        // Flush the entire request.
        client_stream.flush().await?;

        // Info for reading the server response, which arrives along with the initial data.
        // Always AEAD mode
        let read_header_info = ReadHeaderInfo {
            response_header_key,
            response_header_iv,
            response_authentication_v,
        };

        let client_stream = Box::new(VmessStream::new(
            client_stream,
            false,
            data_keys,
            read_length_shake_reader,
            write_length_shake_reader,
            false,
            None,
            Some(read_header_info),
        ));

        Ok(TcpClientSetupResult {
            client_stream,
            early_data: None,
        })
    }

    fn supports_udp_over_tcp(&self) -> bool {
        self.udp_enabled // VMess supports UDP-over-TCP tunneling when enabled
    }

    async fn setup_client_udp_bidirectional(
        &self,
        client_stream: Box<dyn AsyncStream>,
        target: ResolvedLocation,
    ) -> std::io::Result<Box<dyn AsyncMessageStream>> {
        // VMess single-target UDP mode: Send VMess header with COMMAND_UDP (2)
        // and destination address. Uses VmessStream with is_udp=true.
        self.setup_udp_stream_impl(client_stream, target.into_location())
            .await
    }
}

impl VmessTcpClientHandler {
    /// Setup UDP stream for VMess single-target bidirectional UDP mode.
    /// Sends VMess header with COMMAND_UDP (2) and the destination address.
    async fn setup_udp_stream_impl(
        &self,
        mut client_stream: Box<dyn AsyncStream>,
        remote_location: NetLocation,
    ) -> std::io::Result<Box<dyn AsyncMessageStream>> {
        // VMess single-target UDP mode: Send VMess header with COMMAND_UDP (2).
        // Same as TCP setup but with command=2 and is_udp=true for VmessStream.

        // AEAD allows 120 second delta from the current time.
        let random_delta: u64 = rand::rng().random_range(0..241);
        let time_secs: u64 =
            SystemTime::UNIX_EPOCH.elapsed().unwrap().as_secs() - 120u64 + random_delta;

        let mut aead_bytes = [0u8; 16];
        let time_bytes = time_secs.to_be_bytes();
        aead_bytes[0..8].copy_from_slice(&time_bytes);

        rand::rng().fill_bytes(&mut aead_bytes[8..12]);

        let checksum_value = super::crc32::crc32c(&aead_bytes[0..12]);
        let checksum = checksum_value.to_be_bytes();
        aead_bytes[12..16].copy_from_slice(&checksum);

        self.aead_encrypting_key
            .less_safe_encrypt(&mut aead_bytes, EncryptionContext::None)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "AEAD auth ID encryption failed",
                )
            })?;

        let cert_hash = aead_bytes;

        // max length of encrypted header (same as TCP):
        // 41 (instructions up to addr type) + 256 (max domain name length) +
        // 15 (max margin length) + 4 (fnv1a hash) = 316 + TAG_LEN
        let mut header_bytes = [0u8; 316 + TAG_LEN];

        header_bytes[0] = 1; // version

        // Fill encryption keys and response auth:
        // - data encryption iv (16 bytes)
        // - data encryption key (16 bytes)
        // - response authentication v (1 byte)
        rand::rng().fill_bytes(&mut header_bytes[1..34]);

        let data_encryption_iv: &[u8] = &header_bytes[1..17];
        let data_encryption_key: &[u8] = &header_bytes[17..33];
        let response_authentication_v = header_bytes[33];

        // AEAD mode only - use SHA256 for response header keys
        let mut truncated_iv = [0u8; 16];
        let mut truncated_key = [0u8; 16];
        truncated_iv.copy_from_slice(&super::sha2::compute_sha256(data_encryption_iv)[0..16]);
        truncated_key.copy_from_slice(&super::sha2::compute_sha256(data_encryption_key)[0..16]);
        let response_header_iv = truncated_iv;
        let response_header_key = truncated_key;

        let (read_length_shake_reader, write_length_shake_reader) = {
            let mut request_hasher = Shake128::default();
            request_hasher.update(data_encryption_iv);
            let request_reader = request_hasher.finalize_xof();

            let mut response_hasher = Shake128::default();
            response_hasher.update(&response_header_iv);
            let response_reader = response_hasher.finalize_xof();

            (Some(response_reader), Some(request_reader))
        };

        let (encryption_method, unbound_keys) = match self.data_cipher {
            DataCipher::Aes128Gcm => (
                3u8,
                Some((
                    UnboundKey::new(&AES_128_GCM, &response_header_key).unwrap(),
                    UnboundKey::new(&AES_128_GCM, data_encryption_key).unwrap(),
                )),
            ),
            DataCipher::ChaCha20Poly1305 | DataCipher::Any => (
                4u8,
                Some((
                    UnboundKey::new(&CHACHA20_POLY1305, &create_chacha_key(&response_header_key))
                        .unwrap(),
                    UnboundKey::new(&CHACHA20_POLY1305, &create_chacha_key(data_encryption_key))
                        .unwrap(),
                )),
            ),
            DataCipher::None => (5u8, None),
        };

        let data_keys = if let Some((unbound_opening_key, unbound_sealing_key)) = unbound_keys {
            let opening_key = OpeningKey::new(
                unbound_opening_key,
                VmessNonceSequence::new(&response_header_iv),
            );
            let sealing_key = SealingKey::new(
                unbound_sealing_key,
                VmessNonceSequence::new(data_encryption_iv),
            );
            Some((opening_key, sealing_key))
        } else {
            None
        };

        // Set options: standard format data stream and metadata obfuscation
        header_bytes[34] = 0x01 | 0x04;

        // Margin length (4 bits) + encryption method
        let margin_len: u8 = rand::random::<u8>() & 0xf;
        header_bytes[35] = (margin_len << 4) | encryption_method;

        // Reserved byte
        header_bytes[36] = 0;

        // Command = UDP (2) instead of TCP (1)
        header_bytes[37] = COMMAND_UDP;

        let (remote_address, remote_port) = remote_location.unwrap_components();

        header_bytes[38] = (remote_port >> 8) as u8;
        header_bytes[39] = (remote_port & 0xff) as u8;

        let mut cursor = match remote_address {
            Address::Ipv4(v4addr) => {
                header_bytes[40] = 1;
                header_bytes[41..45].copy_from_slice(&v4addr.octets());
                45
            }
            Address::Ipv6(v6addr) => {
                header_bytes[40] = 3;
                header_bytes[41..57].copy_from_slice(&v6addr.octets());
                57
            }
            Address::Hostname(hostname) => {
                if hostname.len() > 255 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Hostname is too long: {hostname}"),
                    ));
                }
                header_bytes[40] = 2;
                header_bytes[41] = hostname.len() as u8;
                header_bytes[42..42 + hostname.len()].copy_from_slice(hostname.as_bytes());
                42 + hostname.len()
            }
        };

        if margin_len > 0 {
            rand::rng().fill_bytes(&mut header_bytes[cursor..cursor + margin_len as usize]);
            cursor += margin_len as usize;
        }

        let mut fnv_hasher = Fnv1aHasher::new();
        fnv_hasher.write(&header_bytes[0..cursor]);
        let check_bytes = fnv_hasher.finish().to_be_bytes();
        header_bytes[cursor..cursor + 4].copy_from_slice(&check_bytes);
        cursor += 4;

        // AEAD encryption of header
        let mut encrypted_payload_length = [0u8; 18];
        let mut nonce = [0u8; 8];
        rand::rng().fill_bytes(&mut nonce);

        let header_length_aead_key = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Key_Length", &cert_hash, &nonce],
        );

        let header_length_nonce = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Nonce_Length", &cert_hash, &nonce],
        );

        let unbound_key = UnboundKey::new(&AES_128_GCM, &header_length_aead_key[0..16]).unwrap();

        let mut sealing_key = SealingKey::new(
            unbound_key,
            SingleUseNonce::new(&header_length_nonce[0..12]),
        );

        encrypted_payload_length[0] = (cursor >> 8) as u8;
        encrypted_payload_length[1] = (cursor & 0xff) as u8;

        let tag = sealing_key
            .seal_in_place_separate_tag(Aad::from(&cert_hash), &mut encrypted_payload_length[0..2])
            .unwrap();

        encrypted_payload_length[2..].copy_from_slice(tag.as_ref());

        let header_aead_key = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Key", &cert_hash, &nonce],
        );

        let header_nonce = super::sha2::kdf(
            &self.instruction_key,
            &[b"VMess Header AEAD Nonce", &cert_hash, &nonce],
        );

        let unbound_key = UnboundKey::new(&AES_128_GCM, &header_aead_key[0..16]).unwrap();
        let mut sealing_key =
            SealingKey::new(unbound_key, SingleUseNonce::new(&header_nonce[0..12]));
        let tag = sealing_key
            .seal_in_place_separate_tag(Aad::from(&cert_hash), &mut header_bytes[0..cursor])
            .unwrap();

        header_bytes[cursor..cursor + TAG_LEN].copy_from_slice(tag.as_ref());
        cursor += TAG_LEN;

        // Build complete AEAD request in a single buffer
        let total_len = 16 + 18 + 8 + cursor;
        let mut complete_request = Vec::with_capacity(total_len);
        complete_request.extend_from_slice(&cert_hash);
        complete_request.extend_from_slice(&encrypted_payload_length);
        complete_request.extend_from_slice(&nonce);
        complete_request.extend_from_slice(&header_bytes[0..cursor]);

        write_all(&mut client_stream, &complete_request).await?;
        client_stream.flush().await?;

        // Info for reading the server response
        let read_header_info = ReadHeaderInfo {
            response_header_key,
            response_header_iv,
            response_authentication_v,
        };

        // Create VmessStream with is_udp=true for bidirectional UDP
        let vmess_stream = VmessStream::new(
            client_stream,
            true, // is_udp = true for bidirectional UDP mode
            data_keys,
            read_length_shake_reader,
            write_length_shake_reader,
            false,
            None,
            Some(read_header_info),
        );

        Ok(Box::new(vmess_stream))
    }
}
