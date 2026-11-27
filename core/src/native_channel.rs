//! Experimental, byte-preserving host/guest stream protocol for virtio-serial.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const START: u8 = 1;
pub const INPUT: u8 = 2;
pub const EOF: u8 = 3;
pub const RESIZE: u8 = 4;
pub const CLOSE: u8 = 5;
pub const READY: u8 = 6;
pub const OUT: u8 = 7;
pub const ERR: u8 = 8;
pub const EXIT: u8 = 9;
pub const FAILURE: u8 = 10;
pub const CLOSED: u8 = 11;
/// Stop a running job. The guest sends EXIT followed by CLOSED after cleanup.
pub const CANCEL: u8 = 12;
pub const VERSION: u8 = 1;
pub const MAX: usize = 64 * 1024;

pub const ALLOWED_FORWARDED_ENV: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "COLORFGBG",
    "NO_COLOR",
    "FORCE_COLOR",
];

const MAX_ENV_VALUE_LEN: usize = 256;

/// Only harmless terminal-presentation variables may cross from host to guest.
pub fn is_safe_forwarded_env(key: &str, value: &str) -> bool {
    ALLOWED_FORWARDED_ENV.contains(&key)
        && value.len() <= MAX_ENV_VALUE_LEN
        && !value.bytes().any(|b| b == 0)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u8,
    pub job: Job,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Job {
    Agent {
        name: String,
        argv: Vec<String>,
        cwd: String,
        tty: Option<Size>,
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    Shell {
        argv: Vec<String>,
        cwd: String,
        tty: Option<Size>,
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    Exec {
        program: String,
        argv: Vec<String>,
        cwd: String,
        tty: Option<Size>,
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    Sync {
        operation: String,
        root: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub term: String,
    #[serde(default)]
    pub pixel_width: u16,
    #[serde(default)]
    pub pixel_height: u16,
    #[serde(default)]
    pub color_term: Option<String>,
}

pub fn frame(out: &mut impl Write, kind: u8, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native frame is too large",
        ));
    }
    let mut header = [0; 5];
    header[0] = kind;
    header[1..].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.write_all(&header)?;
    out.write_all(bytes)
}

pub fn read_frame(input: &mut impl Read, body: &mut Vec<u8>) -> io::Result<u8> {
    let mut header = [0; 5];
    input.read_exact(&mut header)?;
    let [kind, a, b, c, d] = header;
    let len = u32::from_be_bytes([a, b, c, d]) as usize;
    if len > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native frame is too large",
        ));
    }
    body.resize(len, 0);
    input.read_exact(body)?;
    Ok(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_preserve_binary_and_reject_oversize() {
        let mut buf = vec![];
        frame(&mut buf, OUT, &[0, 0xff, b'\n']).unwrap();
        frame(&mut buf, EXIT, &[17]).unwrap();
        let mut input = buf.as_slice();
        let mut body = Vec::new();
        assert_eq!(read_frame(&mut input, &mut body).unwrap(), OUT);
        assert_eq!(body, [0, 0xff, b'\n']);
        assert_eq!(read_frame(&mut input, &mut body).unwrap(), EXIT);
        assert_eq!(body, [17]);
        assert!(frame(&mut buf, OUT, &vec![0; MAX + 1]).is_err());
        assert!(read_frame(&mut &[OUT, 0xff, 0xff, 0xff, 0xff][..], &mut body).is_err());
    }

    #[test]
    fn safe_forwarded_env_enforces_allowlist_and_bounds() {
        assert!(is_safe_forwarded_env("TERM_PROGRAM", "Ghostty"));
        assert!(is_safe_forwarded_env("NO_COLOR", "1"));
        assert!(is_safe_forwarded_env("FORCE_COLOR", "1"));
        assert!(!is_safe_forwarded_env("PATH", "/tmp"));
        assert!(!is_safe_forwarded_env("LD_PRELOAD", "/tmp/x.so"));
        assert!(!is_safe_forwarded_env("TERM_PROGRAM", "bad\0val"));
        assert!(!is_safe_forwarded_env(
            "TERM_PROGRAM",
            &"x".repeat(MAX_ENV_VALUE_LEN + 1)
        ));
    }

    #[test]
    fn agent_job_deserializes_without_env_field() {
        let legacy = r#"{"version":1,"job":{"Agent":{"name":"claude","argv":["--version"],"cwd":"~/p","tty":null}}}"#;
        let parsed: Request = serde_json::from_str(legacy).unwrap();
        match parsed.job {
            Job::Agent { name, env, .. } => {
                assert_eq!(name, "claude");
                assert!(env.is_empty());
            }
            _ => panic!("expected Agent job"),
        }
    }
}
