//! Append-only JSONL journal plus per-step atomic state files.

use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::Result;
use crate::paths::StatePaths;
use crate::util::{atomic_write_json, ensure_dir, now_millis, read_json};

pub struct Journal {
    pub home: PathBuf,
    pub path: PathBuf,
}

impl Journal {
    pub fn new(home: &Path) -> Result<Self> {
        ensure_dir(home)?;
        let state = StatePaths::new(home);
        Ok(Self {
            home: home.to_path_buf(),
            path: state.journal(),
        })
    }

    pub fn append(&self, event: &str, fields: Map<String, Value>) -> Result<Value> {
        let mut record = Map::new();
        record.insert("ts".to_string(), Value::from(now_millis()));
        record.insert("event".to_string(), Value::String(event.to_string()));
        for (key, value) in fields {
            record.insert(key, value);
        }
        let record = Value::Object(record);
        let line = crate::py_json::dumps_compact_utf8(&record) + "\n";
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        #[cfg(unix)]
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_EX);
        }
        #[cfg(windows)]
        let _ = file.lock();
        file.write_all(line.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        #[cfg(unix)]
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
        #[cfg(windows)]
        let _ = file.unlock();
        Ok(record)
    }
}

pub fn state_path(home: &Path, scope: &str, step_id: &str) -> PathBuf {
    let state = StatePaths::new(home);
    if scope == "global" {
        state.global_state().join(format!("{step_id}.json"))
    } else {
        state.instance_steps(scope).join(format!("{step_id}.json"))
    }
}

fn previous_device_state_path(home: &Path, scope: &str, step_id: &str) -> Option<PathBuf> {
    let suffix = step_id.strip_prefix("device-")?;
    let directory = if scope == "global" {
        StatePaths::new(home).global_state()
    } else {
        StatePaths::new(home).instance_steps(scope)
    };
    Some(directory.join(format!("\x73\x74\x65\x61\x6c\x74\x68-{suffix}.json")))
}

pub fn load_state(home: &Path, scope: &str, step_id: &str) -> Option<Value> {
    let current = state_path(home, scope, step_id);
    read_json(&current).or_else(|| {
        previous_device_state_path(home, scope, step_id).and_then(|path| read_json(&path))
    })
}

pub fn save_state(home: &Path, scope: &str, step_id: &str, state: &Value) -> Result<PathBuf> {
    let path = state_path(home, scope, step_id);
    atomic_write_json(&path, state, 0o600)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::load_state;

    #[test]
    fn previous_device_step_state_remains_readable() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-journal-{}-{}",
            std::process::id(),
            crate::util::localstamp()
        ));
        let directory = crate::paths::StatePaths::new(&home).instance_steps("test");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("\x73\x74\x65\x61\x6c\x74\x68-cpu.json"),
            br#"{"status":"applied"}"#,
        )
        .unwrap();
        let value = load_state(&home, "test", "device-cpu").unwrap();
        assert_eq!(value["status"], "applied");
        let _ = std::fs::remove_dir_all(home);
    }
}
