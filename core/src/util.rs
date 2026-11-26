use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// The only clock reading for persisted and wire timestamps: Unix milliseconds.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Normalize an externally supplied Unix timestamp to milliseconds. Some APIs
/// (notably Codex quota) report seconds; anything below 1e11 is seconds, which
/// keeps both forms accepted while wire and stored times stay milliseconds.
pub fn normalize_unix_millis(value: i64) -> i64 {
    const MILLIS_THRESHOLD: i64 = 100_000_000_000;
    if value != 0 && value.abs() < MILLIS_THRESHOLD {
        value.saturating_mul(1000)
    } else {
        value
    }
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Stable, non-secret identity for settings that belong to one transaction.
pub fn sha256_text(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Hex SHA-256 of a file's bytes, streamed.
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Terminal output as a reader sees it: escape sequences (colours, cursor
/// moves, titles) removed, and a carriage return overwriting its line the way
/// a progress bar does, so only each line's final state remains.
pub fn clean_terminal_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\u{8}' => {
                plain.pop();
            }
            c if c.is_control() && c != '\n' && c != '\r' && c != '\t' => {}
            c => plain.push(c),
        }
    }
    if !plain.contains('\r') {
        return plain;
    }
    let mut final_lines = String::with_capacity(plain.len());
    for (index, line) in plain.split('\n').enumerate() {
        if index != 0 {
            final_lines.push('\n');
        }
        if let Some(text) = line.rsplit('\r').find(|part| !part.is_empty()) {
            final_lines.push_str(text);
        }
    }
    final_lines
}

/// Validate a user-facing name (workspace, session, subscription).
pub fn clean_name(raw: &str, what: &str) -> crate::Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(crate::Error::invalid(format!("Enter a {what} name")));
    }
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(crate::Error::invalid(format!(
            "The {what} name must be at most 80 printable characters"
        )));
    }
    Ok(name.to_string())
}

/// Incremental UTF-8 decoder: keeps an incomplete trailing sequence for the
/// next chunk so PTY output never splits a character.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let valid_up_to = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) => match error.error_len() {
                // Invalid bytes in the middle: decode lossily and reset.
                Some(_) => {
                    let text = String::from_utf8_lossy(&self.pending).into_owned();
                    self.pending.clear();
                    return text;
                }
                None => error.valid_up_to(),
            },
        };
        let rest = self.pending.split_off(valid_up_to);

        String::from_utf8(std::mem::replace(&mut self.pending, rest)).unwrap_or_default()
    }
}

/// Locks that survive a panic elsewhere. A thread that panicked while
/// holding a lock leaves it poisoned; the data inside is still the last value
/// written, and taking every later caller down with it helps no one.
pub trait LockExt<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// [`LockExt`] for readers–writer locks.
pub trait RwLockExt<T> {
    fn read_lock(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for std::sync::RwLock<T> {
    fn read_lock(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::clean_terminal_text;

    #[test]
    fn terminal_text_drops_escapes_and_keeps_each_lines_last_state() {
        assert_eq!(clean_terminal_text("\u{1b}[32mok\u{1b}[0m done"), "ok done");
        assert_eq!(
            clean_terminal_text("10%\r50%\r100%\nnext\r\n"),
            "100%\nnext\n"
        );
        assert_eq!(clean_terminal_text("\u{1b}]0;title\u{7}abc\u{8}d"), "abd");
        assert_eq!(clean_terminal_text("plain\ttext"), "plain\ttext");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_stream_joins_split_characters() {
        let mut stream = Utf8Stream::default();
        let bytes = "héllo".as_bytes();
        assert_eq!(stream.push(&bytes[..2]), "h");
        assert_eq!(stream.push(&bytes[2..]), "éllo");
    }

    #[test]
    fn names_are_validated() {
        assert!(clean_name("  ", "workspace").is_err());
        assert_eq!(clean_name(" api ", "workspace").unwrap(), "api");
    }

    #[test]
    fn external_timestamps_normalize_to_milliseconds() {
        assert_eq!(normalize_unix_millis(1_900_000_000), 1_900_000_000_000);
        assert_eq!(normalize_unix_millis(1_900_000_000_000), 1_900_000_000_000);
        assert_eq!(normalize_unix_millis(0), 0);
    }
}
